//! F8 item 3 — immediate MARKET execution at the usable last price, and whether
//! it can coexist on one native book with an AE-9 LIMIT that is resting but not
//! yet eligible.
//! (`sdd/features/a-share-engine/FEASIBILITY.md` §F8.3; feature SPEC §2.3–§2.5,
//! per AE-3 and AE-9.)
//!
//! Every test runs the real `LiveNode` and the real `SandboxExecutionClient` on
//! the controlled clock from [`crate::feasibility::clock`], `feed_base: None`,
//! every observation published by hand. No product code is changed.
//!
//! # Instrument and scale
//!
//! `000001.XSHE` (Ping An Bank), tick 0.01, lot 100, main board. The XSHE
//! account opens with 500,000 CNY (`crate::node::seed` splits the CN million
//! across the two CN venues). The FEASIBILITY example scales one-for-one at this
//! name's own price level: reference `prev = 10.00`, so the 10% band is
//! `[9.00, 11.00]`, the earlier last is 9.90, the usable last is 9.95, and the
//! resting BUY limit is 10.00.
//!
//! # What the daemon must publish for each model
//!
//! AE-9's two models want opposite things from the same L1 book:
//!
//! - A LIMIT must **rest** while ineligible. On this engine that means the side
//!   it would cross carries **zero size**: `handle_l1_add`
//!   (`nautilus-model-0.62.0/src/orderbook/ladder.rs:238-244`) clears the L1 side
//!   on a non-positive size, so `iterate` ends with `core.ask = None`
//!   (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:3821-3822`) and
//!   `is_limit_matched` is false (F4).
//! - A MARKET must fill the **full requested quantity at last**. On this engine
//!   that means a **sized** side at exactly the last price: `process_market_order`
//!   rejects when `core.ask`/`core.bid` is `None` (`mod.rs:3234-3242`), and
//!   `fill_market_order`'s L1 remainder branch (`mod.rs:4790-4846`) moves
//!   whatever the displayed size does not cover one tick past the last price,
//!   exactly once.
//!
//! # Findings
//!
//! **MARKET alone: PASS.** [`market_buy_fills_the_full_quantity_at_last`] and
//! [`market_sell_fills_the_full_quantity_at_last`] fill the whole quantity at
//! the last price in one native `OrderFilled` as `TAKER`, with no tick
//! slippage, provided the published size is at least the order quantity;
//! [`market_beyond_the_published_size_slips_one_tick`] is the counterexample.
//! Suppression and the inclusive boundary
//! ([`market_suppressed_and_boundary_priced_at_the_upper_limit`], and the lower
//! mirror), insufficient cash ([`market_buy_beyond_the_cash_balance`],
//! `OrderDenied` from the risk engine) and insufficient holdings
//! ([`market_sell_without_holdings_is_rejected`], `OrderRejected` from the
//! matching engine) all behave, with the reasons recorded verbatim.
//!
//! **Coexistence with a compatible-price ineligible LIMIT: BLOCKED.**
//! [`sized_ask_fills_the_ineligible_limit_before_the_market`] shows why on the
//! real node: the sized publish the MARKET model needs runs `iterate`, and
//! `iterate` fills every matched resting order on the instrument before any
//! order can be submitted. No ordering avoids it —
//! [`market_against_an_unsized_book_is_rejected`] (submit first: `No market`),
//! [`queueing_the_observation_and_the_order_together_still_fills_the_limit`]
//! (queue both), [`a_trade_tick_never_reaches_this_engine`] (the one data path
//! that updates the L1 book without iterating is dropped by the daemon's own
//! `trade_execution(false)`). A LIMIT whose price is **incompatible** with the
//! last coexists fine: [`an_incompatible_resting_limit_is_untouched_by_a_market`].
//!
//! The tested contract alternative is the assertion in
//! [`sized_ask_fills_the_ineligible_limit_before_the_market`]: a MARKET
//! observation also qualifies every compatible resting LIMIT. Its exact
//! consequence is that the LIMIT fills first, `MAKER`, at its own limit price,
//! full remaining quantity, and does **not** consume the synthesized depth the
//! MARKET then takes. The alternative that keeps AE-9's trigger rule intact is a
//! MarketRig pre-check, proven readable from one node-cache closure by
//! [`a_crossing_resting_limit_is_visible_from_the_node_cache`].

use std::sync::Arc;

use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::messages::DataEvent;
use nautilus_core::UnixNanos;
use nautilus_model::data::{Data, TradeTick};
use nautilus_model::enums::{AggressorSide, OrderSide, OrderStatus};
use nautilus_model::identifiers::{AccountId, ClientOrderId, InstrumentId, TradeId};
use nautilus_model::orders::Order;
use nautilus_model::types::{Price, Quantity};
use serde_json::Value;

use crate::catalog::Entry;
use crate::feasibility::clock::{
    CN_0935, ClockHandle, SECOND_NS, advance, controlled_registry, publish_book, stored_events,
};
use crate::node::{Node, Registry, within};
use crate::store::Store;
use crate::trade::{self, TradeError};

/// The band this file does its own arithmetic with: main board, `prev = 10.00`,
/// 10%, tick 0.01. The sandbox knows nothing about it.
const LIMIT_UP: &str = "11.00";
const LIMIT_DOWN: &str = "9.00";
/// The earlier last, the usable last, and the resting BUY limit — the
/// FEASIBILITY example at this instrument's price level.
const LAST_BEFORE: &str = "9.90";
const LAST: &str = "9.95";
const LIMIT_PX: &str = "10.00";
/// The instrument's lot.
const LOT: u32 = 100;

fn pingan() -> &'static Entry {
    crate::catalog::find("000001.XSHE").expect("the CN catalog entry")
}

fn pingan_id() -> InstrumentId {
    InstrumentId::from(pingan().instrument_id)
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A started desk on the controlled clock at 09:35 Asia/Shanghai, no feed, no
/// book yet.
fn desk(store: &Store, name: &'static str) -> (Registry, ClockHandle, Arc<Node>) {
    let (registry, handle) = controlled_registry(store, None, name, CN_0935);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    (registry, handle, node)
}

/// Advances the clock and publishes one observation with independent sides, then
/// waits for the node to take it. `at_ns` must increase across calls
/// (`process_quote_tick` skips a quote older than `book.ts_last`).
fn observe(node: &Node, bid: (&str, u32), ask: (&str, u32), at_ns: u64) {
    advance(node, at_ns);
    publish_book(node, pingan(), bid, ask, at_ns);
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

/// An AE-9 non-qualifying observation: a usable last price with **no** size on
/// either side, which is what leaves a compatible LIMIT resting.
fn unsized_at(node: &Node, price: &str, at_ns: u64) {
    observe(node, (price, 0), (price, 0), at_ns);
}

/// An AE-9 MARKET observation: `size` lots on both sides at the last price.
fn sized_at(node: &Node, price: &str, size: u32, at_ns: u64) {
    observe(node, (price, size), (price, size), at_ns);
}

/// The production submit path. `price` of `None` is a MARKET order.
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

/// Every captured fill for one order, oldest first: `(quantity, price,
/// commission, liquidity_side)`. All four are NautilusTrader's own `OrderFilled`
/// fields as `crate::trade::event_row` stored them; `liquidity_side` is read
/// back out of the stored payload, which is the event verbatim.
fn fills(
    store: &Store,
    desk_id: &str,
    client_order_id: &str,
) -> Vec<(String, String, String, String)> {
    let args = (desk_id.to_owned(), client_order_id.to_owned());
    store
        .call(move |conn| {
            conn.prepare(
                "SELECT quantity, price, commission, payload FROM fills \
                 WHERE desk_id = ?1 AND client_order_id = ?2 ORDER BY occurred_at_ns, id",
            )?
            .query_map([args.0, args.1], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .expect("the fills read")
        .into_iter()
        .map(|(quantity, price, commission, payload)| {
            let value: Value = serde_json::from_str(&payload).expect("the fill payload is JSON");
            let liquidity = value
                .as_object()
                .and_then(|o| o.values().next())
                .and_then(|body| body.get("liquidity_side"))
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned();
            (quantity, price, commission, liquidity)
        })
        .collect()
}

/// The desk's whole `fills` table, oldest first, as
/// `(client_order_id, quantity, price)`.
fn all_fills(store: &Store, desk_id: &str) -> Vec<(String, String, String)> {
    let desk = desk_id.to_owned();
    store
        .call(move |conn| {
            conn.prepare(
                "SELECT client_order_id, quantity, price FROM fills \
                 WHERE desk_id = ?1 ORDER BY occurred_at_ns, id",
            )?
            .query_map([desk], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
        })
        .expect("the fills read")
}

/// The XSHE account's CNY balance as `total|locked|free`, from the node cache's
/// own `AccountAny` — the sandbox's own accounting, never recomputed here.
fn balance(node: &Node, desk_id: &str) -> String {
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

/// Publishes a `TradeTick` straight into the node's data runner, the same way
/// [`publish_book`] publishes a quote. Used only to establish that the daemon's
/// configured sandbox never routes one to a matching engine.
fn publish_trade(node: &Node, price: &str, size: u32, at_ns: u64) {
    let instrument_id = pingan_id();
    let price = Price::from(price);
    let size = Quantity::from(size);
    node.call(move |_| {
        let tick = TradeTick::new(
            instrument_id,
            price,
            size,
            AggressorSide::NoAggressor,
            TradeId::new("f8-market-trade"),
            UnixNanos::from(at_ns),
            UnixNanos::from(at_ns),
        );
        get_data_event_sender()
            .send(DataEvent::Data(Data::Trade(tick)))
            .expect("the data runner takes the trade");
    })
    .expect("the node answers");
}

/// Prints the desk's whole captured native record. Evidence, not an assertion.
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
    eprintln!("--- {label}: fills {:?}", all_fills(store, desk_id));
}

/// Seeds `quantity` shares inside the band with one MARKET BUY, so a SELL test
/// has holdings. Returns the next free instant.
fn seed_position(
    store: &Store,
    registry: &Registry,
    node: &Node,
    desk_id: &str,
    quantity: u32,
    at_ns: u64,
) -> u64 {
    sized_at(node, LAST_BEFORE, quantity, at_ns);
    let filled = submit(store, registry, desk_id, "f8m-seed", "BUY", quantity, None)
        .expect("the seeding market buy is accepted");
    assert_eq!(filled["status"], "FILLED", "{filled}");
    at_ns + SECOND_NS
}

// ---------------------------------------------------------------------------
// Part 1 — MARKET alone
// ---------------------------------------------------------------------------

/// F8.3, MARKET alone, BUY: a published size at least the order quantity fills
/// the whole quantity at the last price, in one fill, as `TAKER`, with no tick
/// slippage. Also the orchestrator's ordering (ii): the same MARKET against the
/// unsized book that AE-9 publishes between qualifying observations is refused
/// natively.
#[test]
fn market_buy_fills_the_full_quantity_at_last() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-buy");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    // (ii) The AE-9 idle book: a usable last, no size. A MARKET is refused by
    // the sandbox's own pre-check, before any fill exists.
    unsized_at(&node, LAST_BEFORE, at);
    let refused = submit(&store, &registry, &desk_id, "f8m-buy-0", "BUY", 300, None)
        .expect_err("a market buy against an unsized book is refused");
    let TradeError::Rejected(reason) = &refused else {
        panic!("the sandbox refused natively, not through a MarketRig check: {refused:?}");
    };
    assert_eq!(reason, "No market for 000001.XSHE");

    let opening = balance(&node, &desk_id);
    assert_eq!(opening, "500000.00 CNY|0.00 CNY|500000.00 CNY");

    // (i)/(v) The MARKET observation: size exactly the order quantity at last.
    at += SECOND_NS;
    sized_at(&node, LAST, 300, at);
    let filled = submit(&store, &registry, &desk_id, "f8m-buy-1", "BUY", 300, None)
        .expect("the market buy is accepted");
    assert_eq!(filled["status"], "FILLED", "{filled}");
    assert_eq!(
        chain(&store, &desk_id, "f8m-buy-1"),
        vec!["OrderInitialized", "OrderSubmitted", "OrderFilled"],
        "no OrderAccepted: use_market_order_acks is off in the sandbox config"
    );
    let captured = fills(&store, &desk_id, "f8m-buy-1");
    assert_eq!(
        captured.len(),
        1,
        "exactly one native fill, no ladder: {captured:?}"
    );
    let (quantity, price, commission, liquidity) = captured[0].clone();
    assert_eq!((quantity.as_str(), price.as_str()), ("300", LAST));
    assert_eq!(liquidity, "TAKER");
    eprintln!("f8m-buy-1 fill: 300 @ {price} commission {commission} {liquidity}");
    eprintln!(
        "balance before {opening} after {}",
        balance(&node, &desk_id)
    );
    dump("F8.3 market buy", &store, &desk_id);
    registry.stop_all();
}

/// F8.3, MARKET alone, SELL: the mirror, against a sized bid at last.
#[test]
fn market_sell_fills_the_full_quantity_at_last() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-sell");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    at = seed_position(&store, &registry, &node, &desk_id, 300, at);
    let before = balance(&node, &desk_id);

    at += SECOND_NS;
    sized_at(&node, LAST, 300, at);
    let sold = submit(&store, &registry, &desk_id, "f8m-sell-1", "SELL", 300, None)
        .expect("the market sell is accepted");
    assert_eq!(sold["status"], "FILLED", "{sold}");
    let captured = fills(&store, &desk_id, "f8m-sell-1");
    assert_eq!(captured.len(), 1, "{captured:?}");
    let (quantity, price, commission, liquidity) = captured[0].clone();
    assert_eq!((quantity.as_str(), price.as_str()), ("300", LAST));
    assert_eq!(liquidity, "TAKER");
    assert_eq!(commission, "0.90");
    assert_eq!(
        balance(&node, &desk_id),
        "500013.21 CNY|0.00 CNY|500013.21 CNY",
        "497029.11 + 2985.00 - 0.90"
    );
    eprintln!("f8m-sell-1 fill: 300 @ {price} commission {commission} {liquidity}");
    eprintln!("balance before {before} after {}", balance(&node, &desk_id));
    dump("F8.3 market sell", &store, &desk_id);
    registry.stop_all();
}

/// F8.3, MARKET alone: a quantity larger than the published synthetic depth
/// slips one tick, once. This is why AE-9's MARKET model needs a published size
/// at least the order quantity — F4 proved the mechanism on another name; this
/// re-proves it at this price level, because it is the invariant that keeps the
/// fill at the last price.
#[test]
fn market_beyond_the_published_size_slips_one_tick() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-slip");
    let desk_id = handle.desk_id().to_owned();

    sized_at(&node, LAST, LOT, CN_0935 + SECOND_NS);
    let filled = submit(&store, &registry, &desk_id, "f8m-slip-1", "BUY", 300, None)
        .expect("the oversized market buy is accepted");
    assert_eq!(filled["status"], "FILLED", "{filled}");
    assert_eq!(
        fills(&store, &desk_id, "f8m-slip-1")
            .into_iter()
            .map(|(quantity, price, _, _)| (quantity, price))
            .collect::<Vec<_>>(),
        vec![
            ("100".to_owned(), LAST.to_owned()),
            ("200".to_owned(), "9.96".to_owned()),
        ],
        "the displayed size at last, then the whole remainder one tick worse"
    );
    registry.stop_all();
}

/// F8.3, MARKET alone: insufficient cash. Recorded verbatim, whatever the
/// pinned stack does — the daemon adds no check of its own here.
#[test]
fn market_buy_beyond_the_cash_balance() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-cash");
    let desk_id = handle.desk_id().to_owned();

    // 100,000 shares at 9.95 is 995,000 CNY against 500,000 CNY of free cash.
    sized_at(&node, LAST, 100_000, CN_0935 + SECOND_NS);
    let outcome = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-cash-1",
        "BUY",
        100_000,
        None,
    );
    eprintln!("f8m-cash-1 outcome: {outcome:?}");
    let TradeError::Rejected(reason) = outcome.expect_err("the buy is refused") else {
        panic!("the refusal is native");
    };
    assert_eq!(
        reason, "NOTIONAL_EXCEEDS_FREE_BALANCE: free=500000.00 CNY, notional=995000.00 CNY",
        "`OrderDeniedReason::NotionalExceedsFreeBalance` \
         (`nautilus-model-0.62.0/src/events/order/denied_reason.rs:224`), raised by the \
         risk engine before the order reaches the matching engine"
    );
    assert_eq!(
        chain(&store, &desk_id, "f8m-cash-1"),
        vec!["OrderInitialized", "OrderDenied"],
        "OrderDenied, not OrderRejected: this one never reached the sandbox"
    );
    assert!(all_fills(&store, &desk_id).is_empty());
    assert_eq!(
        balance(&node, &desk_id),
        "500000.00 CNY|0.00 CNY|500000.00 CNY",
        "nothing reserved, nothing spent"
    );
    eprintln!("balance after: {}", balance(&node, &desk_id));
    dump("F8.3 insufficient cash", &store, &desk_id);
    registry.stop_all();
}

/// F8.3, MARKET alone: insufficient holdings. The one native SELL guard is the
/// matching engine's cash-account short-sell check (`mod.rs:2879-2894`).
#[test]
fn market_sell_without_holdings_is_rejected() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-short");
    let desk_id = handle.desk_id().to_owned();

    sized_at(&node, LAST, 300, CN_0935 + SECOND_NS);
    let refused = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-short-1",
        "SELL",
        100,
        None,
    )
    .expect_err("a market sell with no holdings is refused");
    let TradeError::Rejected(reason) = &refused else {
        panic!("the sandbox refused natively: {refused:?}");
    };
    eprintln!("f8m-short-1 reason: {reason}");
    assert!(
        reason.starts_with("Short selling not permitted on a CASH account with position None"),
        "the matching engine's own short-sell reason: {reason}"
    );
    assert_eq!(
        chain(&store, &desk_id, "f8m-short-1"),
        vec!["OrderInitialized", "OrderSubmitted", "OrderRejected"],
    );
    assert!(all_fills(&store, &desk_id).is_empty());
    registry.stop_all();
}

/// F8.3, MARKET alone: direction suppression at the upper limit — the F4
/// zero-size side, at this price level, plus the inclusive-band case. The prior
/// observation is inside the band; the last then equals `limit_up`, MarketRig
/// suppresses the ask, and the BUY is refused natively while the SELL fills at
/// exactly 11.00 with no slip.
#[test]
fn market_suppressed_and_boundary_priced_at_the_upper_limit() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-upper");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    // Inside-band prior observation, which also gives the desk its holdings.
    at = seed_position(&store, &registry, &node, &desk_id, 300, at);

    // Last at the upper limit: the ask is suppressed, the bid keeps its size.
    at += SECOND_NS;
    observe(&node, (LIMIT_UP, 300), (LIMIT_UP, 0), at);

    let refused = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-upper-buy",
        "BUY",
        300,
        None,
    )
    .expect_err("a market buy at the upper limit is refused");
    let TradeError::Rejected(reason) = &refused else {
        panic!("the sandbox refused natively: {refused:?}");
    };
    assert_eq!(reason, "No market for 000001.XSHE");
    assert_eq!(
        chain(&store, &desk_id, "f8m-upper-buy"),
        vec!["OrderInitialized", "OrderSubmitted", "OrderRejected"],
    );

    let sold = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-upper-sell",
        "SELL",
        300,
        None,
    )
    .expect("the market sell at the upper limit is accepted");
    assert_eq!(sold["status"], "FILLED", "{sold}");
    assert_eq!(
        fills(&store, &desk_id, "f8m-upper-sell")
            .into_iter()
            .map(|(quantity, price, _, liquidity)| (quantity, price, liquidity))
            .collect::<Vec<_>>(),
        vec![("300".to_owned(), LIMIT_UP.to_owned(), "TAKER".to_owned())],
        "the whole quantity at exactly the inclusive upper boundary, no slip"
    );
    dump("F8.3 upper limit", &store, &desk_id);
    registry.stop_all();
}

/// F8.3, MARKET alone: the mirror at the lower limit. The SELL is refused; the
/// BUY fills at exactly 9.00.
#[test]
fn market_suppressed_and_boundary_priced_at_the_lower_limit() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-lower");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    at = seed_position(&store, &registry, &node, &desk_id, 300, at);

    // Last at the lower limit: the bid is suppressed, the ask keeps its size.
    at += SECOND_NS;
    observe(&node, (LIMIT_DOWN, 0), (LIMIT_DOWN, 300), at);

    let refused = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-lower-sell",
        "SELL",
        300,
        None,
    )
    .expect_err("a market sell at the lower limit is refused");
    let TradeError::Rejected(reason) = &refused else {
        panic!("the sandbox refused natively: {refused:?}");
    };
    assert_eq!(reason, "No market for 000001.XSHE");

    let bought = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-lower-buy",
        "BUY",
        300,
        None,
    )
    .expect("the market buy at the lower limit is accepted");
    assert_eq!(bought["status"], "FILLED", "{bought}");
    assert_eq!(
        fills(&store, &desk_id, "f8m-lower-buy")
            .into_iter()
            .map(|(quantity, price, _, liquidity)| (quantity, price, liquidity))
            .collect::<Vec<_>>(),
        vec![("300".to_owned(), LIMIT_DOWN.to_owned(), "TAKER".to_owned())],
        "the whole quantity at exactly the inclusive lower boundary, no slip"
    );
    dump("F8.3 lower limit", &store, &desk_id);
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// Part 2 — coexistence with an ineligible LIMIT
// ---------------------------------------------------------------------------

/// F8.3, coexistence, the blocker **and** the tested alternative.
///
/// A BUY LIMIT 100 @ 10.00 rests while the AE-9 book is unsized at 9.90 — it is
/// ineligible: compatible price, no qualifying volume increase yet. Publishing
/// the sized ask at 9.95 that the MARKET model needs runs `iterate`
/// (`mod.rs:1615`), which matches every resting order on the instrument
/// (`mod.rs:3701-3723`) **before** any order can be submitted, so the resting
/// LIMIT fills first, as `MAKER`, at its own limit price.
///
/// This is the exact observable consequence of the smallest contract
/// alternative — "a MARKET observation also qualifies every compatible resting
/// LIMIT, regardless of volume" — so it is asserted here rather than proposed.
#[test]
fn sized_ask_fills_the_ineligible_limit_before_the_market() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-both");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    // The AE-9 idle book, and the ineligible LIMIT resting on it.
    unsized_at(&node, LAST_BEFORE, at);
    let rested = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-both-limit",
        "BUY",
        LOT,
        Some(LIMIT_PX),
    )
    .expect("the compatible limit buy rests on the unsized book");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");
    let before = balance(&node, &desk_id);

    // The MARKET observation. Nothing else happens: no order is submitted.
    at += SECOND_NS;
    sized_at(&node, LAST, 300, at);
    within(10, "the sized ask fills the resting limit", || {
        status(&node, "f8m-both-limit") == OrderStatus::Filled
    });
    assert_eq!(
        fills(&store, &desk_id, "f8m-both-limit")
            .into_iter()
            .map(|(quantity, price, _, liquidity)| (quantity, price, liquidity))
            .collect::<Vec<_>>(),
        vec![("100".to_owned(), LIMIT_PX.to_owned(), "MAKER".to_owned())],
        "the ineligible LIMIT filled incidentally, at its own limit price"
    );

    // The MARKET the publish was for, submitted after the incidental fill.
    let filled = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-both-market",
        "BUY",
        300,
        None,
    )
    .expect("the market buy is accepted");
    assert_eq!(filled["status"], "FILLED", "{filled}");
    assert_eq!(
        fills(&store, &desk_id, "f8m-both-market")
            .into_iter()
            .map(|(quantity, price, _, liquidity)| (quantity, price, liquidity))
            .collect::<Vec<_>>(),
        vec![("300".to_owned(), LAST.to_owned(), "TAKER".to_owned())],
        "the incidental LIMIT fill does not consume the synthesized L1 depth"
    );
    eprintln!("balance before {before} after {}", balance(&node, &desk_id));
    dump("F8.3 coexistence", &store, &desk_id);
    registry.stop_all();
}

/// F8.3, coexistence, ordering (ii): submitting the MARKET first, against the
/// unsized book, leaves the resting LIMIT alone but produces no fill either.
#[test]
fn market_against_an_unsized_book_is_rejected() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-first");
    let desk_id = handle.desk_id().to_owned();

    unsized_at(&node, LAST_BEFORE, CN_0935 + SECOND_NS);
    let rested = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-first-limit",
        "BUY",
        LOT,
        Some(LIMIT_PX),
    )
    .expect("the limit rests");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    let refused = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-first-market",
        "BUY",
        300,
        None,
    )
    .expect_err("no market");
    assert!(
        matches!(&refused, TradeError::Rejected(reason) if reason == "No market for 000001.XSHE")
    );
    assert_eq!(status(&node, "f8m-first-limit"), OrderStatus::Accepted);
    assert!(all_fills(&store, &desk_id).is_empty());
    registry.stop_all();
}

/// F8.3, coexistence, ordering (iii): queueing the sized observation and the
/// MARKET together does not isolate them either. `crate::node::Node::call` (the
/// production submit path) and the data runner both hand work to the node
/// thread, and in every run here the queued quote was processed first: it ran
/// `iterate`, filled the resting LIMIT, and only then did the MARKET fill at
/// last. F3's `biased` exec-before-data priority
/// (`nautilus-live-0.62.0/src/runner.rs:579-606`) applies to the runner's own
/// `exec_cmd_rx`, not to a MarketRig node job, so this ordering is a race, not a
/// seam. Whichever side wins, the resting LIMIT is filled by the publish, so the
/// assertion is on the LIMIT.
#[test]
fn queueing_the_observation_and_the_order_together_still_fills_the_limit() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-order");
    let desk_id = handle.desk_id().to_owned();
    let at = CN_0935 + SECOND_NS;

    unsized_at(&node, LAST_BEFORE, at);
    let rested = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-order-limit",
        "BUY",
        LOT,
        Some(LIMIT_PX),
    )
    .expect("the limit rests");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    // Queue the sized observation without waiting for the node to take it, then
    // submit immediately.
    advance(&node, at + SECOND_NS);
    publish_book(&node, pingan(), (LAST, 300), (LAST, 300), at + SECOND_NS);
    let outcome = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-order-market",
        "BUY",
        300,
        None,
    );
    eprintln!("f8m-order-market outcome: {outcome:?}");
    within(10, "the queued quote fills the resting limit", || {
        status(&node, "f8m-order-limit") == OrderStatus::Filled
    });
    assert_eq!(
        fills(&store, &desk_id, "f8m-order-limit")
            .into_iter()
            .map(|(quantity, price, _, liquidity)| (quantity, price, liquidity))
            .collect::<Vec<_>>(),
        vec![("100".to_owned(), LIMIT_PX.to_owned(), "MAKER".to_owned())],
        "no submission ordering saves the ineligible LIMIT from the publish"
    );
    dump("F8.3 queued together", &store, &desk_id);
    registry.stop_all();
}

/// F8.3, coexistence, ordering (iv): a `TradeTick` would be the one data path
/// that updates the L1 book **without** iterating — `process_trade_tick` returns
/// early when `trade_execution` is false
/// (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:2133-2147`) — but the
/// daemon's sandbox client drops the trade before the engine ever sees it
/// (`nautilus-sandbox-0.62.0/src/execution.rs:230-233`, `crate::node`'s
/// `sandbox_config` sets `trade_execution(false)`). The book stays unsized, the
/// MARKET stays refused, the LIMIT stays resting.
#[test]
fn a_trade_tick_never_reaches_this_engine() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-trade");
    let desk_id = handle.desk_id().to_owned();
    let at = CN_0935 + SECOND_NS;

    unsized_at(&node, LAST_BEFORE, at);
    let rested = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-trade-limit",
        "BUY",
        LOT,
        Some(LIMIT_PX),
    )
    .expect("the limit rests");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    advance(&node, at + SECOND_NS);
    publish_trade(&node, LAST, 300, at + SECOND_NS);
    within(10, "the trade reaches the node cache", || {
        node.call(move |context| context.cache.borrow().trade(&pingan_id()).is_some())
            .unwrap()
    });

    let refused = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-trade-market",
        "BUY",
        300,
        None,
    )
    .expect_err("the trade tick gave the engine no book");
    assert!(
        matches!(&refused, TradeError::Rejected(reason) if reason == "No market for 000001.XSHE")
    );
    assert_eq!(status(&node, "f8m-trade-limit"), OrderStatus::Accepted);
    assert!(all_fills(&store, &desk_id).is_empty());
    registry.stop_all();
}

/// F8.3, coexistence, the case that does work: a resting LIMIT whose price is
/// **incompatible** with the last (BUY limit below the last) is not crossed by
/// the sized ask, so the MARKET fills at last and the LIMIT is untouched. Only
/// the compatible-price ineligible LIMIT is blocked.
#[test]
fn an_incompatible_resting_limit_is_untouched_by_a_market() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-incompat");
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    unsized_at(&node, LAST_BEFORE, at);
    let rested = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-incompat-limit",
        "BUY",
        LOT,
        Some("9.50"),
    )
    .expect("the incompatible limit rests");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");

    at += SECOND_NS;
    sized_at(&node, LAST, 300, at);
    let filled = submit(
        &store,
        &registry,
        &desk_id,
        "f8m-incompat-market",
        "BUY",
        300,
        None,
    )
    .expect("the market buy is accepted");
    assert_eq!(filled["status"], "FILLED", "{filled}");
    assert_eq!(
        fills(&store, &desk_id, "f8m-incompat-market")
            .into_iter()
            .map(|(quantity, price, _, _)| (quantity, price))
            .collect::<Vec<_>>(),
        vec![("300".to_owned(), LAST.to_owned())],
    );
    assert_eq!(status(&node, "f8m-incompat-limit"), OrderStatus::Accepted);
    assert_eq!(
        fills(&store, &desk_id, "f8m-incompat-limit").len(),
        0,
        "the ask at 9.95 never crosses a 9.50 buy limit"
    );
    registry.stop_all();
}

/// F8.3, the other alternative, and why it is bigger: "refuse the MARKET while a
/// compatible resting LIMIT would cross" is a MarketRig check, not a native one.
/// The state it needs is in the node cache, readable in the same `Node::call`
/// closure F6 established for check-and-place — so it is implementable, but it
/// is a new daemon-owned refusal, not an engine outcome.
#[test]
fn a_crossing_resting_limit_is_visible_from_the_node_cache() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "f8m-detect");
    let desk_id = handle.desk_id().to_owned();

    unsized_at(&node, LAST_BEFORE, CN_0935 + SECOND_NS);
    for (action, price) in [("f8m-detect-a", LIMIT_PX), ("f8m-detect-b", "9.50")] {
        let rested = submit(&store, &registry, &desk_id, action, "BUY", LOT, Some(price))
            .expect("the limit rests");
        assert_eq!(rested["status"], "ACCEPTED", "{rested}");
    }

    let last = Price::from(LAST);
    let crossing: Vec<String> = node
        .call(move |context| {
            context
                .cache
                .borrow()
                .orders(None, Some(&pingan_id()), None, None, Some(OrderSide::Buy))
                .into_iter()
                .filter(|order| !order.is_closed())
                .filter(|order| order.price().is_some_and(|limit| limit >= last))
                .map(|order| order.client_order_id().to_string())
                .collect()
        })
        .expect("the node answers");
    assert_eq!(
        crossing,
        vec!["f8m-detect-a".to_owned()],
        "only the compatible-price resting limit would be crossed by a sized ask at 9.95"
    );
    registry.stop_all();
}
