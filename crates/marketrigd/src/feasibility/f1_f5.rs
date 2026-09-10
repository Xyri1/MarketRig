//! F1 — can CN matching pause without disabling US/HK?
//! F5 — can missing data suspend cached execution coherently?
//! (`sdd/features/a-share-engine/FEASIBILITY.md`; feature SPEC §2.1, §5.1, §5.3.)
//!
//! Every test runs the real `LiveNode` on the controlled clock from
//! [`crate::feasibility::clock`] with **no feed** (`feed_base: None`): every
//! quote, and every instrument status, is published by hand at a chosen instant.
//! That is the only way to separate "time passed" from "data arrived", and F2
//! already established that the sandbox has no session concept of its own.
//!
//! Findings, in the order the tests establish them:
//!
//! 1. [`withheld_data_still_fills_a_new_order_from_the_cached_book`] — F1 (ii)
//!    and F5 (b). Withholding quotes does **not** stop execution: the matching
//!    engine keeps its last L1 book, so a LIMIT or MARKET order submitted hours
//!    after the last tick fills against it immediately, at an out-of-session
//!    instant. Marking the installation-wide [`crate::feed::MarketState`]
//!    `DEGRADED` changes nothing — that state and the node's book are unrelated.
//!    A daemon-side gate before `crate::trade::place` is mandatory.
//! 2. [`pause_gates_cn_matching_and_submission`] — F1 (i), (ii), (iii), (v).
//!    The smallest supported gate is an `InstrumentStatus` published on the same
//!    data path as a quote: `MarketStatusAction::Pause` puts that one
//!    instrument's `OrderMatchingEngine` into `MarketStatus::Paused`, which
//!    stops `iterate` from matching at all
//!    (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:3702`) and makes
//!    `process_order` answer the sandbox's own `OrderRejected` (`mod.rs:2728`).
//!    It is per instrument, so a US order rests and fills throughout.
//!    `MarketStatusAction::Trading` reopens it — but `process_status` does not
//!    call `iterate`, so a resting order does not fill on the resume itself; the
//!    first quote after it does.
//! 3. [`pause_gates_a_resting_sell`] — F1 (iv): the same on the sell side.
//! 4. [`only_the_instruments_own_quote_matches_the_cached_book`] — with an
//!    `Open` engine, a crossing cached book and a resting order, neither the
//!    sandbox expiry-sweep timer, nor the portfolio equity-curve timer, nor a
//!    quote for another instrument on the same venue and the same sandbox client
//!    matches it. Only that instrument's own quote does. Withholding CN quotes
//!    therefore does freeze *resting* CN orders; it does nothing about new ones.
//! 5. [`recovery_matches_only_the_new_book`] — F5 (c): a quote delivered while
//!    paused still *updates* the book (`mod.rs:1595`, the update precedes the
//!    `iterate` that is gated), so the pause leaves a crossing book behind. The
//!    first post-recovery quote replaces it, and a non-crossing one produces no
//!    fill: matching is against the new book alone.
//! 6. [`a_stale_stamped_quote_matches_the_old_book`] — the provider-transition
//!    defect. `process_quote_tick` compares `quote.ts_event` against
//!    `book.ts_last` and, when the new quote is older, **skips the book update
//!    but still calls `iterate(quote.ts_init)`** (`mod.rs:1584-1592`). Yahoo
//!    stamps `ts_event` with `meta.regularMarketTime` and HiThink with receipt
//!    (`crate::node::cn_cycle`, `crate::node::poll_once`), so switching
//!    `a_share_feed` from HiThink to Yahoo can deliver an older `ts_event` and
//!    match the *old provider's* book at the new provider's arrival instant.
//! 7. [`a_zero_size_book_stops_matching_and_a_quote_restores_it`] — the
//!    "clear stale executable state" candidate. A `QuoteTick` with both sizes
//!    zero leaves a resting order alive and unmatched, and a new MARKET order
//!    gets the sandbox's own `No market for 600519.XSHG` rejection — but a new
//!    crossing LIMIT is still *accepted* and rests. The next real quote matches
//!    every resting order. It suspends matching and refuses MARKET orders; it
//!    is not a submission gate.
//! 8. [`market_state_leads_the_node_book`] — F5's shared/local ordering. Both
//!    `crate::node::poll_once` and `crate::node::cn_cycle` call
//!    `MarketState::accept*` **before** `sender.send(DataEvent::Data(..))`, and
//!    the send only queues work for the node thread. A validation that reads
//!    `MarketState` can therefore see a newer price than the book the sandbox
//!    will match the same order against.

use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::messages::DataEvent;
use nautilus_core::UnixNanos;
use nautilus_model::data::{Data, InstrumentStatus, QuoteTick};
use nautilus_model::enums::{MarketStatusAction, OrderStatus};
use nautilus_model::identifiers::{ClientOrderId, InstrumentId};
use nautilus_model::orders::Order;
use nautilus_model::types::{Price, Quantity};
use rust_decimal::Decimal;

use crate::catalog::Entry;
use crate::feasibility::clock::{
    CN_0935, ClockHandle, DAY_NS, SECOND_NS, advance, controlled_registry, publish_book,
    publish_quote, stored_events,
};
use crate::feed::{ChartQuote, Health};
use crate::node::{Node, Registry, within};
use crate::store::Store;
use crate::trade::{self, TradeError};

/// 11:30:00 Asia/Shanghai — the morning close of feature SPEC §5.1.
const CN_1130: u64 = CN_0935 + 6_900 * SECOND_NS;
/// 11:45:00 — inside the lunch break.
const CN_1145: u64 = CN_0935 + 7_800 * SECOND_NS;
/// 13:00:00 — the afternoon open.
const CN_1300: u64 = CN_0935 + 12_300 * SECOND_NS;
/// 13:05:00 — inside the afternoon session.
const CN_1305: u64 = CN_0935 + 12_600 * SECOND_NS;

fn moutai() -> &'static Entry {
    crate::catalog::find("600519.XSHG").expect("the CN catalog entry")
}

fn apple() -> &'static Entry {
    crate::catalog::find("AAPL.XNAS").expect("the US catalog entry")
}

/// A started desk on the controlled clock at 09:35 Asia/Shanghai, no feed, with
/// one standing quote at `price` so the CN venue has a book.
fn desk_at_0935(
    store: &Store,
    name: &'static str,
    price: &'static str,
) -> (Registry, ClockHandle, std::sync::Arc<Node>) {
    let (registry, handle) = controlled_registry(store, None, name, CN_0935);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    publish_quote(&node, moutai(), price, 100, CN_0935);
    await_quote(&node, moutai(), price);
    (registry, handle, node)
}

/// Publishes one `InstrumentStatus` through the same data-event sender a quote
/// takes, and waits for the node to have processed it.
///
/// The `DataEngine` caches it and republishes it to `data.status.{venue}.{sym}`
/// (`nautilus-data-0.62.0/src/engine/mod.rs:2728`), which is exactly the pattern
/// the sandbox's status handler subscribes
/// (`nautilus-sandbox-0.62.0/src/execution.rs:582`, `:630`). Publication is
/// synchronous inside that handler, so the cached status proves the matching
/// engine saw it.
fn publish_status(node: &Node, entry: &'static Entry, action: MarketStatusAction, ts_ns: u64) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    node.call(move |_| {
        let status = InstrumentStatus::new(
            instrument_id,
            action,
            UnixNanos::from(ts_ns),
            UnixNanos::from(ts_ns),
            None,
            None,
            None,
            None,
            None,
        );
        get_data_event_sender()
            .send(DataEvent::Data(Data::InstrumentStatus(status)))
            .expect("the data runner takes the status");
    })
    .expect("the node answers");
    within(10, "the instrument status reaches the node", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .instrument_status(&instrument_id)
                .map(|s| s.action)
                == Some(action)
        })
        .unwrap()
    });
}

/// A quote whose `ts_event` and `ts_init` differ — what a provider transition
/// produces, and what [`crate::feasibility::clock::publish_quote`] cannot make.
fn publish_quote_stamped(
    node: &Node,
    entry: &'static Entry,
    price: &'static str,
    size: u32,
    ts_event_ns: u64,
    ts_init_ns: u64,
) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let price = Price::from(price);
    let size = Quantity::from(size);
    node.call(move |_| {
        let tick = QuoteTick::new(
            instrument_id,
            price,
            price,
            size,
            size,
            UnixNanos::from(ts_event_ns),
            UnixNanos::from(ts_init_ns),
        );
        get_data_event_sender()
            .send(DataEvent::Data(Data::Quote(tick)))
            .expect("the data runner takes the quote");
    })
    .expect("the node answers");
}

/// Waits for the node's cache to carry `price` on both sides of `entry`.
fn await_quote(node: &Node, entry: &'static Entry, price: &'static str) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let price = Price::from(price);
    within(10, "the published quote reaches the cache", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .quote(&instrument_id)
                .map(|q| q.ask_price)
                == Some(price)
        })
        .unwrap()
    });
}

fn status(node: &Node, client_order_id: &'static str) -> OrderStatus {
    let id = ClientOrderId::from(client_order_id);
    node.call(move |context| {
        context
            .cache
            .borrow()
            .order(&id)
            .map(|order| order.status())
            .expect("the order is cached")
    })
    .expect("the node answers")
}

/// The desk's captured chain for one order: `(kind, ts_event)` oldest first.
fn chain(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<(String, i64)> {
    stored_events(store, desk_id)
        .into_iter()
        .filter(|(id, _, _)| id == client_order_id)
        .map(|(_, kind, ns)| (kind, ns))
        .collect()
}

fn kinds(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<String> {
    chain(store, desk_id, client_order_id)
        .into_iter()
        .map(|(kind, _)| kind)
        .collect()
}

/// One production submit through `crate::trade::submit`.
#[expect(
    clippy::too_many_arguments,
    reason = "one call shape for every scenario"
)]
fn order(
    store: &Store,
    registry: &Registry,
    desk_id: &str,
    action_id: &str,
    instrument_id: &str,
    side: &str,
    order_type: &str,
    quantity: u32,
    price: Option<&str>,
) -> Result<serde_json::Value, TradeError> {
    let price = price.map_or("null".to_owned(), |p| format!("\"{p}\""));
    let body = format!(
        r#"{{"action_id":"{action_id}","instrument_id":"{instrument_id}",
            "side":"{side}","type":"{order_type}","quantity":"{quantity}","price":{price}}}"#
    );
    trade::submit(store, registry, desk_id, &body, &trade::Source::Session)
        .map(|(record, _)| record.outcome.expect("an ungated submit lands an outcome"))
}

fn fills(store: &Store) -> i64 {
    store
        .call(|conn| conn.query_row("SELECT count(*) FROM fills", [], |r| r.get(0)))
        .expect("the fills count reads")
}

// ---------------------------------------------------------------------------
// F1 (ii) + F5 (a), (b): withholding data is not a gate
// ---------------------------------------------------------------------------

/// The sandbox's L1 book is a *cache*, not a stream: it survives any amount of
/// silence, and it is what a new order matches against. Neither withholding
/// quotes nor marking the installation-wide feed `DEGRADED` prevents a fill at
/// an out-of-session instant.
#[test]
fn withheld_data_still_fills_a_new_order_from_the_cached_book() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f1-cached", "1700.00");
    let desk = handle.desk_id().to_owned();

    // F5 (a): the readiness gate is closed — the feed failed, so the daemon
    // publishes nothing more and the shared state says so. (The observation the
    // published quote would have carried is accepted first, exactly as
    // `crate::node::poll_once` does, so `DEGRADED` has something to age.)
    registry.market().accept(
        moutai(),
        &ChartQuote {
            price: Decimal::new(170_000, 2),
            currency: "CNY".to_owned(),
            source_time_ns: CN_0935 as i64,
        },
        CN_0935 as i64,
    );
    registry.market().mark_degraded(moutai().instrument_id);
    assert_eq!(
        registry.market().read(moutai(), CN_1130 as i64).health,
        Health::Degraded,
        "the installation-wide observation is degraded"
    );

    // Hours later, still no tick, and the afternoon session has opened without
    // one. (F1 asked this at 11:45, out of session; §5.1's admission check now
    // refuses a CN order there, so the same question is asked at 13:05 — the
    // book under test is still the untouched 09:35 one.)
    advance(&node, CN_1305);

    // F1 (ii) / F5 (b): a crossing LIMIT fills against the two-hour-old book.
    let limit = order(
        &store,
        &registry,
        &desk,
        "f1-cached-1",
        "600519.XSHG",
        "BUY",
        "LIMIT",
        100,
        Some("1800.00"),
    )
    .expect("the sandbox takes it");
    assert_eq!(
        limit["status"], "FILLED",
        "a LIMIT submitted while data is withheld fills from the cached book: {limit}"
    );

    // And so does a MARKET order.
    let market = order(
        &store,
        &registry,
        &desk,
        "f1-cached-2",
        "600519.XSHG",
        "BUY",
        "MARKET",
        100,
        None,
    )
    .expect("the sandbox takes it");
    assert_eq!(
        market["status"], "FILLED",
        "and so does a MARKET order: {market}"
    );

    // Both fills are stamped with the clock's instant, hours after the book was
    // last updated at 09:35.
    for action in ["f1-cached-1", "f1-cached-2"] {
        let filled = chain(&store, &desk, action)
            .into_iter()
            .find(|(kind, _)| kind == "OrderFilled")
            .unwrap_or_else(|| panic!("{action} filled"));
        assert_eq!(
            filled.1, CN_1305 as i64,
            "{action} is stamped at the clock's instant: {filled:?}"
        );
    }
    assert_eq!(fills(&store), 2);
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// F1 (i), (ii), (iii), (v): the pause gate
// ---------------------------------------------------------------------------

/// The whole F1 sequence on one desk: a resting CN buy and a resting US buy,
/// the CN instrument paused at 11:30, crossing CN quotes at 11:30:00 and 11:45
/// that do not fill, a new CN order the sandbox itself refuses, a crossing US
/// quote that *does* fill, and the 13:00 resume where the resting CN order fills
/// exactly once against the first quote after the reopen.
#[test]
fn pause_gates_cn_matching_and_submission() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f1-pause", "1700.00");
    let desk = handle.desk_id().to_owned();

    // A resting CN buy under the book, and a resting US buy under its own.
    let rested = order(
        &store,
        &registry,
        &desk,
        "f1-pause-cn",
        "600519.XSHG",
        "BUY",
        "LIMIT",
        100,
        Some("1600.00"),
    )
    .expect("the resting CN buy is accepted");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    publish_quote(&node, apple(), "320.00", 100, CN_0935);
    await_quote(&node, apple(), "320.00");
    let us = order(
        &store,
        &registry,
        &desk,
        "f1-pause-us",
        "AAPL.XNAS",
        "BUY",
        "LIMIT",
        10,
        Some("300.00"),
    )
    .expect("the resting US buy is accepted");
    assert_eq!(us["status"], "ACCEPTED", "{us}");

    // 11:30:00 — the morning close. The gate is one `InstrumentStatus` for the
    // CN instrument; nothing else in the node is touched.
    advance(&node, CN_1130);
    publish_status(&node, moutai(), MarketStatusAction::Pause, CN_1130);

    // (i) A crossing quote stamped exactly at the boundary, and another at
    // 11:45: no fill either time. The quote still updates the book — only
    // matching is gated.
    publish_quote(&node, moutai(), "1500.00", 100, CN_1130);
    await_quote(&node, moutai(), "1500.00");
    assert_eq!(
        status(&node, "f1-pause-cn"),
        OrderStatus::Accepted,
        "the boundary quote did not fill the paused order"
    );
    advance(&node, CN_1145);
    publish_quote(&node, moutai(), "1450.00", 100, CN_1145);
    await_quote(&node, moutai(), "1450.00");
    assert_eq!(
        status(&node, "f1-pause-cn"),
        OrderStatus::Accepted,
        "a mid-lunch crossing quote did not fill it either"
    );
    assert_eq!(fills(&store), 0, "nothing filled while paused");

    // (ii) A new CN order while closed. F1 found that the paused *sandbox*
    // refuses it in its own words; production now refuses it one step earlier —
    // §5.1's session check runs inside admission, so the order never reaches the
    // engine at all (slice 014 step 3).
    let refused = order(
        &store,
        &registry,
        &desk,
        "f1-pause-shut",
        "600519.XSHG",
        "BUY",
        "LIMIT",
        100,
        Some("1800.00"),
    )
    .expect_err("the closed session refuses a new order");
    let TradeError::Invalid(what) = &refused else {
        panic!("expected MarketRig's own session refusal, got {refused:?}");
    };
    assert_eq!(
        what,
        "600519.XSHG is outside the supported session [09:30,11:30) and \
         [13:00,14:57) Asia/Shanghai"
    );
    assert!(
        kinds(&store, &desk, "f1-pause-shut").is_empty(),
        "and nothing was handed to the sandbox"
    );

    // (v) Venue independence: the US instrument was never paused, so its own
    // crossing quote fills its resting order during the CN closure.
    publish_quote(&node, apple(), "290.00", 100, CN_1145);
    within(10, "the US order fills", || {
        status(&node, "f1-pause-us") == OrderStatus::Filled
    });
    assert_eq!(
        status(&node, "f1-pause-cn"),
        OrderStatus::Accepted,
        "and the CN order is still resting"
    );

    // (iii) 13:00 — reopen. `process_status` does not call `iterate`, so the
    // resume alone fills nothing even though the cached book crosses.
    advance(&node, CN_1300);
    publish_status(&node, moutai(), MarketStatusAction::Trading, CN_1300);
    assert_eq!(
        status(&node, "f1-pause-cn"),
        OrderStatus::Accepted,
        "reopening does not itself match the cached crossing book"
    );

    // The first quote after the reopen does, exactly once, at 13:00.
    publish_quote(&node, moutai(), "1400.00", 100, CN_1300);
    within(10, "the resting CN order fills after the reopen", || {
        status(&node, "f1-pause-cn") == OrderStatus::Filled
    });
    let captured = kinds(&store, &desk, "f1-pause-cn");
    assert_eq!(
        captured,
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
        "exactly one fill, under the original client order id"
    );
    let filled = chain(&store, &desk, "f1-pause-cn")
        .into_iter()
        .find(|(kind, _)| kind == "OrderFilled")
        .unwrap();
    assert_eq!(
        filled.1, CN_1300 as i64,
        "and it is stamped at the reopen instant"
    );

    // The day close is the same mechanism with a different action:
    // `MarketStatusAction::Close` (or `Halt`) reaches `MarketStatus::Closed`
    // (`mod.rs:2307-2311`), which gates `iterate` and `process_order` exactly as
    // `Paused` does — so §5.1's lunch pause and its 14:57 close need no separate
    // machinery, only a different word in the refusal.
    let close = CN_1300 + 6_420 * SECOND_NS;
    advance(&node, close);
    publish_status(&node, moutai(), MarketStatusAction::Close, close);
    let shut = order(
        &store,
        &registry,
        &desk,
        "f1-pause-late",
        "600519.XSHG",
        "BUY",
        "LIMIT",
        100,
        Some("1800.00"),
    )
    .expect_err("the closed engine refuses a new order");
    let TradeError::Rejected(reason) = &shut else {
        panic!("expected a sandbox rejection, got {shut:?}");
    };
    assert_eq!(
        reason,
        "Market 600519.XSHG is CLOSED, cannot accept order f1-pause-late",
    );
    registry.stop_all();
}

/// F1 (iv): the same on the sell side. A cash account cannot short, so the desk
/// buys first, then rests a sell above the book.
#[test]
fn pause_gates_a_resting_sell() {
    let (_dir, store) = crate::store::open_temp();
    // The shares are bought on day D−1: §1.1's T+1 lock now refuses a same-day
    // sell of them (slice 014 step 3), and this test is about the pause gate.
    let (registry, handle) = controlled_registry(&store, None, "f1-sell", CN_0935 - DAY_NS);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    publish_quote(&node, moutai(), "1700.00", 100, CN_0935 - DAY_NS);
    await_quote(&node, moutai(), "1700.00");
    let desk = handle.desk_id().to_owned();

    let bought = order(
        &store,
        &registry,
        &desk,
        "f1-sell-buy",
        "600519.XSHG",
        "BUY",
        "MARKET",
        100,
        None,
    )
    .expect("the opening buy fills");
    assert_eq!(bought["status"], "FILLED", "{bought}");

    // Day D: the lock has lapsed and the shares are sellable.
    advance(&node, CN_0935);
    publish_quote(&node, moutai(), "1700.00", 100, CN_0935);
    await_quote(&node, moutai(), "1700.00");

    let rested = order(
        &store,
        &registry,
        &desk,
        "f1-sell-1",
        "600519.XSHG",
        "SELL",
        "LIMIT",
        100,
        Some("1800.00"),
    )
    .expect("the resting sell is accepted");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    advance(&node, CN_1130);
    publish_status(&node, moutai(), MarketStatusAction::Pause, CN_1130);

    // A bid above the sell limit: crossing, and ignored while paused.
    publish_quote(&node, moutai(), "1900.00", 100, CN_1130);
    await_quote(&node, moutai(), "1900.00");
    assert_eq!(
        status(&node, "f1-sell-1"),
        OrderStatus::Accepted,
        "the paused engine does not fill a resting sell"
    );

    advance(&node, CN_1300);
    publish_status(&node, moutai(), MarketStatusAction::Trading, CN_1300);
    publish_quote(&node, moutai(), "1950.00", 100, CN_1300);
    within(10, "the resting sell fills after the reopen", || {
        status(&node, "f1-sell-1") == OrderStatus::Filled
    });
    assert_eq!(
        kinds(&store, &desk, "f1-sell-1"),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
    );
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// F5 (c): recovery matches the new book only
// ---------------------------------------------------------------------------

/// A quote delivered while the gate is closed still replaces the book, so the
/// pause leaves a crossing book behind. The first quote after the reopen
/// replaces *that*: a non-crossing one produces no fill, and only a later
/// crossing one does.
#[test]
fn recovery_matches_only_the_new_book() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f5-recovery", "1700.00");
    let desk = handle.desk_id().to_owned();

    let rested = order(
        &store,
        &registry,
        &desk,
        "f5-recovery-1",
        "600519.XSHG",
        "BUY",
        "LIMIT",
        100,
        Some("1600.00"),
    )
    .expect("the resting buy is accepted");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    // Closed, and a crossing update arrives anyway (the daemon's own gate is
    // what withholds it in production; here it proves the book still moves).
    advance(&node, CN_1130);
    publish_status(&node, moutai(), MarketStatusAction::Pause, CN_1130);
    publish_quote(&node, moutai(), "1500.00", 100, CN_1130);
    await_quote(&node, moutai(), "1500.00");
    assert_eq!(status(&node, "f5-recovery-1"), OrderStatus::Accepted);

    // Recovery with a *new* reference that does not cross: no fill against the
    // stale 1500 book.
    advance(&node, CN_1300);
    publish_status(&node, moutai(), MarketStatusAction::Trading, CN_1300);
    publish_quote(&node, moutai(), "1700.00", 100, CN_1300);
    await_quote(&node, moutai(), "1700.00");
    assert_eq!(
        status(&node, "f5-recovery-1"),
        OrderStatus::Accepted,
        "the first post-recovery quote replaced the crossing book; no fill"
    );
    assert_eq!(fills(&store), 0);

    // And a genuinely crossing quote afterwards does fill it.
    let later = CN_1300 + 60 * SECOND_NS;
    advance(&node, later);
    publish_quote(&node, moutai(), "1550.00", 100, later);
    within(10, "the new book fills the resting order", || {
        status(&node, "f5-recovery-1") == OrderStatus::Filled
    });
    registry.stop_all();
}

/// F1's sub-question: with a crossing book already cached, an `Open` engine and
/// a resting order, does anything but that instrument's own quote match it?
///
/// Every `iterate` call site in
/// `nautilus-execution-0.62.0/src/matching_engine/mod.rs` is reached from one
/// instrument's own data — `process_quote_tick` (`:1590`, `:1615`),
/// `process_trade_tick` (`:2123`, `:2221`), `process_bar` (`:1870`, `:2055`),
/// `process_order_book_delta(s)`/`_depth10` (`:1378`-`:1536`) and
/// `process_instrument_close` (`:2331`) — and the sandbox gates three of them
/// off: `trade_execution` and `bar_execution` are `false` in
/// `crate::node::sandbox_config`, and MarketRig publishes neither deltas nor
/// closes. `process_status` and `process_order` do not call `iterate` at all.
/// The node's only two timers (`crate::feasibility::clock`'s module docs) touch
/// no order. This exercises the three live candidates against a crossing cached
/// book: the sandbox expiry sweep, the portfolio equity-curve sample, and a
/// quote for a *different* instrument on the same venue and the same sandbox
/// client.
#[test]
fn only_the_instruments_own_quote_matches_the_cached_book() {
    // On the US venue: the candidate question is venue-agnostic, and only the CN
    // instruments carry MarketRig's own 14:57 alert, which would otherwise
    // cancel the resting order halfway through this test (slice 014 step 3).
    let sibling = crate::catalog::find("MSFT.XNAS").expect("a second XNAS entry");
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f1-events", "1700.00");
    let desk = handle.desk_id().to_owned();
    publish_quote(&node, apple(), "300.00", 100, CN_0935);
    await_quote(&node, apple(), "300.00");

    let rested = order(
        &store,
        &registry,
        &desk,
        "f1-events-1",
        "AAPL.XNAS",
        "BUY",
        "LIMIT",
        10,
        Some("290.00"),
    )
    .expect("the resting buy is accepted");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    // The only way to leave a crossing book behind a resting order: take the
    // crossing quote while the engine is paused, then reopen it.
    advance(&node, CN_1130);
    publish_status(&node, apple(), MarketStatusAction::Pause, CN_1130);
    publish_quote(&node, apple(), "280.00", 100, CN_1130);
    await_quote(&node, apple(), "280.00");
    advance(&node, CN_1300);
    publish_status(&node, apple(), MarketStatusAction::Trading, CN_1300);
    assert_eq!(status(&node, "f1-events-1"), OrderStatus::Accepted);

    // Candidate 1 and 2: every timer this node owns, across UTC midnight, so the
    // sandbox sweep and the portfolio equity-curve sample both fire.
    let midnight = CN_0935 + 22 * 3_600 * SECOND_NS + 25 * 60 * SECOND_NS;
    let fired = advance(&node, midnight + 60 * SECOND_NS);
    assert!(
        fired.iter().any(|n| n.ends_with("-sandbox-expiry-sweep"))
            && fired
                .iter()
                .any(|n| n.starts_with("portfolio_equity_curve.")),
        "both timer families fired: {:?}",
        fired.iter().collect::<std::collections::BTreeSet<_>>()
    );
    assert_eq!(
        status(&node, "f1-events-1"),
        OrderStatus::Accepted,
        "no timer matched the crossing cached book"
    );

    // Candidate 3: a quote for another instrument on the same venue, through the
    // same sandbox execution client.
    let other = midnight + 120 * SECOND_NS;
    publish_quote(&node, sibling, "400.00", 100, other);
    await_quote(&node, sibling, "400.00");
    assert_eq!(
        status(&node, "f1-events-1"),
        OrderStatus::Accepted,
        "another instrument's tick drives only its own engine"
    );
    assert_eq!(fills(&store), 0);

    // And the instrument's own quote does match it.
    let own = other + 60 * SECOND_NS;
    advance(&node, own);
    publish_quote(&node, apple(), "285.00", 100, own);
    within(10, "its own quote fills it", || {
        status(&node, "f1-events-1") == OrderStatus::Filled
    });
    registry.stop_all();
}

/// The provider-transition defect. `OrderMatchingEngine::process_quote_tick`
/// treats a quote whose `ts_event` predates `book.ts_last` as stale: it skips
/// the book update and still calls `iterate(quote.ts_init)`
/// (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:1584-1592`). The two
/// CN providers stamp `ts_event` differently — HiThink with receipt
/// (`crate::node::cn_cycle`), Yahoo with `meta.regularMarketTime`
/// (`crate::node::poll_once`) — so the first Yahoo quote after a switch can be
/// older, and then the *old* provider's book matches at the new quote's arrival
/// instant.
#[test]
fn a_stale_stamped_quote_matches_the_old_book() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f5-stale", "1700.00");
    let desk = handle.desk_id().to_owned();

    let rested = order(
        &store,
        &registry,
        &desk,
        "f5-stale-1",
        "600519.XSHG",
        "BUY",
        "LIMIT",
        100,
        Some("1600.00"),
    )
    .expect("the resting buy is accepted");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    // The old provider's last book: crossing, taken while the gate was closed.
    advance(&node, CN_1130);
    publish_status(&node, moutai(), MarketStatusAction::Pause, CN_1130);
    publish_quote(&node, moutai(), "1500.00", 100, CN_1130);
    await_quote(&node, moutai(), "1500.00");
    assert_eq!(status(&node, "f5-stale-1"), OrderStatus::Accepted);

    // The switch: the new provider's first quote carries a correct,
    // non-crossing reference of 1700 but an older `ts_event` (a market time
    // from 10:30) and a current `ts_init` (13:00 receipt).
    advance(&node, CN_1300);
    publish_status(&node, moutai(), MarketStatusAction::Trading, CN_1300);
    publish_quote_stamped(
        &node,
        moutai(),
        "1700.00",
        100,
        CN_1130 - 3_600 * SECOND_NS,
        CN_1300,
    );

    within(10, "the stale-stamped quote is processed", || {
        status(&node, "f5-stale-1") != OrderStatus::Accepted
    });
    assert_eq!(
        status(&node, "f5-stale-1"),
        OrderStatus::Filled,
        "the stale-stamped quote matched the old provider's book"
    );
    let filled = chain(&store, &desk, "f5-stale-1")
        .into_iter()
        .find(|(kind, _)| kind == "OrderFilled")
        .expect("it filled");
    assert_eq!(
        filled.1, CN_1300 as i64,
        "stamped at the new quote's arrival instant: {filled:?}"
    );
    // The new quote's own price, 1700, does not cross a 1600 buy: the decision
    // to fill can only have come from the discarded 1500 book. The fill lands at
    // the resting order's own limit, so nothing in the fill row shows the stale
    // reference.
    let fill_price: String = store
        .call(|conn| conn.query_row("SELECT price FROM fills", [], |r| r.get(0)))
        .expect("the fill row");
    assert_eq!(
        fill_price, "1600.00",
        "at the resting limit — the fill row does not reveal the stale book"
    );
    // And what an observer reads is the new quote, which contradicts the fill.
    let observable = node
        .call(move |context| {
            context
                .cache
                .borrow()
                .quote(&InstrumentId::from(moutai().instrument_id))
                .map(|q| q.ask_price.to_string())
        })
        .unwrap();
    assert_eq!(
        observable.as_deref(),
        Some("1700.00"),
        "the cache took the stale-stamped quote the matching engine discarded"
    );
    registry.stop_all();
}

/// The "clear stale executable state" candidate: publish a book with both sizes
/// zero for every CN instrument at gate close. It suspends matching and leaves
/// the resting order alive — but a new MARKET order against it is *accepted and
/// left open*, not refused, so it is not a substitute for a submission gate.
#[test]
fn a_zero_size_book_stops_matching_and_a_quote_restores_it() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f5-zero", "1700.00");
    let desk = handle.desk_id().to_owned();

    let rested = order(
        &store,
        &registry,
        &desk,
        "f5-zero-1",
        "600519.XSHG",
        "BUY",
        "LIMIT",
        100,
        Some("1600.00"),
    )
    .expect("the resting buy is accepted");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    // Gate close: a crossing price with no size on either side. (Still inside
    // the afternoon session, because §5.1's admission check now refuses a CN
    // order out of session before the book can be asked anything.)
    advance(&node, CN_1305);
    publish_book(&node, moutai(), ("1500.00", 0), ("1500.00", 0), CN_1305);
    await_quote(&node, moutai(), "1500.00");
    assert_eq!(
        status(&node, "f5-zero-1"),
        OrderStatus::Accepted,
        "a zero-size crossing book does not fill the resting order"
    );

    // A new MARKET order against it gets the sandbox's own refusal, in its own
    // words — the outcome feature SPEC §2.3 predicts for a suppressed side.
    let refused = order(
        &store,
        &registry,
        &desk,
        "f5-zero-2",
        "600519.XSHG",
        "BUY",
        "MARKET",
        100,
        None,
    )
    .expect_err("the zero-size book refuses a MARKET order");
    let TradeError::Rejected(reason) = &refused else {
        panic!("expected a sandbox rejection, got {refused:?}");
    };
    assert_eq!(
        reason, "No market for 600519.XSHG",
        "the sandbox's own no-market reason"
    );
    assert_eq!(
        kinds(&store, &desk, "f5-zero-2"),
        vec!["OrderInitialized", "OrderSubmitted", "OrderRejected"],
    );

    // A new *LIMIT* order is not refused: it rests on the zero-size book, which
    // is why the zeroed book alone is not a submission gate.
    let resting = order(
        &store,
        &registry,
        &desk,
        "f5-zero-3",
        "600519.XSHG",
        "BUY",
        "LIMIT",
        100,
        Some("1800.00"),
    )
    .expect("a crossing LIMIT is accepted, not refused");
    assert_eq!(resting["status"], "ACCEPTED", "{resting}");
    assert_eq!(fills(&store), 0, "and nothing filled");

    // The next real quote restores matching for both resting orders.
    let later = CN_1305 + 60 * SECOND_NS;
    advance(&node, later);
    publish_quote(&node, moutai(), "1550.00", 100, later);
    within(10, "both resting orders fill on the restored book", || {
        status(&node, "f5-zero-1") == OrderStatus::Filled
            && status(&node, "f5-zero-3") == OrderStatus::Filled
    });
    assert_eq!(
        kinds(&store, &desk, "f5-zero-1"),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
    );
    registry.stop_all();
}

/// F5's shared/local observation order. `crate::node::poll_once` and
/// `crate::node::cn_cycle` both call `MarketState::accept*` and *then*
/// `sender.send(DataEvent::Data(..))`, and that send only queues work for the
/// node thread. So the installation-wide observation can be strictly newer than
/// the book the sandbox will match against — a validation that reads
/// `MarketState` is not reading the book that decides the fill.
#[test]
fn market_state_leads_the_node_book() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f5-order", "1700.00");
    let desk = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(moutai().instrument_id);

    // Exactly what `poll_once` does first, with the publish that would follow it
    // withheld: the shared state advances, the node's book does not.
    registry.market().accept(
        moutai(),
        &ChartQuote {
            price: Decimal::new(150_000, 2),
            currency: "CNY".to_owned(),
            source_time_ns: CN_1130 as i64,
        },
        CN_1130 as i64,
    );
    let shared = registry.market().read(moutai(), CN_1130 as i64);
    assert_eq!(shared.last.as_deref(), Some("1500.00"));
    assert_eq!(shared.health, Health::Live);
    let local = node
        .call(move |context| {
            context
                .cache
                .borrow()
                .quote(&instrument_id)
                .map(|q| q.ask_price.to_string())
        })
        .unwrap();
    assert_eq!(
        local.as_deref(),
        Some("1700.00"),
        "the node's book is still the older observation"
    );

    // A buy at 1600 looks marketable against the shared 1500 and is not against
    // the local 1700, and the local book is what decides: the order rests.
    let rested = order(
        &store,
        &registry,
        &desk,
        "f5-order-1",
        "600519.XSHG",
        "BUY",
        "LIMIT",
        100,
        Some("1600.00"),
    )
    .expect("the order is accepted");
    assert_eq!(
        rested["status"], "ACCEPTED",
        "the sandbox matched the older local book, not the newer shared one: {rested}"
    );
    assert_eq!(fills(&store), 0);
    registry.stop_all();
}
