//! F4 — does conservative book shaping constrain every fill?
//! (`sdd/features/a-share-engine/FEASIBILITY.md`; feature SPEC §2.3.)
//!
//! Every test runs the real `LiveNode` and the real
//! `SandboxExecutionClient` on the controlled clock from
//! [`crate::feasibility::clock`], with **no feed**: each quote is published by
//! hand through [`publish_book`], which is the only way to give the two sides
//! independent sizes. The band is the test's own arithmetic — `prev = 1200.00`
//! on a main-board name, so `limit_up = 1320.00` and `limit_down = 1080.00`;
//! the sandbox knows nothing about it.
//!
//! Findings, in the order the tests establish them:
//!
//! 1. [`zero_ask_blocks_buys_at_the_upper_limit`] — a `QuoteTick` whose
//!    `ask_size` is zero propagates all the way to the matching core. The
//!    ladder path is `OrderBook::update_quote_tick` →`update_book_ask` →
//!    `BookLadder::add` → `handle_l1_add`
//!    (`nautilus-model-0.62.0/src/orderbook/ladder.rs:238-244`), which clears
//!    the whole L1 side on a non-positive size instead of adding; the core
//!    reads it back at the end of `OrderMatchingEngine::iterate`
//!    (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:3821-3822`), so
//!    `core.ask` becomes `None`. A new MARKET BUY is then refused by the
//!    sandbox's own pre-check in `process_market_order` (`mod.rs:3234-3242`)
//!    with the reason `No market for 600519.XSHG`; a LIMIT BUY at the upper
//!    limit rests and does not fill on any number of repeated zero-ask
//!    publishes; and the SELL side still fills at the bid.
//! 2. [`restoring_the_ask_fills_the_resting_buy`] — restoring `ask 1319.99`
//!    fills the resting BUY, in band.
//! 3. [`market_remainder_slips_one_tick`] — with `ask 1319.99` size one lot, a
//!    MARKET BUY of three lots fills `100 @ 1319.99` **and** `200 @ 1320.00`:
//!    the L1 remainder path (`mod.rs:4790-4846`) slips the leaves by exactly
//!    one `price_increment` past the last fill price, once.
//! 4. [`market_remainder_at_the_limit_price_leaves_the_band`] — the
//!    counterfactual that makes the mechanism load-bearing: leave the ask at
//!    `1320.00` with a real size and the same order fills `200 @ 1320.01`,
//!    **outside** the band. `SandboxExecutionClientConfig` exposes no knob that
//!    stops it (see [`no_config_knob_bounds_the_slip`]), so the only supported
//!    defence is never to publish a sized ask at `limit_up` — which is exactly
//!    the zeroing rule.
//! 5. [`limit_remainder_rests_instead_of_slipping`] — the slip is MARKET-only;
//!    a LIMIT BUY leaves its remainder resting.
//! 6. [`zero_bid_blocks_sells_at_the_lower_limit`] and
//!    [`market_sell_remainder_slips_into_the_band`] — the mirror at
//!    `limit_down`.
//! 7. [`book_top_hides_a_suppressed_side`] — `feed::MarketState::book_all`
//!    derives both sizes from the catalog lot whenever an observation exists,
//!    so a suppressed side is invisible on the agent's book resource today.

use std::sync::Arc;

use nautilus_model::enums::OrderStatus;
use nautilus_model::identifiers::{ClientOrderId, InstrumentId};
use nautilus_model::orders::Order;
use rust_decimal::Decimal;
use serde_json::Value;

use crate::catalog::Entry;
use crate::feasibility::clock::{
    CN_0935, ClockHandle, SECOND_NS, advance, controlled_registry, publish_book, stored_events,
};
use crate::node::{Node, Registry, within};
use crate::store::Store;
use crate::trade::{self, TradeError};

/// The band the tests do their own arithmetic with: main board, `prev` 1200.00,
/// 10%, tick 0.01.
const LIMIT_UP: &str = "1320.00";
const LIMIT_DOWN: &str = "1080.00";
/// One tick under the upper limit, and one tick over the lower one.
const UNDER_UP: &str = "1319.99";
const OVER_DOWN: &str = "1080.01";
/// The instrument's lot, which is also the size the daemon synthesizes
/// (`crate::node::synthesized`).
const LOT: u32 = 100;

fn moutai() -> &'static Entry {
    crate::catalog::find("600519.XSHG").expect("the CN catalog entry")
}

/// A started desk on the controlled clock at 09:35 Asia/Shanghai, no feed, no
/// book yet. Its XSHG account holds 500,000 CNY (`crate::node::seed` split
/// across the two CN venues), which bounds every quantity used here.
fn desk(store: &Store, name: &'static str) -> (Registry, ClockHandle, Arc<Node>) {
    let (registry, handle) = controlled_registry(store, None, name, CN_0935);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    (registry, handle, node)
}

/// Advances the clock to `at_ns`, publishes one quote with independent sides,
/// and waits until the node has taken it. `at_ns` must increase across calls:
/// `OrderBook::update_quote_tick` drops a quote older than `book.ts_last`, and
/// the wait keys off the tick's own `ts_event`.
fn tick(node: &Node, bid: (&str, u32), ask: (&str, u32), at_ns: u64) {
    advance(node, at_ns);
    publish_book(node, moutai(), bid, ask, at_ns);
    let id = InstrumentId::from(moutai().instrument_id);
    within(10, "the published quote reaches the node", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .quote(&id)
                .is_some_and(|quote| quote.ts_event.as_u64() == at_ns)
        })
        .unwrap()
    });
}

/// The production submit path (`crate::trade::submit`), with the body built for
/// Moutai. `price` of `None` is a MARKET order.
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
        r#"{{"action_id":"{action_id}","instrument_id":"600519.XSHG","side":"{side}",
            "type":"{kind}","quantity":"{quantity}","price":{price}}}"#
    );
    trade::submit(store, registry, desk_id, &body, &trade::Source::Session)
        .map(|(record, _)| record.outcome.expect("a placed order answers"))
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

/// The captured native chain for one order, oldest first.
fn chain(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<String> {
    stored_events(store, desk_id)
        .into_iter()
        .filter(|(id, _, _)| id == client_order_id)
        .map(|(_, kind, _)| kind)
        .collect()
}

/// The stored `order_events.payload` for one event kind — the sandbox's own
/// serialized event, which is where its reason string lives verbatim.
fn payload(store: &Store, desk_id: &str, client_order_id: &str, kind: &str) -> Option<String> {
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
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        })
        .expect("the payload read")
}

/// Every captured fill, oldest first: `(client_order_id, side, quantity,
/// price)`. These are NautilusTrader's own `OrderFilled` fields as
/// `crate::trade::event_row` stored them, never recomputed.
fn fills(store: &Store, desk_id: &str) -> Vec<(String, String, String, String)> {
    let desk = desk_id.to_owned();
    store
        .call(move |conn| {
            conn.prepare(
                "SELECT client_order_id, side, quantity, price FROM fills \
                 WHERE desk_id = ?1 ORDER BY occurred_at_ns, id",
            )?
            .query_map([desk], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect()
        })
        .expect("the fills read")
}

/// The fill ladder for one order: `(quantity, price)` in capture order.
fn ladder(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<(String, String)> {
    fills(store, desk_id)
        .into_iter()
        .filter(|(id, _, _, _)| id == client_order_id)
        .map(|(_, _, quantity, price)| (quantity, price))
        .collect()
}

fn decimal(text: &str) -> Decimal {
    text.parse().expect("decimal text")
}

/// Prints the desk's whole captured native record under `--nocapture`: every
/// `order_events` row with its verbatim payload, then every `fills` row. This is
/// the spike's evidence, not an assertion.
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
    eprintln!("--- {label}: fills");
    for fill in fills(store, desk_id) {
        eprintln!("{fill:?}");
    }
}

/// §2.3's closing requirement: every fill price inside the inclusive band.
fn assert_every_fill_in_band(store: &Store, desk_id: &str) {
    let captured = fills(store, desk_id);
    let (low, high) = (decimal(LIMIT_DOWN), decimal(LIMIT_UP));
    assert!(
        captured
            .iter()
            .all(|(_, _, _, price)| (low..=high).contains(&decimal(price))),
        "every fill price is inside [{LIMIT_DOWN}, {LIMIT_UP}]: {captured:?}"
    );
}

// ---------------------------------------------------------------------------
// F4.1 and F4.2 — the upper limit
// ---------------------------------------------------------------------------

/// F4 1(a)–(d): quote → ladder → matching core, at the upper limit.
///
/// Zeroing the ask makes the sandbox refuse a new MARKET BUY with its own
/// reason, leaves a LIMIT BUY at the limit resting across repeated zero-ask
/// publishes, and leaves the SELL side working at the bid.
#[test]
fn zero_ask_blocks_buys_at_the_upper_limit() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f4-upper");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    // Inside the band, both sides sized: an ordinary synthesized book.
    tick(&node, ("1200.00", LOT), ("1200.00", LOT), at);
    let seeded = submit(
        &store,
        &registry,
        &desk_id,
        "f4-upper-seed",
        "BUY",
        LOT,
        None,
    )
    .expect("the seeding market buy is accepted");
    assert_eq!(seeded["status"], "FILLED", "{seeded}");

    // (b) An order resting from *before* the zeroing. A BUY whose limit is at
    //     1320.00 cannot be created here — against a sized 1200.00 ask it is
    //     marketable and fills at once — so the pre-existing resting order is a
    //     non-marketable BUY below the market, which must stay put when the
    //     market runs to the limit. The at-the-limit resting case is (c).
    let resting = submit(
        &store,
        &registry,
        &desk_id,
        "f4-upper-rest",
        "BUY",
        LOT,
        Some("1180.00"),
    )
    .expect("the non-marketable limit buy rests");
    assert_eq!(resting["status"], "ACCEPTED", "{resting}");

    // The market reaches the upper limit and MarketRig suppresses the ask.
    at += SECOND_NS;
    tick(&node, (LIMIT_UP, LOT), (LIMIT_UP, 0), at);

    // (a) A new MARKET BUY: the sandbox's own refusal, verbatim.
    let refused = submit(
        &store,
        &registry,
        &desk_id,
        "f4-upper-market",
        "BUY",
        LOT,
        None,
    )
    .expect_err("a market buy at the suppressed side is refused");
    let TradeError::Rejected(reason) = &refused else {
        panic!("the sandbox refused natively, not through a MarketRig check: {refused:?}");
    };
    assert_eq!(
        reason, "No market for 600519.XSHG",
        "`process_market_order`'s own pre-check reason, unrenamed"
    );
    assert_eq!(
        chain(&store, &desk_id, "f4-upper-market"),
        vec!["OrderInitialized", "OrderSubmitted", "OrderRejected"],
        "the native chain ends in OrderRejected, not a cancellation"
    );
    assert!(
        payload(&store, &desk_id, "f4-upper-market", "OrderRejected")
            .expect("the rejection payload")
            .contains("No market for 600519.XSHG"),
        "and the stored payload carries the sandbox's reason"
    );
    assert_eq!(status(&node, "f4-upper-market"), OrderStatus::Rejected);

    // (c) A new LIMIT BUY *at* the upper limit rests; it is not marketable
    //     because there is no ask at all.
    let rested = submit(
        &store,
        &registry,
        &desk_id,
        "f4-upper-limit",
        "BUY",
        LOT,
        Some(LIMIT_UP),
    )
    .expect("the limit buy at the upper limit is accepted");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    // (b) Repeated zero-ask publishes leave both resting BUYs untouched.
    for _ in 0..3 {
        at += SECOND_NS;
        tick(&node, (LIMIT_UP, LOT), (LIMIT_UP, 0), at);
    }
    assert_eq!(status(&node, "f4-upper-limit"), OrderStatus::Accepted);
    assert_eq!(status(&node, "f4-upper-rest"), OrderStatus::Accepted);
    assert_eq!(
        ladder(&store, &desk_id, "f4-upper-limit"),
        Vec::<(String, String)>::new(),
        "no BUY fill while the ask is suppressed"
    );

    // (d) The opposite direction still works at the upper limit.
    let sold = submit(
        &store,
        &registry,
        &desk_id,
        "f4-upper-sell",
        "SELL",
        LOT,
        None,
    )
    .expect("the market sell at the upper limit is accepted");
    assert_eq!(sold["status"], "FILLED", "{sold}");
    assert_eq!(
        ladder(&store, &desk_id, "f4-upper-sell"),
        vec![("100".to_owned(), LIMIT_UP.to_owned())],
        "the sell filled at the bid, which is the upper limit"
    );

    dump("F4.1 upper limit", &store, &desk_id);
    assert_every_fill_in_band(&store, &desk_id);
    registry.stop_all();
}

/// F4 2: restoring the side lets the resting BUY fill, in band.
#[test]
fn restoring_the_ask_fills_the_resting_buy() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f4-restore");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    tick(&node, (LIMIT_UP, LOT), (LIMIT_UP, 0), at);
    let rested = submit(
        &store,
        &registry,
        &desk_id,
        "f4-restore-1",
        "BUY",
        LOT,
        Some(LIMIT_UP),
    )
    .expect("the limit buy rests while the ask is suppressed");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    // The market backs off the limit and MarketRig restores the ask.
    at += SECOND_NS;
    tick(&node, (UNDER_UP, LOT), (UNDER_UP, LOT), at);
    within(10, "the restored ask fills the resting buy", || {
        status(&node, "f4-restore-1") == OrderStatus::Filled
    });
    assert_eq!(
        ladder(&store, &desk_id, "f4-restore-1"),
        vec![("100".to_owned(), LIMIT_UP.to_owned())],
        "a resting BUY fills as MAKER at its own limit price \
         (`determine_limit_price_and_volume`, mod.rs:4032-4060), not at the ask"
    );
    assert_every_fill_in_band(&store, &desk_id);
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// F4.3 — remainders and slippage
// ---------------------------------------------------------------------------

/// F4 3, the supported case: displayed liquidity is one lot, the order is three.
/// The L1 remainder slips exactly one tick, once, and lands on the upper limit —
/// still inside the inclusive band.
#[test]
fn market_remainder_slips_one_tick() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f4-slip");
    let desk_id = handle.desk_id().to_owned();

    tick(&node, (UNDER_UP, LOT), (UNDER_UP, LOT), CN_0935 + SECOND_NS);
    let filled = submit(&store, &registry, &desk_id, "f4-slip-1", "BUY", 300, None)
        .expect("the oversized market buy is accepted");
    assert_eq!(filled["status"], "FILLED", "{filled}");
    assert_eq!(
        ladder(&store, &desk_id, "f4-slip-1"),
        vec![
            ("100".to_owned(), UNDER_UP.to_owned()),
            ("200".to_owned(), LIMIT_UP.to_owned()),
        ],
        "displayed volume first, then the whole leaves quantity one tick worse"
    );
    dump("F4.3 market remainder", &store, &desk_id);
    assert_every_fill_in_band(&store, &desk_id);
    registry.stop_all();
}

/// F4 3, the decisive negative: the same order against a **sized** ask at the
/// upper limit slips one tick *past* it. This is why the mechanism is the
/// zeroing rather than any check on the order — nothing rejects this fill, and
/// no supported configuration prevents it.
#[test]
fn market_remainder_at_the_limit_price_leaves_the_band() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f4-slip-out");
    let desk_id = handle.desk_id().to_owned();

    tick(&node, (LIMIT_UP, LOT), (LIMIT_UP, LOT), CN_0935 + SECOND_NS);
    let filled = submit(
        &store,
        &registry,
        &desk_id,
        "f4-slip-out-1",
        "BUY",
        300,
        None,
    )
    .expect("the oversized market buy is accepted");
    assert_eq!(filled["status"], "FILLED", "{filled}");
    assert_eq!(
        ladder(&store, &desk_id, "f4-slip-out-1"),
        vec![
            ("100".to_owned(), LIMIT_UP.to_owned()),
            ("200".to_owned(), "1320.01".to_owned()),
        ],
        "the remainder lands one tick above limit_up"
    );
    dump("F4.3 market remainder at limit_up", &store, &desk_id);
    let out_of_band = fills(&store, &desk_id)
        .into_iter()
        .filter(|(_, _, _, price)| decimal(price) > decimal(LIMIT_UP))
        .count();
    assert_eq!(
        out_of_band, 1,
        "a sized ask at limit_up produces an out-of-band fill, so the daemon \
         must never publish one"
    );
    registry.stop_all();
}

/// F4 3, third case: the slip is MARKET-only. A LIMIT BUY larger than the
/// displayed lot fills what is there and rests with the remainder — the L1
/// remainder branch lists only `Market | MarketIfTouched | StopMarket |
/// TrailingStopMarket` (`mod.rs:4779-4789`).
#[test]
fn limit_remainder_rests_instead_of_slipping() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f4-limit-rem");
    let desk_id = handle.desk_id().to_owned();

    tick(&node, (UNDER_UP, LOT), (UNDER_UP, LOT), CN_0935 + SECOND_NS);
    let partial = submit(
        &store,
        &registry,
        &desk_id,
        "f4-limit-rem-1",
        "BUY",
        300,
        Some(LIMIT_UP),
    )
    .expect("the marketable limit buy is accepted");
    assert_eq!(partial["status"], "PARTIALLY_FILLED", "{partial}");
    assert_eq!(
        ladder(&store, &desk_id, "f4-limit-rem-1"),
        vec![("100".to_owned(), UNDER_UP.to_owned())],
        "one fill at the displayed ask; the remainder rests"
    );
    assert_eq!(
        status(&node, "f4-limit-rem-1"),
        OrderStatus::PartiallyFilled
    );
    assert_every_fill_in_band(&store, &desk_id);
    registry.stop_all();
}

/// F4 3, the config question: nothing on `SandboxExecutionClientConfig` reaches
/// the two knobs that would bound the slip. `price_protection_points` — the only
/// field `apply_fills` consults before slipping (`mod.rs:4801-4808`) — is not a
/// sandbox field at all and `to_matching_engine_config` never sets it, so it
/// stays `None`; and there is no `fill_model` field, only a `fee_model`.
#[test]
fn no_config_knob_bounds_the_slip() {
    let config = nautilus_sandbox::SandboxExecutionClientConfig::default();
    assert_eq!(
        config.to_matching_engine_config().price_protection_points,
        None,
        "the sandbox cannot ask the matching engine for price protection"
    );
    // The fields the sandbox does expose, and what each would (not) do here:
    // `reject_stop_orders`, `support_gtd_orders`, `support_contingent_orders`,
    // `use_position_ids`, `use_random_ids`, `use_reduce_only`, `frozen_account`,
    // `bar_execution`, `trade_execution`, `book_type`, `fee_model`. Only
    // `book_type` touches the slip, and only by leaving L1 — which the
    // synthesized one-level feed cannot fill.
    assert!(config.to_matching_engine_config().use_reduce_only);
    assert!(!config.to_matching_engine_config().liquidity_consumption);
}

// ---------------------------------------------------------------------------
// F4.4 — the lower limit, mirrored
// ---------------------------------------------------------------------------

/// F4 4: zeroing the bid at the lower limit.
#[test]
fn zero_bid_blocks_sells_at_the_lower_limit() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f4-lower");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    // Three lots of inventory, bought inside the band in one fill.
    tick(&node, ("1100.00", 300), ("1100.00", 300), at);
    let seeded = submit(
        &store,
        &registry,
        &desk_id,
        "f4-lower-seed",
        "BUY",
        300,
        None,
    )
    .expect("the seeding market buy is accepted");
    assert_eq!(seeded["status"], "FILLED", "{seeded}");

    // The market reaches the lower limit and MarketRig suppresses the bid.
    at += SECOND_NS;
    tick(&node, (LIMIT_DOWN, 0), (LIMIT_DOWN, LOT), at);

    let refused = submit(
        &store,
        &registry,
        &desk_id,
        "f4-lower-market",
        "SELL",
        LOT,
        None,
    )
    .expect_err("a market sell at the suppressed side is refused");
    let TradeError::Rejected(reason) = &refused else {
        panic!("the sandbox refused natively: {refused:?}");
    };
    assert_eq!(reason, "No market for 600519.XSHG");
    assert_eq!(
        chain(&store, &desk_id, "f4-lower-market"),
        vec!["OrderInitialized", "OrderSubmitted", "OrderRejected"],
    );

    let rested = submit(
        &store,
        &registry,
        &desk_id,
        "f4-lower-limit",
        "SELL",
        LOT,
        Some(LIMIT_DOWN),
    )
    .expect("the limit sell at the lower limit is accepted");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    for _ in 0..3 {
        at += SECOND_NS;
        tick(&node, (LIMIT_DOWN, 0), (LIMIT_DOWN, LOT), at);
    }
    assert_eq!(status(&node, "f4-lower-limit"), OrderStatus::Accepted);
    assert_eq!(
        ladder(&store, &desk_id, "f4-lower-limit"),
        Vec::<(String, String)>::new(),
        "no SELL fill while the bid is suppressed"
    );

    // The opposite direction still works at the lower limit.
    let bought = submit(
        &store,
        &registry,
        &desk_id,
        "f4-lower-buy",
        "BUY",
        LOT,
        None,
    )
    .expect("the market buy at the lower limit is accepted");
    assert_eq!(bought["status"], "FILLED", "{bought}");
    assert_eq!(
        ladder(&store, &desk_id, "f4-lower-buy"),
        vec![("100".to_owned(), LIMIT_DOWN.to_owned())],
    );

    // Restore the bid one tick above the limit: the resting SELL fills.
    at += SECOND_NS;
    tick(&node, (OVER_DOWN, LOT), (OVER_DOWN, LOT), at);
    within(10, "the restored bid fills the resting sell", || {
        status(&node, "f4-lower-limit") == OrderStatus::Filled
    });
    assert_eq!(
        ladder(&store, &desk_id, "f4-lower-limit"),
        vec![("100".to_owned(), LIMIT_DOWN.to_owned())],
        "the resting SELL fills as MAKER at its own limit price"
    );

    dump("F4.4 lower limit", &store, &desk_id);
    assert_every_fill_in_band(&store, &desk_id);
    registry.stop_all();
}

/// F4 4, the mirrored remainder: a MARKET SELL bigger than the displayed bid
/// slips one tick *down*, onto the lower limit, still in band.
#[test]
fn market_sell_remainder_slips_into_the_band() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f4-lower-rem");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    tick(&node, ("1100.00", 300), ("1100.00", 300), at);
    submit(
        &store,
        &registry,
        &desk_id,
        "f4-lower-rem-seed",
        "BUY",
        300,
        None,
    )
    .expect("the seeding market buy is accepted");

    at += SECOND_NS;
    tick(&node, (OVER_DOWN, LOT), (OVER_DOWN, LOT), at);
    let sold = submit(
        &store,
        &registry,
        &desk_id,
        "f4-lower-rem-1",
        "SELL",
        300,
        None,
    )
    .expect("the oversized market sell is accepted");
    assert_eq!(sold["status"], "FILLED", "{sold}");
    assert_eq!(
        ladder(&store, &desk_id, "f4-lower-rem-1"),
        vec![
            ("100".to_owned(), OVER_DOWN.to_owned()),
            ("200".to_owned(), LIMIT_DOWN.to_owned()),
        ],
        "displayed volume first, then the leaves one tick lower"
    );
    assert_every_fill_in_band(&store, &desk_id);
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// F4.5 — what the agent's book resource would show (report only)
// ---------------------------------------------------------------------------

/// F4 5: `feed::MarketState::book_all` derives both sizes from the catalog lot
/// whenever an observation exists (`crate::feed::BookTop::of`), and knows
/// nothing about the node's book. A suppressed side is therefore invisible on
/// the agent's book resource as it stands; §2.3's price-condition field is the
/// only place the condition could surface. No product change is made here.
#[test]
fn book_top_hides_a_suppressed_side() {
    let market = crate::feed::MarketState::new();
    market.accept(
        moutai(),
        &crate::feed::ChartQuote {
            price: decimal(LIMIT_UP),
            currency: "CNY".to_owned(),
            source_time_ns: CN_0935 as i64,
        },
        CN_0935 as i64,
    );
    let top = market
        .book_all(CN_0935 as i64)
        .into_iter()
        .find(|top| top.observation.instrument_id == "600519.XSHG")
        .expect("the CN row");
    assert_eq!(
        (
            top.bid_price.as_deref(),
            top.ask_price.as_deref(),
            top.bid_size.as_deref(),
            top.ask_size.as_deref(),
        ),
        (Some(LIMIT_UP), Some(LIMIT_UP), Some("100"), Some("100")),
        "both sides carry one lot even at the upper limit"
    );
}
