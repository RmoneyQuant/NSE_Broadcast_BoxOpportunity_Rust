//! Live-market state, the strike-pair dependency index, and the
//! incremental recompute scheduler -- Phases 3, 4, and 6 of the Box Spread
//! Pipeline roadmap, kept in one crate per the target architecture's
//! `crates/nse_state/` layout (`store.rs` + `index.rs` + `scheduler.rs`).

mod index;
mod scheduler;
mod store;

pub use index::{PairLegs, SharedIndex, StrikePairDependencyIndex};
pub use scheduler::{PricingSession, SchedulerEvent};
pub use store::{LiveMarketStateStore, Session};
