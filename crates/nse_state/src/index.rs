//! Strike-pair dependency index -- Phase 4 of the roadmap. Maps each token
//! to the box-spread strike pairs it's a leg of, so a single quote update
//! can dirty exactly those pairs instead of the scheduler rescanning the
//! whole book on every tick.

use nse_pricing::BoxPairId;
use nse_refdata::InstrumentMaster;
use std::collections::{HashMap, HashSet};

/// The four leg tokens making up one box-spread strike pair: call and put
/// at the lower strike (`k1`), call and put at the upper strike (`k2`).
#[derive(Clone, Copy, Debug)]
pub struct PairLegs {
    pub call_k1: i32,
    pub call_k2: i32,
    pub put_k1: i32,
    pub put_k2: i32,
}

pub struct StrikePairDependencyIndex {
    token_to_pairs: HashMap<i32, Vec<BoxPairId>>,
    pair_legs: HashMap<BoxPairId, PairLegs>,
}

impl StrikePairDependencyIndex {
    /// Groups `instruments`' allowed tokens by expiry, keeps only strikes
    /// that have both a CE and a PE token at that expiry (the strikes a box
    /// spread could actually use as a leg), then forms every unordered pair
    /// of those strikes within the same expiry. Every leg token of a pair is
    /// recorded against that pair's `BoxPairId` in both directions --
    /// `token_to_pairs` for dirtying on a quote update, `pair_legs` for
    /// resolving a dirtied pair back to its four tokens.
    pub fn build(instruments: &InstrumentMaster) -> Self {
        Self::build_with(instruments, |_, _| true)
    }

    /// Same as `build`, but only forms a pair when `(k1, k2)` (raw ×100
    /// strikes, `k1 < k2`) is in `allowed_pairs` -- everything else is
    /// skipped entirely, not just hidden from a query: it never enters
    /// `token_to_pairs`/`pair_legs`, so the scheduler can never dirty or
    /// price it. For narrowing the universe down to a specific handful of
    /// pairs (e.g. manual verification against the exchange screen)
    /// without touching the general-purpose combinatorial `build`.
    pub fn build_restricted(instruments: &InstrumentMaster, allowed_pairs: &HashSet<(i64, i64)>) -> Self {
        Self::build_with(instruments, |k1, k2| allowed_pairs.contains(&(k1, k2)))
    }

    /// Same as `build`, but `keep_pair(k1, k2)` (raw ×100 strikes, `k1 <
    /// k2`) decides whether each combinatorial pair is formed at all -- the
    /// general form `build`/`build_restricted` are both thin wrappers
    /// around. A pair `keep_pair` rejects never enters `token_to_pairs`/
    /// `pair_legs`, so the scheduler can never dirty or price it -- this is
    /// "don't calculate it," not "calculate it and hide it."
    pub fn build_with(instruments: &InstrumentMaster, keep_pair: impl Fn(i64, i64) -> bool) -> Self {
        // expiry -> strike -> (ce_token, pe_token), built up before pairing
        // so a strike missing either side is dropped up front.
        type StrikeSides = HashMap<i64, (Option<i32>, Option<i32>)>;
        let mut by_expiry: HashMap<i64, StrikeSides> = HashMap::new();

        for &token in instruments.allowed_tokens() {
            let Some(info) = instruments.get(token) else {
                continue;
            };
            if info.strike_raw == 0 {
                continue; // no strike -- not an option leg (e.g. a future)
            }

            let entry = by_expiry
                .entry(info.expiry_epoch)
                .or_default()
                .entry(info.strike_raw)
                .or_insert((None, None));

            match info.option_type.as_str() {
                "CE" => entry.0 = Some(token),
                "PE" => entry.1 = Some(token),
                _ => {}
            }
        }

        let mut token_to_pairs: HashMap<i32, Vec<BoxPairId>> = HashMap::new();
        let mut pair_legs: HashMap<BoxPairId, PairLegs> = HashMap::new();

        for (&expiry_id, strikes) in &by_expiry {
            let mut valid: Vec<(i64, i32, i32)> = strikes
                .iter()
                .filter_map(|(&strike, &(ce, pe))| match (ce, pe) {
                    (Some(c), Some(p)) => Some((strike, c, p)),
                    _ => None,
                })
                .collect();
            valid.sort_by_key(|&(strike, _, _)| strike);

            for i in 0..valid.len() {
                for j in (i + 1)..valid.len() {
                    let (k1, call_k1, put_k1) = valid[i];
                    let (k2, call_k2, put_k2) = valid[j];
                    if !keep_pair(k1, k2) {
                        continue;
                    }
                    let pid = BoxPairId::new(expiry_id, k1, k2);
                    let legs = PairLegs {
                        call_k1,
                        call_k2,
                        put_k1,
                        put_k2,
                    };

                    for token in [call_k1, call_k2, put_k1, put_k2] {
                        token_to_pairs.entry(token).or_default().push(pid);
                    }
                    pair_legs.insert(pid, legs);
                }
            }
        }

        Self {
            token_to_pairs,
            pair_legs,
        }
    }

    pub fn pairs_for(&self, token: i32) -> &[BoxPairId] {
        self.token_to_pairs
            .get(&token)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn legs_of(&self, pid: BoxPairId) -> Option<PairLegs> {
        self.pair_legs.get(&pid).copied()
    }

    pub fn pair_count(&self) -> usize {
        self.pair_legs.len()
    }
}

/// Hot-swappable on contract roll -- `arc_swap::ArcSwap` is the Rust
/// analogue of an atomic `shared_ptr<const T>` swap: build the new index
/// off-thread, swap the pointer, old readers finish against the old `Arc`
/// until it drops. Zero locking on the read path.
pub type SharedIndex = arc_swap::ArcSwap<StrikePairDependencyIndex>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// The §08 worked example: one NIFTY expiry, four adjacent strikes
    /// (23900/24000/24100/24200), each with both a CE and a PE token.
    fn write_worked_example_csv() -> std::path::PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nse_index_test_{id}.csv"));
        let mut file = fs::File::create(&path).unwrap();
        let rows = [
            "C,OPTIDX,1,OPTIDX,NIFTY,1450000000,2390000,CE",
            "C,OPTIDX,2,OPTIDX,NIFTY,1450000000,2390000,PE",
            "C,OPTIDX,3,OPTIDX,NIFTY,1450000000,2400000,CE",
            "C,OPTIDX,4,OPTIDX,NIFTY,1450000000,2400000,PE",
            "C,OPTIDX,5,OPTIDX,NIFTY,1450000000,2410000,CE",
            "C,OPTIDX,6,OPTIDX,NIFTY,1450000000,2410000,PE",
            "C,OPTIDX,7,OPTIDX,NIFTY,1450000000,2420000,CE",
            "C,OPTIDX,8,OPTIDX,NIFTY,1450000000,2420000,PE",
        ];
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        path
    }

    #[test]
    fn a_quote_update_dirties_exactly_the_pairs_that_use_it_as_a_leg() {
        let path = write_worked_example_csv();
        let instruments = InstrumentMaster::load(&path, &["NIFTY"], &["OPTIDX"]);
        fs::remove_file(&path).ok();

        let index = StrikePairDependencyIndex::build(&instruments);

        // token 3 = 24000 CE -- matches the §08 diagram: dirties exactly
        // three pairs, one with each of the other three strikes.
        let mut pairs = index.pairs_for(3).to_vec();
        pairs.sort_by_key(|p| (p.k1, p.k2));
        assert_eq!(
            pairs,
            vec![
                BoxPairId::new(1450000000, 2390000, 2400000),
                BoxPairId::new(1450000000, 2400000, 2410000),
                BoxPairId::new(1450000000, 2400000, 2420000),
            ]
        );

        // every other strike is untouched -- only the dirtied set changes.
        let pairs_for_token_1 = index.pairs_for(1); // 23900 CE
        assert!(pairs_for_token_1.contains(&BoxPairId::new(1450000000, 2390000, 2400000)));
        assert!(!pairs_for_token_1.contains(&BoxPairId::new(1450000000, 2400000, 2410000)));
    }

    #[test]
    fn four_strikes_yield_six_combinatorial_pairs() {
        let path = write_worked_example_csv();
        let instruments = InstrumentMaster::load(&path, &["NIFTY"], &["OPTIDX"]);
        fs::remove_file(&path).ok();

        let index = StrikePairDependencyIndex::build(&instruments);
        assert_eq!(index.pair_count(), 6); // C(4, 2) = 6
    }

    #[test]
    fn build_restricted_only_forms_the_allowed_pairs() {
        let path = write_worked_example_csv();
        let instruments = InstrumentMaster::load(&path, &["NIFTY"], &["OPTIDX"]);
        fs::remove_file(&path).ok();

        let allowed: HashSet<(i64, i64)> = [(2_390_000, 2_400_000), (2_400_000, 2_410_000)].into_iter().collect();
        let index = StrikePairDependencyIndex::build_restricted(&instruments, &allowed);

        assert_eq!(index.pair_count(), 2);
        assert!(index.legs_of(BoxPairId::new(1450000000, 2390000, 2400000)).is_some());
        assert!(index.legs_of(BoxPairId::new(1450000000, 2400000, 2410000)).is_some());
        // (23900, 24100) is a valid combinatorial pair but not in `allowed`.
        assert!(index.legs_of(BoxPairId::new(1450000000, 2390000, 2410000)).is_none());
        // token 7 (24200 CE) isn't a leg of either allowed pair.
        assert!(index.pairs_for(7).is_empty());
    }

    #[test]
    fn legs_of_resolves_a_pair_back_to_its_four_tokens() {
        let path = write_worked_example_csv();
        let instruments = InstrumentMaster::load(&path, &["NIFTY"], &["OPTIDX"]);
        fs::remove_file(&path).ok();

        let index = StrikePairDependencyIndex::build(&instruments);
        let pid = BoxPairId::new(1450000000, 2390000, 2400000);
        let legs = index.legs_of(pid).expect("pair should exist");

        assert_eq!(legs.call_k1, 1); // 23900 CE
        assert_eq!(legs.put_k1, 2); // 23900 PE
        assert_eq!(legs.call_k2, 3); // 24000 CE
        assert_eq!(legs.put_k2, 4); // 24000 PE
    }

    #[test]
    fn a_strike_missing_one_side_is_excluded_from_pairing() {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nse_index_test_{id}.csv"));
        let mut file = fs::File::create(&path).unwrap();
        // 24000 has only a CE, no PE -- can't be a box leg, must be excluded.
        writeln!(file, "C,OPTIDX,1,OPTIDX,NIFTY,1450000000,2390000,CE").unwrap();
        writeln!(file, "C,OPTIDX,2,OPTIDX,NIFTY,1450000000,2390000,PE").unwrap();
        writeln!(file, "C,OPTIDX,3,OPTIDX,NIFTY,1450000000,2400000,CE").unwrap();
        drop(file);

        let instruments = InstrumentMaster::load(&path, &["NIFTY"], &["OPTIDX"]);
        fs::remove_file(&path).ok();

        let index = StrikePairDependencyIndex::build(&instruments);
        assert_eq!(index.pair_count(), 0);
        assert!(index.pairs_for(3).is_empty());
    }
}
