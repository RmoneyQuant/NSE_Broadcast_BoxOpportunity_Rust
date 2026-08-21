//! Native desktop window -- replaces both Phase 7's browser dashboard and
//! the egui/wgpu attempt that followed it. egui needed a GPU-accelerated
//! rendering path (Vulkan, DX12, or OpenGL 3.3+); confirmed via
//! `RUST_LOG=info` that none is reachable in this deployment's session at
//! all -- not even DX12's WARP software adapter enumerates, consistent
//! with an RDP/VDI session without GPU passthrough. Win32 common controls
//! (this module, via `native-windows-gui`) render through GDI, which is
//! exactly what RDP was built to virtualize, so this has no GPU dependency
//! at all. The pricing pipeline runs on a background thread and only ever
//! touches `SharedGuiState` through its `Mutex`; this module (running on
//! the main thread, as `nwg::dispatch_thread_events` requires) never
//! touches the decoder/pricing session directly.

use native_windows_gui as nwg;
use nse_pricing::{DropEvent, OpportunityRecord};
use nse_sink::{Side, WireOpportunityRecord, WirePairId};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct GuiState {
    pub long: HashMap<WirePairId, WireOpportunityRecord>,
    pub short: HashMap<WirePairId, WireOpportunityRecord>,
    pub benchmark_rate: f64,
    pub packet_count: usize,
    pub message_count: usize,
    /// Display label of the expiry actually in effect right now -- set by
    /// the background thread once it finishes rebuilding the pipeline,
    /// empty until the first "Apply Filter".
    pub active_expiry: String,
    pub pair_count: usize,
    /// Live, real: nearest strike (50-rupee steps) to the current NIFTY
    /// future's LTP -- the only real "current level" this feed can produce
    /// (no index/spot broadcast exists). `None` until the future's first
    /// tick arrives. Not user-editable anywhere -- K1 Min/K2 Max
    /// auto-fill from it once, but stay ordinary editable numbers after that.
    pub current_atm: Option<f64>,
}

pub type SharedGuiState = Arc<Mutex<GuiState>>;

/// One expiry, plus either specific strikes or a K1 Min/K2 Max range --
/// exactly one of the two selects which strikes are in play. `main.rs`'s
/// `build_pipeline` is the only thing that interprets this.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterSpec {
    /// `Some(epoch)` for one specific expiry; `None` means "every expiry"
    /// -- the GUI's Expiry dropdown's "All Expiries" entry, index 0.
    pub expiry_epoch: Option<i64>,
    /// Raw ×100 strikes, manually picked in the GUI's strike list --
    /// `Some` overrides `k1_min`/`k2_max` entirely; `None` means "use the
    /// bounds instead." Always `None` when `expiry_epoch` is `None`: the
    /// strike list only ever shows one expiry's strikes at a time, so
    /// manual per-strike selection doesn't make sense across "every expiry."
    pub selected_strikes: Option<Vec<i64>>,
    pub k1_min: f64,
    pub k2_max: f64,
}

/// Set by the GUI thread when the operator clicks "Apply Filter", read
/// (and cleared) by the background pricing thread the next time it checks
/// -- at most once per incoming packet, so on `live` mode this is
/// effectively applied within a tick or two; on `replay` mode, within
/// 200ms (the fixture's per-line pacing).
pub type SharedFilterRequest = Arc<Mutex<Option<FilterSpec>>>;

impl GuiState {
    pub fn emit_opportunity(&mut self, record: &OpportunityRecord, long_qualifies: bool, short_qualifies: bool) {
        if long_qualifies && let Some(wire) = WireOpportunityRecord::from_record(record, Side::Long) {
            self.long.insert(wire.pair_id, wire);
        }
        if short_qualifies && let Some(wire) = WireOpportunityRecord::from_record(record, Side::Short) {
            self.short.insert(wire.pair_id, wire);
        }
    }

    pub fn emit_drop(&mut self, drop: DropEvent) {
        let pair_id = WirePairId { expiry_id: drop.expiry, k1: drop.strike_1 / 100, k2: drop.strike_2 / 100 };
        match Side::from(drop.side) {
            Side::Long => {
                self.long.remove(&pair_id);
            }
            Side::Short => {
                self.short.remove(&pair_id);
            }
        }
    }
}

// "MV" is the strike gap (K2 - K1), rupees. "Gross" is the raw pre-tax box
// price (nse_pricing::box_pricer, unchanged from box_pricer.py); "Net" is
// the real cash deployment (long table) /
// cash generation (short table) figure -- Gross with the buy/sell round-trip
// tax multipliers applied once to each leg's own price (nse_pricing::cost).
// The "(Lot)" columns multiply that per-share figure by Lot Size x Quantity
// (read live from the GUI's own input boxes -- a display-only scaling, not
// something the pricing pipeline itself knows or needs to rebuild for).
// "K1 Bid"/"K1 Ask"/"K2 Bid"/"K2 Ask" are the four raw leg prices the
// formula actually used for that side (see nse_pricing::LegPrices) -- which
// instrument (call or put) plays "the K1 bid" differs between the long and
// short tables, since they trade opposite legs at each strike. "Rate" is
// the un-annualized return over the trade's life; "Ann. rate" scales that
// by 365/DTE.
const COLUMNS: [(&str, i32); 17] = [
    ("Expiry", 95),
    ("DTE", 45),
    ("K1", 60),
    ("K2", 60),
    ("MV", 55),
    ("K1 Bid", 65),
    ("K1 Ask", 65),
    ("K2 Bid", 65),
    ("K2 Ask", 65),
    ("Gross", 70),
    ("Net", 70),
    ("Net (Lot)", 90),
    ("Profit", 70),
    ("Profit (Lot)", 95),
    ("Rate", 65),
    ("Ann. rate", 75),
    ("Src", 55),
];

/// Sorted by highest Net (`spread_tax`) first -- the real, tax-adjusted
/// cash deployed (long table) / generated (short table) for that row, per
/// explicit request. No separate sort-by UI control is offered here the
/// way the egui version had one. `min_rate`/`max_rate`, if set, drop any
/// row whose Ann. rate isn't strictly above/below them (both can be set at
/// once for a band) -- display-only (the pricing pipeline still computed
/// every row; this just hides the ones outside the range).
fn sorted_rows(rows: &HashMap<WirePairId, WireOpportunityRecord>, min_rate: Option<f64>, max_rate: Option<f64>) -> Vec<&WireOpportunityRecord> {
    let mut sorted: Vec<&WireOpportunityRecord> = rows
        .values()
        .filter(|r| min_rate.is_none_or(|min| r.annualized_interest_rate > min))
        .filter(|r| max_rate.is_none_or(|max| r.annualized_interest_rate < max))
        .collect();
    sorted.sort_by(|a, b| b.spread_tax.partial_cmp(&a.spread_tax).unwrap_or(std::cmp::Ordering::Equal));
    sorted
}

fn row_strings(r: &WireOpportunityRecord, lot_multiplier: f64) -> [String; 17] {
    [
        r.expiry.map(|d| d.format("%d-%b-%Y").to_string()).unwrap_or_default(),
        r.days_to_expiry.to_string(),
        r.pair_id.k1.to_string(),
        r.pair_id.k2.to_string(),
        (r.pair_id.k2 - r.pair_id.k1).to_string(),
        format!("{:.2}", r.k1_bid),
        format!("{:.2}", r.k1_ask),
        format!("{:.2}", r.k2_bid),
        format!("{:.2}", r.k2_ask),
        format!("{:.2}", r.spread),
        format!("{:.2}", r.spread_tax),
        format!("{:.2}", r.spread_tax * lot_multiplier),
        format!("{:.2}", r.profit),
        format!("{:.2}", r.profit * lot_multiplier),
        format!("{:.2}%", r.interest_rate),
        format!("{:.2}%", r.annualized_interest_rate),
        r.source.clone(),
    ]
}

fn build_list_view(parent: &nwg::Window, x: i32, y: i32, w: i32, h: i32) -> nwg::ListView {
    let mut list = nwg::ListView::default();
    nwg::ListView::builder()
        .parent(parent)
        .position((x, y))
        .size((w, h))
        .list_style(nwg::ListViewStyle::Detailed)
        .ex_flags(nwg::ListViewExFlags::GRID | nwg::ListViewExFlags::FULL_ROW_SELECT)
        .build(&mut list)
        .expect("failed to create list view");

    for (i, (name, width)) in COLUMNS.iter().enumerate() {
        list.insert_column(nwg::InsertListViewColumn { index: Some(i as i32), fmt: None, width: Some(*width), text: Some((*name).to_string()) });
    }
    list.set_headers_enabled(true);
    list
}

/// Populates `strikes_listbox` with every strike available at `epoch`, and
/// keeps `current_strikes` in sync (index-aligned with the listbox) so
/// Apply Filter can map selected indices back to real strikes. Shared
/// between the Expiry dropdown's selection handler and the auto-select
/// that follows "Load Expiries", so both stay in sync the same way.
fn populate_strikes_for_expiry(strikes_listbox: &nwg::ListBox<String>, current_strikes: &RefCell<Vec<i64>>, strikes_by_expiry: &HashMap<i64, Vec<i64>>, epoch: i64) {
    let strikes = strikes_by_expiry.get(&epoch).cloned().unwrap_or_default();
    let display: Vec<String> = strikes.iter().map(|s| format!("{:.2}", *s as f64 / 100.0)).collect();
    strikes_listbox.set_collection(display);
    *current_strikes.borrow_mut() = strikes;
}

fn refresh_list(list: &nwg::ListView, rows: &HashMap<WirePairId, WireOpportunityRecord>, lot_multiplier: f64, min_rate: Option<f64>, max_rate: Option<f64>) {
    list.clear();
    for r in sorted_rows(rows, min_rate, max_rate) {
        let cells = row_strings(r, lot_multiplier);
        let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
        list.insert_items_row(None, &refs);
    }
}

const TABLE_MARGIN: i32 = 10;
const TABLE_GAP: i32 = 10;
const TABLE_Y: i32 = 220;
const HEADING_Y: i32 = 195;
const RATE_FILTER_Y: i32 = 170;
const MIN_TABLE_W: i32 = 200;
const MIN_TABLE_H: i32 = 150;

/// Resizes/repositions the two tables, their headings, and the
/// short-side rate filter (Cash Gen. Max Rate %, whose left edge tracks
/// the short table's) to fill whatever the window's current client area
/// is -- the only part of the layout that reflows on resize. The
/// long-side rate filter and the rest of the filter controls above stay
/// put; their fixed positions/sizes never having needed to change, since
/// the long table's left edge is a fixed margin, not resize-dependent.
#[allow(clippy::too_many_arguments)]
fn layout_tables(window_w: i32, window_h: i32, long_heading: &nwg::Label, short_heading: &nwg::Label, long_list: &nwg::ListView, short_list: &nwg::ListView, maxrate_label: &nwg::Label, maxrate_input: &nwg::TextInput) {
    let table_w = ((window_w - TABLE_MARGIN * 2 - TABLE_GAP) / 2).max(MIN_TABLE_W);
    let table_h = (window_h - TABLE_Y - TABLE_MARGIN).max(MIN_TABLE_H);
    let right_x = TABLE_MARGIN + table_w + TABLE_GAP;

    maxrate_label.set_position(right_x, RATE_FILTER_Y);
    maxrate_input.set_position(right_x + 160, RATE_FILTER_Y - 2);

    long_heading.set_size(table_w as u32, 20);
    short_heading.set_position(right_x, HEADING_Y);
    short_heading.set_size(table_w as u32, 20);

    long_list.set_size(table_w as u32, table_h as u32);
    short_list.set_position(right_x, TABLE_Y);
    short_list.set_size(table_w as u32, table_h as u32);
}

/// Blocks the calling thread pumping the window's message loop -- call
/// this last, from `main()`, after the pricing pipeline is already running
/// on its own background thread. `expiries` (sorted, epoch + display date)
/// and `strikes_by_expiry` (epoch -> sorted strikes with both a CE and PE)
/// are the full, unrestricted catalog computed once at startup -- the
/// pickers here never need to touch the contract file themselves.
pub fn run(state: SharedGuiState, filter_request: SharedFilterRequest, expiries: Vec<(i64, String)>, strikes_by_expiry: HashMap<i64, Vec<i64>>) -> Result<(), nwg::NwgError> {
    nwg::init()?;
    let _ = nwg::Font::set_global_family("Segoe UI");

    let mut window = Default::default();
    nwg::Window::builder().size((1900, 900)).position((50, 50)).title("Box Spread Desk").build(&mut window)?;

    let mut status_label = Default::default();
    nwg::Label::builder().text("starting...").position((10, 10)).size((1880, 20)).parent(&window).build(&mut status_label)?;

    // Row 2: Index (informational -- this pipeline is NIFTY-only) / Load
    // Expiries / Expiry picker.
    let mut index_label = Default::default();
    nwg::Label::builder().text("Index").position((10, 37)).size((35, 22)).parent(&window).build(&mut index_label)?;
    let mut index_combo = Default::default();
    nwg::ComboBox::builder().collection(vec!["NIFTY 50".to_string()]).selected_index(Some(0)).position((48, 35)).size((110, 200)).parent(&window).build(&mut index_combo)?;

    let mut load_expiries_button = Default::default();
    nwg::Button::builder().text("Load Expiries").position((166, 35)).size((110, 22)).parent(&window).build(&mut load_expiries_button)?;

    let mut expiry_label = Default::default();
    nwg::Label::builder().text("Expiry").position((286, 37)).size((40, 22)).parent(&window).build(&mut expiry_label)?;
    let mut expiry_combo = Default::default();
    nwg::ComboBox::<String>::builder().position((328, 35)).size((110, 200)).parent(&window).build(&mut expiry_combo)?;

    // Row 3: strike multi-select on the left, K1 Min/K2 Max + Lot
    // Size/Quantity + Apply on the right. Selecting one or more strikes
    // here overrides K1 Min/K2 Max entirely; selecting none falls back to
    // the bounds -- see FilterSpec's doc.
    let mut strikes_label = Default::default();
    nwg::Label::builder().text("Strikes (ctrl/shift-click; none = use bounds)").position((10, 60)).size((300, 20)).parent(&window).build(&mut strikes_label)?;
    let mut strikes_listbox = Default::default();
    nwg::ListBox::<String>::builder().flags(nwg::ListBoxFlags::VISIBLE | nwg::ListBoxFlags::MULTI_SELECT).position((10, 80)).size((300, 100)).parent(&window).build(&mut strikes_listbox)?;

    let mut k1min_label = Default::default();
    nwg::Label::builder().text("K1 Min").position((325, 80)).size((55, 22)).parent(&window).build(&mut k1min_label)?;
    let mut k1min_input = Default::default();
    nwg::TextInput::builder().position((383, 78)).size((75, 22)).parent(&window).build(&mut k1min_input)?;

    let mut k2max_label = Default::default();
    nwg::Label::builder().text("K2 Max").position((325, 105)).size((55, 22)).parent(&window).build(&mut k2max_label)?;
    let mut k2max_input = Default::default();
    nwg::TextInput::builder().position((383, 103)).size((75, 22)).parent(&window).build(&mut k2max_input)?;

    let mut lotsize_label = Default::default();
    nwg::Label::builder().text("Lot Size").position((475, 80)).size((55, 22)).parent(&window).build(&mut lotsize_label)?;
    let mut lotsize_input = Default::default();
    nwg::TextInput::builder().text("65").position((533, 78)).size((70, 22)).parent(&window).build(&mut lotsize_input)?;

    let mut qty_label = Default::default();
    nwg::Label::builder().text("Quantity").position((475, 105)).size((55, 22)).parent(&window).build(&mut qty_label)?;
    let mut qty_input = Default::default();
    nwg::TextInput::builder().text("1").position((533, 103)).size((70, 22)).parent(&window).build(&mut qty_input)?;

    let mut apply_button = Default::default();
    nwg::Button::builder().text("Apply Filter").position((620, 88)).size((120, 26)).parent(&window).build(&mut apply_button)?;

    // Each rate filter sits directly above its own table, not crammed into
    // the shared control row above -- display-only, filtering which
    // already-computed rows are shown (by Ann. rate), read live every
    // refresh same as Lot Size/Quantity. No pipeline rebuild needed, unlike
    // Apply Filter above: this never changes which pairs are priced, only
    // which priced rows are listed. Long/deployment: higher rate is the
    // attractive side, so this is a floor ("show only above").
    // Short/generation: the rate is your implied borrowing cost, so lower
    // is attractive -- a ceiling ("show only below") instead, not the same
    // direction as the long filter. The short-side filter's x position
    // tracks the short table's (both move together on resize -- see
    // layout_tables); the long-side one never needs to, its table's left
    // edge is a fixed margin.
    let mut minrate_label = Default::default();
    nwg::Label::builder().text("Cash Dep. Min Rate %").position((10, 170)).size((150, 22)).parent(&window).build(&mut minrate_label)?;
    let mut minrate_input = Default::default();
    nwg::TextInput::builder().position((165, 168)).size((60, 22)).parent(&window).build(&mut minrate_input)?;

    let mut maxrate_label = Default::default();
    nwg::Label::builder().text("Cash Gen. Max Rate %").position((950, 170)).size((155, 22)).parent(&window).build(&mut maxrate_label)?;
    let mut maxrate_input = Default::default();
    nwg::TextInput::builder().position((1110, 168)).size((60, 22)).parent(&window).build(&mut maxrate_input)?;

    let mut long_heading = Default::default();
    nwg::Label::builder().text("Long box \u{2014} cash deployment").position((10, 195)).size((930, 20)).parent(&window).build(&mut long_heading)?;

    let mut short_heading = Default::default();
    nwg::Label::builder().text("Short box \u{2014} cash generation").position((950, 195)).size((930, 20)).parent(&window).build(&mut short_heading)?;

    let long_list = build_list_view(&window, 10, 220, 930, 670);
    let short_list = build_list_view(&window, 950, 220, 930, 670);

    // AnimationTimer, not the plain Timer -- the latter is deprecated
    // precisely because its callback isn't guaranteed to fire on the owning
    // thread, which would be unsound for touching Win32 controls here.
    let mut timer = Default::default();
    nwg::AnimationTimer::builder().parent(&window).interval(std::time::Duration::from_millis(300)).active(true).build(&mut timer)?;

    // Raw strikes backing whatever's currently in `strikes_listbox`,
    // index-aligned -- refreshed every time the expiry selection changes,
    // read back when Apply Filter maps selected indices to real strikes.
    let current_strikes: RefCell<Vec<i64>> = RefCell::new(Vec::new());

    // Lot Size/Quantity/Min Rate/Max Rate used to be read straight off
    // their TextInputs on every 300ms refresh tick -- meaning the table
    // refiltered mid-keystroke, before the operator had finished typing a
    // number. These now stage the last value Apply Filter actually
    // committed; OnTimerTick reads only these, never the boxes directly,
    // so nothing changes until Apply Filter is clicked -- consistent with
    // how Expiry/Strikes/K1 Min/K2 Max already only take effect on Apply.
    let active_lot_multiplier: Cell<f64> = Cell::new(65.0);
    let active_min_rate: Cell<Option<f64>> = Cell::new(None);
    let active_max_rate: Cell<Option<f64>> = Cell::new(None);

    let window = Rc::new(window);
    let events_window = window.clone();

    let handler = nwg::full_bind_event_handler(&window.handle, move |evt, _evt_data, handle| {
        use nwg::Event as E;
        match evt {
            E::OnWindowClose if &handle == &events_window as &nwg::Window => {
                nwg::stop_thread_dispatch();
            }
            // WM_SIZE splits into three different nwg events depending on
            // *how* the size changed: SIZE_MAXIMIZED becomes
            // OnWindowMaximize, SIZE_MINIMIZED becomes OnWindowMinimize,
            // and everything else (manual drag-resize, and restoring from
            // maximized back to normal) becomes OnResize. Only listening
            // for OnResize meant clicking Maximize never reflowed anything
            // -- both have to be handled the same way here.
            E::OnResize | E::OnWindowMaximize if &handle == &events_window as &nwg::Window => {
                let (w, h) = events_window.size();
                layout_tables(w as i32, h as i32, &long_heading, &short_heading, &long_list, &short_list, &maxrate_label, &maxrate_input);
            }
            E::OnButtonClick if handle == load_expiries_button => {
                // Index 0 is always "All Expiries" (FilterSpec::expiry_epoch
                // = None); every real expiry follows at index i+1, so
                // combo-index-to-expiries-index is a constant -1 offset
                // everywhere below.
                let mut display: Vec<String> = vec!["All Expiries".to_string()];
                display.extend(expiries.iter().map(|(_, label)| label.clone()));
                expiry_combo.set_collection(display);
                // Auto-select the nearest single expiry (index 1, not "All
                // Expiries" at index 0 -- a much larger universe isn't the
                // safer default) so the form is already valid to Apply right
                // after this one click, per "show data after apply filters"
                // -- the operator can still pick a different one, including "All".
                if !expiries.is_empty() {
                    expiry_combo.set_selection(Some(1));
                    populate_strikes_for_expiry(&strikes_listbox, &current_strikes, &strikes_by_expiry, expiries[0].0);
                }
            }
            E::OnComboxBoxSelection if handle == expiry_combo => {
                match expiry_combo.selection() {
                    Some(0) => {
                        // "All Expiries" -- no single strike list applies
                        // across every expiry, so manual per-strike
                        // selection is unavailable; clearing forces
                        // K1 Min/K2 Max bounds mode (selected_strikes ends
                        // up None either way, same as leaving nothing
                        // selected in the list).
                        strikes_listbox.set_collection(Vec::new());
                        current_strikes.borrow_mut().clear();
                    }
                    Some(idx) => populate_strikes_for_expiry(&strikes_listbox, &current_strikes, &strikes_by_expiry, expiries[idx - 1].0),
                    None => {}
                }
            }
            E::OnButtonClick if handle == apply_button => {
                let Some(expiry_idx) = expiry_combo.selection() else {
                    nwg::modal_error_message(events_window.handle, "No expiry selected", "Click Load Expiries, then pick one from the Expiry dropdown first.");
                    return;
                };
                // Index 0 is "All Expiries" (None); every real expiry is at
                // index i+1 -- see the Expiry dropdown's population above.
                let expiry_epoch = if expiry_idx == 0 { None } else { Some(expiries[expiry_idx - 1].0) };

                let selected_indices = strikes_listbox.multi_selection();
                let selected_strikes = if selected_indices.is_empty() {
                    None
                } else {
                    let strikes = current_strikes.borrow();
                    Some(selected_indices.iter().map(|&i| strikes[i]).collect::<Vec<i64>>())
                };

                let (k1_min, k2_max) = if selected_strikes.is_some() {
                    (0.0, 0.0) // unused -- selected_strikes overrides bounds entirely
                } else {
                    let parse = |input: &nwg::TextInput, field: &str| -> Option<f64> {
                        match input.text().trim().parse::<f64>() {
                            Ok(v) => Some(v),
                            Err(e) => {
                                nwg::modal_error_message(events_window.handle, "Invalid bounds", &format!("{field} is not a valid number: {e}"));
                                None
                            }
                        }
                    };
                    let Some(k1) = parse(&k1min_input, "K1 Min") else {
                        return;
                    };
                    let Some(k2) = parse(&k2max_input, "K2 Max") else {
                        return;
                    };
                    if k2 <= k1 {
                        nwg::modal_error_message(events_window.handle, "Invalid bounds", "K2 Max must be greater than K1 Min.");
                        return;
                    }
                    (k1, k2)
                };

                *filter_request.lock().unwrap() = Some(FilterSpec { expiry_epoch, selected_strikes, k1_min, k2_max });

                // Display-only knobs stage here too, atomically with
                // everything else -- see active_lot_multiplier's doc.
                let lot_size: f64 = lotsize_input.text().trim().parse().unwrap_or(1.0);
                let qty: f64 = qty_input.text().trim().parse().unwrap_or(1.0);
                active_lot_multiplier.set(lot_size * qty);
                active_min_rate.set(minrate_input.text().trim().parse().ok());
                active_max_rate.set(maxrate_input.text().trim().parse().ok());

                status_label.set_text("Applying filter...");
            }
            E::OnTimerTick if handle == timer => {
                let lot_multiplier = active_lot_multiplier.get();
                let min_rate = active_min_rate.get();
                let max_rate = active_max_rate.get();

                let s = state.lock().unwrap();

                // ATM is live and not user-typed anywhere -- the moment
                // it's available, seed K1 Min/K2 Max with it once. Checking
                // "still empty" (rather than a one-shot flag) means this
                // only ever fills a box the operator hasn't touched yet;
                // it never overwrites something they've typed or that
                // Apply Filter already populated.
                if let Some(atm) = s.current_atm {
                    if k1min_input.text().trim().is_empty() {
                        k1min_input.set_text(&format!("{:.0}", atm - 1500.0));
                    }
                    if k2max_input.text().trim().is_empty() {
                        k2max_input.set_text(&format!("{:.0}", atm + 1500.0));
                    }
                }

                status_label.set_text(&format!(
                    "Benchmark: {:.2}%   |   {} packets, {} messages   |   {} long, {} short opportunities   |   ATM: {}   |   active: {} ({} pair(s))",
                    s.benchmark_rate,
                    s.packet_count,
                    s.message_count,
                    s.long.len(),
                    s.short.len(),
                    s.current_atm.map(|a| format!("{a:.0}")).unwrap_or_else(|| "waiting for future tick...".to_string()),
                    if s.active_expiry.is_empty() { "none applied yet" } else { &s.active_expiry },
                    s.pair_count
                ));
                // Long/deployment uses only the floor (min_rate); short/generation
                // uses only the ceiling (max_rate) -- see the controls' doc comment
                // for why the two tables filter in opposite directions.
                refresh_list(&long_list, &s.long, lot_multiplier, min_rate, None);
                refresh_list(&short_list, &s.short, lot_multiplier, None, max_rate);
            }
            _ => {}
        }
    });

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    Ok(())
}
