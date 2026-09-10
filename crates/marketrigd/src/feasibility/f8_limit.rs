//! F8 items 1 and 2 — the AE-9 sampled-price LIMIT trigger on the real sandbox
//! (`sdd/features/a-share-engine/FEASIBILITY.md` §F8; feature SPEC §2.3–§2.5).
//!
//! Instrument: `000001.XSHE` (Ping An Bank), tick 0.01, lot 100 — the CN
//! catalog name whose real price lives near the SPEC's own 9.90/9.95/10.00
//! example, so the numbers are used literally. The XSHE account opens with
//! 500,000 CNY (`crate::node::seed`), fee 3 bp per side
//! (`MakerTakerFeeModel`, `node.rs:511`).
//!
//! **Volume never reaches the node.** Cumulative volume is a field of the
//! daemon's HiThink snapshot; a `QuoteTick` has no such field and the matching
//! engine has no notion of it. §2.5's volume predicate is therefore a
//! daemon-side decision about *what to publish*, not matching. Every test below
//! writes that decision out in the open: a non-qualifying snapshot publishes an
//! **idle book** (both sides sized zero at the observed last price, which F4
//! proved clears the L1 side and leaves `core.bid`/`core.ask` `None`), and a
//! qualifying snapshot publishes a **crossing book** for one tick — ask = last
//! for BUYs, bid = last for SELLs, size = the whole resting eligible quantity —
//! then restores the idle book. NautilusTrader alone decides the fill: a
//! resting LIMIT crosses as MAKER at *its own limit price*
//! (`determine_limit_price_and_volume`, `matching_engine/mod.rs:4032-4060`),
//! which is exactly AE-9's "full remaining quantity at the limit price".
//!
//! Findings, in the order the tests establish them:
//!
//! 1. [`buy_limit_waits_for_the_qualifying_observation`] /
//!    [`sell_limit_waits_for_the_qualifying_observation`] — item 1, both
//!    directions, with the native chain, MAKER liquidity, commission, CNY
//!    `total|locked|free` before and after, position and persisted history.
//! 2. [`price_isolation_is_native`] — one crossing publish reaches only the
//!    orders whose limit the published price crosses. Price-specific isolation
//!    is free; no per-order switch is involved.
//! 3. [`observation_before_admission_cannot_trigger`] — item 2's decisive case.
//!    Publishing on the node thread *before* admitting removes the "received
//!    before admission but queued for delivery" state: the earlier observation
//!    has already iterated when the new order is placed, so it cannot trigger
//!    it, and the new order waits for its own qualifying observation.
//! 4. [`equal_volume_after_an_incompatible_increase_never_fills`] — an
//!    incompatible price with increased volume, then a compatible equal-volume
//!    read: no publish, no fill, and the next increase fills both orders whose
//!    baselines it beats.
//! 5. [`admission_inside_the_crossing_window_fills_at_submission`] — the
//!    hazard, and the one thing the daemon must do beyond sequencing: while the
//!    crossing book is up, a newly admitted compatible LIMIT fills at
//!    submission as TAKER, violating §2.4. Publish-crossing → confirm →
//!    restore-idle must therefore exclude admission (one node-level mutex; both
//!    are already `Node::call` closures).
//! 6. [`trade_tick_is_inert_under_the_daemon_config`] — the evaluated
//!    alternative signal. `trade_execution` is a real sandbox config seam, but
//!    MarketRig sets it `false` (`node.rs:513-515`), so a `TradeTick` moves
//!    nothing today; and `process_trade_tick` sets *both* core sides to the
//!    trade price (`mod.rs:2150-2208`), so even enabled it would leave a
//!    two-sided crossing book and still need the idle restore.
//!
//! Platform: macOS arm64 only.

use std::sync::Arc;

use nautilus_core::UnixNanos;
use nautilus_model::data::TradeTick;
use nautilus_model::enums::{AggressorSide, OrderStatus};
use nautilus_model::identifiers::{AccountId, ClientOrderId, InstrumentId, TradeId};
use nautilus_model::orders::Order;
use nautilus_model::types::{Price, Quantity};
use serde_json::Value;

use crate::catalog::Entry;
use crate::feasibility::clock::{
    CN_0935, ClockHandle, DAY_NS, SECOND_NS, advance, controlled_registry, publish_book,
    stored_events,
};
use crate::node::{Node, Registry, within};
use crate::store::Store;
use crate::trade::{self, TradeError};

/// The XSHE account's opening cash (`crate::node::seed`: the CN seed split
/// across the two CN venues).
const OPENING: &str = "500000.00 CNY|0.00 CNY|500000.00 CNY";

fn pingan() -> &'static Entry {
    crate::catalog::find("000001.XSHE").expect("the CN catalog entry")
}

fn pingan_id() -> InstrumentId {
    InstrumentId::from(pingan().instrument_id)
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A started desk on the controlled clock at 09:35 Asia/Shanghai on day **D−1**,
/// no feed: every observation below is published by hand, and a test that needs
/// inventory buys it before today so §1.1's T+1 lock does not hold it (slice 014
/// step 3).
fn desk(store: &Store, name: &'static str) -> (Registry, ClockHandle, Arc<Node>) {
    let (registry, handle) = controlled_registry(store, None, name, CN_0935 - DAY_NS);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    (registry, handle, node)
}

/// Waits until the node's cache holds the quote stamped `at_ns`.
fn awaited(node: &Node, at_ns: u64) {
    within(10, "the published observation reaches the node", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .quote(&pingan_id())
                .is_some_and(|quote| quote.ts_event.as_u64() == at_ns)
        })
        .unwrap()
    });
}

/// What the daemon publishes for a **non-qualifying** snapshot: the observed
/// last price on both sides with size zero. F4 proved a non-positive L1 size
/// clears that side (`orderbook/ladder.rs:238-244`), so this book crosses
/// nothing in either direction and every compatible LIMIT rests (§2.4).
fn idle(node: &Node, last: &str, at_ns: u64) {
    advance(node, at_ns);
    publish_book(node, pingan(), (last, 0), (last, 0), at_ns);
    awaited(node, at_ns);
}

/// What the daemon publishes for a **qualifying** snapshot with resting BUYs:
/// an ask at the observed last price, sized to the whole resting eligible
/// quantity. Every crossing resting BUY fills as MAKER at its own limit price.
fn crossing_ask(node: &Node, last: &str, size: u32, at_ns: u64) {
    advance(node, at_ns);
    publish_book(node, pingan(), (last, 0), (last, size), at_ns);
    awaited(node, at_ns);
}

/// The mirror for resting SELLs.
fn crossing_bid(node: &Node, last: &str, size: u32, at_ns: u64) {
    advance(node, at_ns);
    publish_book(node, pingan(), (last, size), (last, 0), at_ns);
    awaited(node, at_ns);
}

/// A two-sided sized book, used only to seed a position with a MARKET order.
fn sized(node: &Node, price: &str, size: u32, at_ns: u64) {
    advance(node, at_ns);
    publish_book(node, pingan(), (price, size), (price, size), at_ns);
    awaited(node, at_ns);
}

/// One `TradeTick` with chosen timestamps, straight into the data runner —
/// [`crate::feasibility::clock::publish_quote`]'s sibling for the alternative
/// signal evaluated in [`trade_tick_fills_but_leaves_a_crossing_book`].
fn publish_trade(node: &Node, price: &str, size: u32, at_ns: u64) {
    let (price, size) = (Price::from(price), Quantity::from(size));
    node.call(move |_| {
        let tick = TradeTick::new(
            pingan_id(),
            price,
            size,
            AggressorSide::NoAggressor,
            TradeId::new("F8-TRADE"),
            UnixNanos::from(at_ns),
            UnixNanos::from(at_ns),
        );
        nautilus_common::live::runner::get_data_event_sender()
            .send(nautilus_common::messages::DataEvent::Data(
                nautilus_model::data::Data::Trade(tick),
            ))
            .expect("the data runner takes the trade");
    })
    .expect("the node answers");
}

/// The production submit path for Ping An. `price` of `None` is a MARKET order.
fn submit(
    store: &Store,
    registry: &Registry,
    desk_id: &str,
    action_id: &str,
    side: &str,
    quantity: u32,
    price: Option<&str>,
) -> Result<Value, TradeError> {
    let (kind, price) = match price {
        Some(price) => ("LIMIT", format!("\"{price}\"")),
        None => ("MARKET", "null".to_owned()),
    };
    let body = format!(
        r#"{{"action_id":"{action_id}","instrument_id":"000001.XSHE","side":"{side}",
            "type":"{kind}","quantity":"{quantity}","price":{price}}}"#
    );
    trade::submit(store, registry, desk_id, &body, &trade::Source::Session)
        .map(|(record, _)| record.outcome.expect("a placed order answers"))
}

fn status(node: &Node, client_order_id: &str) -> OrderStatus {
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

/// The captured native chain for one order, oldest first.
fn chain(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<String> {
    stored_events(store, desk_id)
        .into_iter()
        .filter(|(id, _, _)| id == client_order_id)
        .map(|(_, kind, _)| kind)
        .collect()
}

/// Every captured fill for one order: `(quantity, price, commission)`, exactly
/// as NautilusTrader emitted them (`crate::trade::event_row`).
fn fills(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<(String, String, String)> {
    let args = (desk_id.to_owned(), client_order_id.to_owned());
    store
        .call(move |conn| {
            conn.prepare(
                "SELECT quantity, price, commission FROM fills \
                 WHERE desk_id = ?1 AND client_order_id = ?2 ORDER BY occurred_at_ns, id",
            )?
            .query_map([args.0, args.1], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
        })
        .expect("the fills read")
}

/// The stored `order_events.payload` for one event kind — the sandbox's own
/// serialized event, verbatim.
fn payload(store: &Store, desk_id: &str, client_order_id: &str, kind: &str) -> String {
    let args = (
        desk_id.to_owned(),
        client_order_id.to_owned(),
        kind.to_owned(),
    );
    store
        .call(move |conn| {
            conn.query_row(
                "SELECT payload FROM order_events \
                 WHERE desk_id = ?1 AND client_order_id = ?2 AND kind = ?3",
                [args.0, args.1, args.2],
                |r| r.get(0),
            )
        })
        .expect("the payload read")
}

/// The XSHE account's CNY balance as `total|locked|free`, from the node cache's
/// own `AccountAny` — the sandbox's reservation accounting, never recomputed.
fn balance_cny(node: &Node, desk_id: &str) -> String {
    let account_id = AccountId::from(format!("XSHE-{desk_id}").as_str());
    node.call(move |context| {
        let cache = context.cache.borrow();
        let account = cache.account(&account_id).expect("the venue account");
        let balance = account
            .balances()
            .values()
            .copied()
            .next()
            .expect("one CNY balance");
        format!("{}|{}|{}", balance.total, balance.locked, balance.free)
    })
    .expect("the node answers")
}

/// The desk's open long quantity in Ping An.
fn position(node: &Node) -> f64 {
    node.call(|context| {
        context
            .cache
            .borrow()
            .positions_open(None, Some(&pingan_id()), None, None, None)
            .iter()
            .map(|p| p.signed_qty)
            .sum::<f64>()
    })
    .expect("the node answers")
}

/// One order's replayed `history_orders` projection.
fn history(store: &Store, desk_id: &str, client_order_id: &str) -> Value {
    trade::history_orders(store, desk_id)
        .expect("the history reads")
        .into_iter()
        .find(|order| order["client_order_id"] == client_order_id)
        .unwrap_or_else(|| panic!("{client_order_id} replays into history_orders"))
}

/// Prints the desk's whole captured native record under `--nocapture`.
fn dump(label: &str, store: &Store, desk_id: &str) {
    let desk = desk_id.to_owned();
    let rows: Vec<String> = store
        .call(move |conn| {
            conn.prepare(
                "SELECT client_order_id || ' ' || kind || ' ts_event=' || occurred_at_ns \
                 || ' ' || payload FROM order_events \
                 WHERE desk_id = ?1 ORDER BY occurred_at_ns, id",
            )?
            .query_map([desk], |r| r.get(0))?
            .collect()
        })
        .expect("the order events read");
    eprintln!("--- {label}: order_events");
    for row in rows {
        eprintln!("{row}");
    }
}

// ---------------------------------------------------------------------------
// F8 item 1 — the trigger, both directions
// ---------------------------------------------------------------------------

/// F8 1: BUY 100 @ 10.00 with the cached compatible last at 9.90 / volume 1000.
/// No submission-time fill; 9.95 / 1000 does not fill; 9.95 / 1001 fills exactly
/// 100 @ 10.00 natively, as MAKER, at 3 bp.
#[test]
fn buy_limit_waits_for_the_qualifying_observation() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8-buy");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    // Observation 1 — last 9.90, cumulative volume 1000. The volume is the
    // daemon's; the node sees only the price, on a book that crosses nothing.
    idle(&node, "9.90", at);
    assert_eq!(balance_cny(&node, &desk_id), OPENING);

    // Admission: baseline = (observation 1, volume 1000). §2.4 — even a
    // compatible LIMIT rests.
    let accepted = submit(
        &store,
        &registry,
        &desk_id,
        "f8-buy-1",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the compatible limit buy is accepted");
    assert_eq!(accepted["status"], "ACCEPTED", "{accepted}");
    assert_eq!(
        chain(&store, &desk_id, "f8-buy-1"),
        vec!["OrderInitialized", "OrderSubmitted", "OrderAccepted"],
        "no submission-time fill: the chain stops at OrderAccepted"
    );
    assert_eq!(
        balance_cny(&node, &desk_id),
        "500000.00 CNY|1000.00 CNY|499000.00 CNY",
        "the sandbox reserved 100 x 10.00"
    );

    // Observation 2 — 9.95 / volume 1000. Price compatible, volume unchanged:
    // §2.5 says equal-volume snapshots never trigger, so the daemon publishes
    // the idle book and nothing crosses.
    at += SECOND_NS;
    idle(&node, "9.95", at);
    assert_eq!(status(&node, "f8-buy-1"), OrderStatus::Accepted);
    assert!(
        fills(&store, &desk_id, "f8-buy-1").is_empty(),
        "9.95 / 1000 does not fill"
    );

    // Observation 3 — 9.95 / volume 1001. Volume exceeds both the order
    // baseline (1000) and the preceding observation (1000), price compatible:
    // the daemon publishes an ask at the observed last, sized to the whole
    // resting eligible quantity.
    at += SECOND_NS;
    crossing_ask(&node, "9.95", 100, at);
    within(
        10,
        "the qualifying observation fills the resting buy",
        || status(&node, "f8-buy-1") == OrderStatus::Filled,
    );
    // …and the idle book goes straight back up, so nothing admitted later
    // crosses (see `admission_inside_the_crossing_window_fills_at_submission`).
    at += SECOND_NS;
    idle(&node, "9.95", at);

    // The native record.
    assert_eq!(
        chain(&store, &desk_id, "f8-buy-1"),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
    );
    assert_eq!(
        fills(&store, &desk_id, "f8-buy-1"),
        vec![("100".to_owned(), "10.00".to_owned(), "0.30".to_owned())],
        "the full remaining quantity at the order's own limit price, 3 bp"
    );
    let filled = payload(&store, &desk_id, "f8-buy-1", "OrderFilled");
    assert!(
        filled.contains(r#""liquidity_side":"MAKER""#),
        "the resting order is the maker: {filled}"
    );
    assert!(
        filled.contains(r#""last_px":"10.00""#) && filled.contains(r#""last_qty":"100""#),
        "the payload is the sandbox's own, uncorrected: {filled}"
    );
    assert_eq!(
        balance_cny(&node, &desk_id),
        "498999.70 CNY|0.00 CNY|498999.70 CNY",
        "500000 - 1000.00 notional - 0.30 commission, reservation released"
    );
    assert_eq!(position(&node), 100.0);

    let replayed = history(&store, &desk_id, "f8-buy-1");
    assert_eq!(replayed["status"], "FILLED", "{replayed}");
    assert_eq!(replayed["filled_quantity"], "100", "{replayed}");
    assert_eq!(replayed["average_price"], "10.00", "{replayed}");
    assert_eq!(replayed["price"], "10.00", "{replayed}");

    dump("F8.1 BUY", &store, &desk_id);
    registry.stop_all();
}

/// F8 1, mirrored: SELL 100 @ 9.90 with the cached compatible last at 10.00 /
/// volume 1000. 9.95 / 1000 does not fill; 9.95 / 1001 fills exactly
/// 100 @ 9.90.
#[test]
fn sell_limit_waits_for_the_qualifying_observation() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8-sell");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 - DAY_NS + SECOND_NS;

    // Seed the holding on a sized book on day D−1, then take the book away.
    sized(&node, "10.00", 100, at);
    let seeded = submit(
        &store,
        &registry,
        &desk_id,
        "f8-sell-seed",
        "BUY",
        100,
        None,
    )
    .expect("the seeding market buy is accepted");
    assert_eq!(seeded["status"], "FILLED", "{seeded}");
    assert_eq!(position(&node), 100.0);
    let after_seed = balance_cny(&node, &desk_id);
    assert_eq!(after_seed, "498999.70 CNY|0.00 CNY|498999.70 CNY");

    // Day D, observation 1 — last 10.00, volume 1000. Compatible for a SELL @
    // 9.90 (10.00 >= 9.90) and still no submission-time fill.
    at = CN_0935 + SECOND_NS;
    idle(&node, "10.00", at);
    let accepted = submit(
        &store,
        &registry,
        &desk_id,
        "f8-sell-1",
        "SELL",
        100,
        Some("9.90"),
    )
    .expect("the compatible limit sell is accepted");
    assert_eq!(accepted["status"], "ACCEPTED", "{accepted}");
    assert_eq!(
        chain(&store, &desk_id, "f8-sell-1"),
        vec!["OrderInitialized", "OrderSubmitted", "OrderAccepted"],
    );
    assert_eq!(
        balance_cny(&node, &desk_id),
        "498999.70 CNY|100.00 CNY|498899.70 CNY",
        "a cash-account SELL locks the *share count*, booked in the account \
         currency (`CashAccount::calculate_balance_locked`): 100, not 990.00 CNY"
    );

    // Observation 2 — 9.95 / volume 1000: compatible, equal volume, no publish.
    at += SECOND_NS;
    idle(&node, "9.95", at);
    assert_eq!(status(&node, "f8-sell-1"), OrderStatus::Accepted);
    assert!(fills(&store, &desk_id, "f8-sell-1").is_empty());

    // Observation 3 — 9.95 / volume 1001: qualifying, so a bid at the observed
    // last sized to the resting quantity.
    at += SECOND_NS;
    crossing_bid(&node, "9.95", 100, at);
    within(
        10,
        "the qualifying observation fills the resting sell",
        || status(&node, "f8-sell-1") == OrderStatus::Filled,
    );
    at += SECOND_NS;
    idle(&node, "9.95", at);

    assert_eq!(
        chain(&store, &desk_id, "f8-sell-1"),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
    );
    assert_eq!(
        fills(&store, &desk_id, "f8-sell-1"),
        vec![("100".to_owned(), "9.90".to_owned(), "0.30".to_owned())],
        "the full quantity at the SELL's own limit price, not at the 9.95 bid"
    );
    let filled = payload(&store, &desk_id, "f8-sell-1", "OrderFilled");
    assert!(filled.contains(r#""liquidity_side":"MAKER""#), "{filled}");
    assert_eq!(
        balance_cny(&node, &desk_id),
        "499989.40 CNY|0.00 CNY|499989.40 CNY",
        "498999.70 + 990.00 proceeds - 0.30 commission"
    );
    assert_eq!(position(&node), 0.0);

    let replayed = history(&store, &desk_id, "f8-sell-1");
    assert_eq!(replayed["status"], "FILLED", "{replayed}");
    assert_eq!(replayed["average_price"], "9.90", "{replayed}");

    dump("F8.1 SELL", &store, &desk_id);
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// F8 item 2 — the temporal predicate across several orders
// ---------------------------------------------------------------------------

/// The isolation the sandbox gives for free: one crossing publish reaches only
/// the orders whose limit the published price actually crosses. A BUY @ 10.00
/// fills on an ask of 9.95; a BUY @ 9.90 admitted at the same baseline does
/// not. No per-order switch is involved.
#[test]
fn price_isolation_is_native() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8-price");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    idle(&node, "9.90", at);
    for (action, price) in [("f8-price-hi", "10.00"), ("f8-price-lo", "9.90")] {
        let accepted = submit(&store, &registry, &desk_id, action, "BUY", 100, Some(price))
            .expect("both limit buys rest");
        assert_eq!(accepted["status"], "ACCEPTED", "{accepted}");
    }

    // A qualifying observation at 9.95: compatible with 10.00, not with 9.90.
    at += SECOND_NS;
    crossing_ask(&node, "9.95", 100, at);
    within(10, "the compatible order fills", || {
        status(&node, "f8-price-hi") == OrderStatus::Filled
    });
    at += SECOND_NS;
    idle(&node, "9.95", at);

    assert_eq!(
        fills(&store, &desk_id, "f8-price-hi"),
        vec![("100".to_owned(), "10.00".to_owned(), "0.30".to_owned())],
    );
    assert_eq!(status(&node, "f8-price-lo"), OrderStatus::Accepted);
    assert!(
        fills(&store, &desk_id, "f8-price-lo").is_empty(),
        "9.95 is above the 9.90 limit: no fill, no partial, no allocation"
    );
    registry.stop_all();
}

/// F8 2, the decisive case: an observation accepted **before** a later order's
/// admission cannot trigger that order, and a snapshot "still queued for
/// delivery" at admission cannot exist.
///
/// The mechanism is sequencing, not a per-order switch: publish and admission
/// are both `Node::call` closures on the one node thread, so an observation
/// accepted first is published — and has iterated — before the order is placed.
/// Queueing alone is not enough: the runner's `biased` select drains
/// `exec_cmd_rx` before `data_evt_rx` (`nautilus-live-0.62.0/src/runner.rs:579-606`,
/// F3), so a `SubmitOrder` queued after a quote can still overtake it. The
/// daemon must *confirm* the publish landed before admitting, which is what
/// every wait below does (the data engine adds the quote to the cache and then
/// publishes it to the sandbox inside one `handle_quote` call, so a later
/// `Node::call` that sees the cached quote sees an engine that has iterated).
/// The new order is then admitted onto an idle book with its own baseline and
/// waits for the next qualifying observation.
#[test]
fn observation_before_admission_cannot_trigger() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8-baseline");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    // Order A, baseline = (observation 1, volume 1000).
    idle(&node, "9.90", at);
    submit(
        &store,
        &registry,
        &desk_id,
        "f8-base-a",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("A rests");

    // Observation 2 — 9.95 / 1001. Qualifies A. It is accepted before B is
    // admitted, so it is published first: crossing ask, confirm, idle restore.
    at += SECOND_NS;
    crossing_ask(&node, "9.95", 100, at);
    within(10, "A fills on its own qualifying observation", || {
        status(&node, "f8-base-a") == OrderStatus::Filled
    });
    at += SECOND_NS;
    idle(&node, "9.95", at);

    // Order B, admitted after that observation: baseline = (observation 2,
    // volume 1001). Same instrument, same limit, same price compatibility.
    let accepted = submit(
        &store,
        &registry,
        &desk_id,
        "f8-base-b",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("B is accepted");
    assert_eq!(accepted["status"], "ACCEPTED", "{accepted}");
    assert!(
        fills(&store, &desk_id, "f8-base-b").is_empty(),
        "the observation that filled A cannot trigger B: it had already iterated"
    );

    // Observation 3 — 9.95 / 1001 again: equal volume, no publish, no fill.
    at += SECOND_NS;
    idle(&node, "9.95", at);
    assert_eq!(status(&node, "f8-base-b"), OrderStatus::Accepted);

    // Observation 4 — 9.95 / 1002: exceeds B's baseline and the preceding
    // observation. B fills, on its own terms.
    at += SECOND_NS;
    crossing_ask(&node, "9.95", 100, at);
    within(10, "B fills on the next qualifying observation", || {
        status(&node, "f8-base-b") == OrderStatus::Filled
    });
    at += SECOND_NS;
    idle(&node, "9.95", at);

    assert_eq!(
        fills(&store, &desk_id, "f8-base-b"),
        vec![("100".to_owned(), "10.00".to_owned(), "0.30".to_owned())],
    );
    assert_eq!(
        balance_cny(&node, &desk_id),
        "497999.40 CNY|0.00 CNY|497999.40 CNY",
        "two fills of 1000.00 + 0.30"
    );
    assert_eq!(position(&node), 200.0);
    dump("F8.2 baselines", &store, &desk_id);
    registry.stop_all();
}

/// F8 2: an incompatible price with increased volume advances the preceding
/// baseline without filling; a compatible equal-volume read after it must not
/// fill; and the next increase fills both orders, because both baselines are
/// below it. Order B is admitted between the two, at the advanced baseline.
#[test]
fn equal_volume_after_an_incompatible_increase_never_fills() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8-equal");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    // Observation 1 — 9.90 / 1000. Order A, baseline 1000.
    idle(&node, "9.90", at);
    submit(
        &store,
        &registry,
        &desk_id,
        "f8-equal-a",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("A rests");

    // Observation 2 — 10.20 / 1001: volume up, price incompatible with A
    // (10.20 > 10.00). No publish crosses; the preceding baseline advances to
    // 1001. Order B is admitted here, at that baseline, with a limit the
    // current last price already satisfies.
    at += SECOND_NS;
    idle(&node, "10.20", at);
    assert_eq!(status(&node, "f8-equal-a"), OrderStatus::Accepted);
    let accepted = submit(
        &store,
        &registry,
        &desk_id,
        "f8-equal-b",
        "BUY",
        100,
        Some("10.30"),
    )
    .expect("B is accepted");
    assert_eq!(
        accepted["status"], "ACCEPTED",
        "a compatible LIMIT still rests at submission: {accepted}"
    );

    // Observation 3 — 9.95 / 1001: compatible with both, volume unchanged.
    // §2.5: "a later price-only change without more volume does not qualify".
    at += SECOND_NS;
    idle(&node, "9.95", at);
    assert_eq!(status(&node, "f8-equal-a"), OrderStatus::Accepted);
    assert_eq!(status(&node, "f8-equal-b"), OrderStatus::Accepted);
    assert!(fills(&store, &desk_id, "f8-equal-a").is_empty());
    assert!(fills(&store, &desk_id, "f8-equal-b").is_empty());

    // Observation 4 — 9.95 / 1002: exceeds A's 1000, B's 1001 and the preceding
    // 1001, compatible with both. One publish sized to the whole resting
    // eligible quantity fills both, each at its own limit price.
    at += SECOND_NS;
    crossing_ask(&node, "9.95", 200, at);
    within(10, "both resting orders fill", || {
        status(&node, "f8-equal-a") == OrderStatus::Filled
            && status(&node, "f8-equal-b") == OrderStatus::Filled
    });
    at += SECOND_NS;
    idle(&node, "9.95", at);

    assert_eq!(
        fills(&store, &desk_id, "f8-equal-a"),
        vec![("100".to_owned(), "10.00".to_owned(), "0.30".to_owned())],
    );
    assert_eq!(
        fills(&store, &desk_id, "f8-equal-b"),
        vec![("100".to_owned(), "10.30".to_owned(), "0.31".to_owned())],
        "each order fills at its own limit price; the ask price is only the trigger"
    );
    dump("F8.2 equal volume", &store, &desk_id);
    registry.stop_all();
}

/// F8 2, the hazard and the one daemon-side requirement beyond sequencing.
///
/// The crossing book is not consumed by the fills it causes: it is L1
/// top-of-book, restated by every quote. While it is up, a newly admitted
/// compatible LIMIT crosses it and fills **at submission**, as TAKER at the
/// published price — which violates §2.4 and gives that order a fill from an
/// observation that precedes its own baseline. Publish → confirm → restore must
/// therefore be one critical section that excludes admission.
#[test]
fn admission_inside_the_crossing_window_fills_at_submission() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8-window");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    idle(&node, "9.90", at);
    submit(
        &store,
        &registry,
        &desk_id,
        "f8-window-a",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("A rests");

    at += SECOND_NS;
    crossing_ask(&node, "9.95", 100, at);
    within(10, "A fills", || {
        status(&node, "f8-window-a") == OrderStatus::Filled
    });

    // No idle restore: admit B while the crossing ask still stands.
    let outcome = submit(
        &store,
        &registry,
        &desk_id,
        "f8-window-b",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("B is accepted");
    assert_eq!(
        outcome["status"], "FILLED",
        "HAZARD: B fills at submission against the crossing book: {outcome}"
    );
    assert_eq!(
        fills(&store, &desk_id, "f8-window-b"),
        vec![("100".to_owned(), "9.95".to_owned(), "0.30".to_owned())],
        "and it fills as a taker at the published price, not at its limit"
    );
    let filled = payload(&store, &desk_id, "f8-window-b", "OrderFilled");
    assert!(filled.contains(r#""liquidity_side":"TAKER""#), "{filled}");
    registry.stop_all();
}

/// The evaluated alternative signal, and why it is not the mechanism.
///
/// `OrderMatchingEngineConfig::trade_execution` *is* reachable — the sandbox
/// maps it (`nautilus-sandbox-0.62.0/src/config.rs:88,119`) — but MarketRig
/// sets it to `false` (`node.rs:513-515`), so a `TradeTick` reaches the cache
/// and moves nothing in the engine. Enabling it would be a product change, and
/// it buys nothing: `process_trade_tick` sets **both** core sides to the trade
/// price (`matching_engine/mod.rs:2150-2208`), so it would leave a two-sided
/// crossing book and still need the idle restore, while a `QuoteTick` shapes
/// one side at a time under the config the daemon already ships.
#[test]
fn trade_tick_is_inert_under_the_daemon_config() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8-trade");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    idle(&node, "9.90", at);
    submit(
        &store,
        &registry,
        &desk_id,
        "f8-trade-a",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("A rests");

    at += SECOND_NS;
    advance(&node, at);
    publish_trade(&node, "9.95", 1, at);
    within(10, "the trade tick reaches the node's cache", || {
        node.call(|context| {
            context
                .cache
                .borrow()
                .trade(&pingan_id())
                .is_some_and(|trade| trade.price == Price::from("9.95"))
        })
        .unwrap()
    });

    assert_eq!(
        status(&node, "f8-trade-a"),
        OrderStatus::Accepted,
        "with `trade_execution(false)` the trade never reaches the matching engine"
    );
    assert!(fills(&store, &desk_id, "f8-trade-a").is_empty());

    // And the quote route still works on the same node, unchanged.
    at += SECOND_NS;
    crossing_ask(&node, "9.95", 100, at);
    within(10, "the quote route fills it", || {
        status(&node, "f8-trade-a") == OrderStatus::Filled
    });
    dump("F8.2 trade tick", &store, &desk_id);
    registry.stop_all();
}
