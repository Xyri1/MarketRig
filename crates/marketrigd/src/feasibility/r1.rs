//! R1 — do restored reservations release correctly?
//! (`sdd/features/a-share-engine/FEASIBILITY.md`, "Follow-up handoff → R1".)
//!
//! F3 found that a resting CN buy which survives a restart and is then canceled
//! leaves its cash locked forever: the account comes back from `book_snapshots`
//! with `AccountBalance.locked` intact but with an **empty** per-instrument lock
//! map, because `CashAccount::balances_locked` is `#[serde(skip, default)]`
//! ("transient, not persisted",
//! `nautilus-model-0.62.0/src/accounts/cash.rs:66-71`). Every release path ends
//! in `base::clear_balance_locked` / `base::recalculate_balance`
//! (`nautilus-model-0.62.0/src/accounts/base.rs:425-471`), which derive `locked`
//! and `free` **only** from that map — so with the map empty they recompute
//! nothing and the stale figures stand.
//!
//! # The route taken
//!
//! `Portfolio::initialize_orders` (`nautilus-portfolio-0.62.0/src/portfolio.rs:1620`)
//! is NautilusTrader's own answer for exactly this state: it walks
//! `cache.iter_client_order_ids_open`, groups the open orders per instrument and
//! account, and hands each group to `AccountsManager::update_orders`
//! (`manager.rs:147`), which for a `CashAccount` runs `update_balance_locked`
//! (`manager.rs:377-483`) — `Account::calculate_balance_locked` per order,
//! summed per currency, then `clear_balance_locked` + `update_balance_locked` —
//! and writes the account back with `Cache::update_account`
//! (`nautilus-common-0.62.0/src/cache/mod.rs:5013`). Every figure is computed by
//! NautilusTrader; MarketRig supplies nothing.
//!
//! Its only upstream caller is `LiveNode::perform_startup_reconciliation`
//! (`nautilus-live-0.62.0/src/node/mod.rs:863`), which runs it after the cache
//! has been repopulated from the venue. MarketRig sets
//! `with_reconciliation(false)` (`crate::node::build`) and restores from its own
//! `book_snapshots` instead (per D64), so that call never happens — which is
//! precisely why the map is never rebuilt. The fix is one line at the end of
//! `crate::trade::apply`, in the same position upstream uses.
//!
//! Routes considered and not taken:
//!
//! - **(a) per-order `calculate_balance_locked` + `update_balance_locked` in
//!   `apply`.** Both are public (`nautilus-model-0.62.0/src/accounts/cash.rs:89`,
//!   and `Account::calculate_balance_locked` on the trait), so it works — but it
//!   makes MarketRig own the aggregation `AccountsManager::update_balance_locked`
//!   already owns: grouping by account and instrument, skipping `reduce_only`
//!   and price-less orders, the trigger-price fallback, the per-currency sum, the
//!   base-currency xrate, and the `clear` -> `update` ordering the crate's own
//!   module docs require. That is MarketRig arithmetic on money, for no gain.
//! - **(c) re-emitting `OrderAccepted` for each restored order.** It fabricates
//!   native events; `crate::trade::capture_order` stores the node's own chain, so
//!   the fabricated event would land in `order_events` and in
//!   `history_orders`. Rejected.
//! - **(d) restoring the account before the orders.** Already the case:
//!   `apply` adds accounts, then positions, then orders. Ordering is not the
//!   cause. The sandbox client does seed its own account first — `connect`
//!   sends an unreported `AccountState` from `starting_balances`
//!   (`nautilus-sandbox-0.62.0/src/execution.rs:994-1001`) — and `apply`'s
//!   `Cache::add_account` then replaces it with the snapshot's. Neither step
//!   can rebuild `balances_locked`, because no `OrderAccepted` is published for
//!   a re-handed order (`crate::trade::hand_to_node` announces nothing and
//!   NautilusTrader's invalid-transition guard drops the repeats), so the
//!   portfolio's `update_order` never runs for it.
//!
//! # What each test establishes
//!
//! - [`restored_cancel_strands_the_reservation`] — the retained reproduction of
//!   the F3 defect, on the real node, with the repair undone by putting the
//!   snapshot's own account back into the cache. It asserts the defect.
//! - [`s1_single_restored_order_releases_and_stays_released`] — S1.
//! - [`s2_partial_fill_releases_only_the_unfilled_reservation`] — S2.
//! - [`s3_two_restored_orders_release_independently`] — S3.
//! - [`s4_session_end_recovery_releases_with_no_data`] — S4.
//!
//! Platform: macOS only; no Windows run.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nautilus_common::messages::execution::{CancelOrder, TradingCommand};
use nautilus_common::msgbus::{self, MessagingSwitchboard};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::accounts::AccountAny;
use nautilus_model::enums::OrderStatus;
use nautilus_model::identifiers::{AccountId, ClientOrderId, InstrumentId, StrategyId};
use nautilus_model::orders::Order;
use nautilus_model::types::Price;
use serde_json::Value;

use crate::catalog::Entry;
use crate::feasibility::clock::{
    self, CN_0935, CN_1457, ClockHandle, SECOND_NS, advance, controlled_registry, stored_events,
};
use crate::feed::{self, FeedBase, MarketState};
use crate::node::{Node, Registry, within};
use crate::store::Store;
use crate::trade;

/// `crate::trade::STRATEGY`, pinned by `f2::strategy_matches_production`.
const STRATEGY: &str = "MARKETRIG-001";

/// 15:30 Asia/Shanghai — the same trading day, past the §5.1 14:57 deadline.
const CN_1530: u64 = CN_0935 + 21_300 * SECOND_NS;

/// The XSHG account's opening cash: `crate::node::seed` divides the market's
/// 1,000,000 CNY across the two CN venues.
const OPENING: &str = "500000.00 CNY|0.00 CNY|500000.00 CNY";

fn moutai() -> &'static Entry {
    crate::catalog::find("600519.XSHG").expect("the CN catalog entry")
}

fn moutai_id() -> InstrumentId {
    InstrumentId::from(moutai().instrument_id)
}

// ---------------------------------------------------------------------------
// Harness — the same shape `f3.rs` uses, kept local because its helpers are
// private to that module.
// ---------------------------------------------------------------------------

/// A controlled desk at 09:35 with one non-crossing quote in its book.
fn open_desk(store: &Store, name: &'static str) -> (Registry, ClockHandle, Arc<Node>) {
    let (registry, handle) = controlled_registry(store, None, name, CN_0935);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    clock::publish_quote(&node, moutai(), "1700.00", 100, CN_0935);
    await_ask(&node, "1700.00");
    (registry, handle, node)
}

/// A second registry over the same store: the restart. `Registry::ensure` runs
/// `crate::trade::restore`, and therefore `apply`, before it returns.
fn restart(store: &Store, feed: Option<FeedBase>, desk_id: &str) -> (Registry, Arc<Node>) {
    let registry = Registry::new(store.clone(), Arc::new(MarketState::new()), feed);
    let node = registry
        .ensure(desk_id)
        .expect("the node restores and starts");
    (registry, node)
}

/// Rests one CN limit buy through the production submit path.
fn rest_buy(
    store: &Store,
    registry: &Registry,
    desk_id: &str,
    action_id: &str,
    quantity: u32,
    price: &str,
) {
    let body = format!(
        r#"{{"action_id":"{action_id}","instrument_id":"600519.XSHG",
            "side":"BUY","type":"LIMIT","quantity":"{quantity}","price":"{price}"}}"#
    );
    let (record, _) = trade::submit(store, registry, desk_id, &body, &trade::Source::Session)
        .expect("the limit buy is accepted");
    assert_eq!(
        record.outcome.clone().unwrap()["status"],
        "ACCEPTED",
        "{action_id} must rest, not fill"
    );
}

/// Waits for the CN instrument's cached quote to read `price` on the ask.
fn await_ask(node: &Node, price: &'static str) {
    within(10, "the quote reaches the node", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .quote(&moutai_id())
                .map(|q| q.ask_price)
                == Some(Price::from(price))
        })
        .unwrap()
    });
}

/// The XSHG account's CNY balance as `total|locked|free`, read from the node
/// cache's own `AccountAny` — the sandbox's reservation accounting.
fn balance_cny(node: &Node, desk_id: &str) -> String {
    let account_id = AccountId::from(format!("XSHG-{desk_id}").as_str());
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

/// Is the account's per-(instrument, currency) lock map populated? This is the
/// state `book_snapshots` cannot carry.
fn lock_map_len(node: &Node, desk_id: &str) -> usize {
    let account_id = AccountId::from(format!("XSHG-{desk_id}").as_str());
    node.call(move |context| {
        match &*context
            .cache
            .borrow()
            .account(&account_id)
            .expect("the venue account")
        {
            AccountAny::Cash(cash) => cash.balances_locked.len(),
            other => panic!("the sandbox account is a cash account, got {other:?}"),
        }
    })
    .expect("the node answers")
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

/// The order's venue order id, as the sandbox assigned it.
fn venue_order_id(node: &Node, client_order_id: &str) -> String {
    let id = ClientOrderId::from(client_order_id);
    node.call(move |context| {
        context
            .cache
            .borrow()
            .order(&id)
            .and_then(|order| order.venue_order_id())
            .expect("the accepted order carries a venue order id")
            .to_string()
    })
    .expect("the node answers")
}

/// The desk's captured event kinds for one client order id, oldest first.
fn kinds(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<String> {
    stored_events(store, desk_id)
        .into_iter()
        .filter(|(id, _, _)| id == client_order_id)
        .map(|(_, kind, _)| kind)
        .collect()
}

fn fills(store: &Store) -> i64 {
    store
        .call(|conn| conn.query_row("SELECT count(*) FROM fills", [], |r| r.get(0)))
        .expect("the fills count")
}

/// The desk's single `book_snapshots` payload.
fn snapshot_payload(store: &Store, desk_id: &str) -> Value {
    let desk = desk_id.to_owned();
    let payload: String = store
        .call(move |conn| {
            conn.query_row(
                "SELECT payload FROM book_snapshots WHERE desk_id = ?1",
                [desk],
                |r| r.get(0),
            )
        })
        .expect("the snapshot row");
    serde_json::from_str(&payload).expect("the snapshot is JSON")
}

/// The same `TradingCommand::CancelOrder` `crate::trade::cancel` builds, stamped
/// at `at_ns` instead of `store::now_ns()` so the terminal event carries the
/// controlled instant. This is the shape a restore job would use.
fn cancel_at(node: &Node, client_order_id: &'static str, at_ns: u64) {
    node.call(move |context| {
        let target = ClientOrderId::from(client_order_id);
        let (instrument_id, venue_order_id) = {
            let cache = context.cache.borrow();
            let order = cache.order(&target).expect("the restored order is cached");
            (order.instrument_id(), order.venue_order_id())
        };
        msgbus::send_trading_command(
            MessagingSwitchboard::exec_engine_queue_execute(),
            TradingCommand::CancelOrder(CancelOrder::new(
                context.trader_id,
                None,
                StrategyId::new(STRATEGY),
                instrument_id,
                target,
                venue_order_id,
                UUID4::new(),
                UnixNanos::from(at_ns),
                None,
                None,
            )),
        );
    })
    .expect("the node answers");
}

/// Cancels through the production path and waits for the terminal event.
fn cancel(store: &Store, registry: &Registry, desk_id: &str, client_order_id: &str, action: &str) {
    trade::cancel(
        store,
        registry,
        desk_id,
        client_order_id,
        &format!(r#"{{"action_id":"{action}"}}"#),
        &trade::Source::Session,
    )
    .unwrap_or_else(|e| panic!("{client_order_id} cancels: {e:?}"));
}

/// A stand-in chart feed that answers nothing until its flag is set — F3's
/// `held_server`, which is what "hold the first publish until restoration has
/// decided" looks like on the real poller.
fn held_server(body: String) -> (String, Arc<AtomicBool>) {
    use std::io::{BufRead, BufReader, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let released = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&released);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let (body, flag) = (body.clone(), Arc::clone(&flag));
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                    if line == "\r\n" {
                        break;
                    }
                    line.clear();
                }
                while !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            });
        }
    });
    (base, released)
}

// ---------------------------------------------------------------------------
// The retained reproduction
// ---------------------------------------------------------------------------

/// **Documents the defect.** F3's finding, kept runnable after the repair.
///
/// The day-D order rests, the desk restarts, and then the account the snapshot
/// itself carries is put back into the node cache through the public
/// `Cache::update_account` — the exact state `crate::trade::apply` left behind
/// before it called `Portfolio::initialize_orders`, deserialized by the same
/// serde path `crate::trade::restore` uses, with no figure invented here. The
/// real `CancelOrder` then terminates the order with a native `OrderCanceled`
/// and **does not release the cash**.
///
/// The two assertions that name the cause: the serialized account carries no
/// `balances_locked` key at all, and the deserialized account's map is empty
/// while its `AccountBalance.locked` still reads the day-D figure.
#[test]
fn restored_cancel_strands_the_reservation() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "r1-defect");
    let desk_id = handle.desk_id().to_owned();

    assert_eq!(balance_cny(&node, &desk_id), OPENING);
    rest_buy(&store, &registry, &desk_id, "r1-defect-1", 100, "1600.00");
    let rested = balance_cny(&node, &desk_id);
    assert_eq!(rested, "500000.00 CNY|160000.00 CNY|340000.00 CNY");
    assert_eq!(lock_map_len(&node, &desk_id), 1, "the node took the lock");
    let venue_id = venue_order_id(&node, "r1-defect-1");
    assert!(!venue_id.is_empty(), "the sandbox assigned {venue_id}");
    registry.stop_all();

    // The snapshot the restart reads: `CashAccount::balances_locked` is
    // `#[serde(skip)]`, so the key is not even present.
    let payload = snapshot_payload(&store, &desk_id);
    let accounts = payload["accounts"].to_string();
    assert!(accounts.contains(&format!("XSHG-{desk_id}")), "{accounts}");
    assert!(
        !accounts.contains("balances_locked"),
        "the pinned crate does not serialize the lock map: {accounts}"
    );

    let (registry, node) = restart(&store, None, &desk_id);
    advance(&node, CN_1530);
    assert_eq!(status(&node, "r1-defect-1"), OrderStatus::Accepted);
    assert_eq!(
        venue_order_id(&node, "r1-defect-1"),
        venue_id,
        "the restored order kept its venue order id"
    );

    // Undo the repair: put the snapshot's own account back, exactly as `apply`
    // left it before `initialize_orders` was added.
    let stale: Vec<AccountAny> =
        serde_json::from_value(payload["accounts"].clone()).expect("the snapshot's accounts");
    let stale_xshg = stale
        .into_iter()
        .find(|account| account.id() == AccountId::from(format!("XSHG-{desk_id}").as_str()))
        .expect("the XSHG account");
    match &stale_xshg {
        AccountAny::Cash(cash) => assert!(
            cash.balances_locked.is_empty(),
            "the deserialized account lost its lock map"
        ),
        other => panic!("expected a cash account, got {other:?}"),
    }
    node.call(move |context| {
        context
            .cache
            .borrow_mut()
            .update_account(&stale_xshg)
            .expect("the account goes back into the cache")
    })
    .expect("the node answers");
    assert_eq!(
        balance_cny(&node, &desk_id),
        rested,
        "the stale account still shows day D's reservation"
    );
    assert_eq!(
        lock_map_len(&node, &desk_id),
        0,
        "…with nothing behind it to recompute from"
    );

    cancel(&store, &registry, &desk_id, "r1-defect-1", "r1-defect-x");
    assert_eq!(status(&node, "r1-defect-1"), OrderStatus::Canceled);
    assert_eq!(
        kinds(&store, &desk_id, "r1-defect-1"),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ],
        "one native terminal event"
    );

    // The defect, asserted.
    assert_eq!(
        balance_cny(&node, &desk_id),
        rested,
        "DEFECT: `OrderCanceled` released nothing — `clear_balance_locked` \
         iterated an empty `balances_locked` map, so `recalculate_balance` was \
         never reached for CNY"
    );
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// S1 — one restored order, cancel, restart again
// ---------------------------------------------------------------------------

/// S1: rest an unfilled BUY, capture `total|locked|free` and the venue order id,
/// restart, cancel → `OrderCanceled`; free is back to the pre-order free, locked
/// is zero, total never moved. Restart once more: still correct, no duplicate
/// terminal event, one `history_orders` chain.
#[test]
fn s1_single_restored_order_releases_and_stays_released() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "r1-s1");
    let desk_id = handle.desk_id().to_owned();

    let free_before_order = balance_cny(&node, &desk_id);
    assert_eq!(free_before_order, OPENING);
    rest_buy(&store, &registry, &desk_id, "r1-s1-1", 100, "1600.00");
    let rested = balance_cny(&node, &desk_id);
    assert_eq!(rested, "500000.00 CNY|160000.00 CNY|340000.00 CNY");
    let venue_id = venue_order_id(&node, "r1-s1-1");
    registry.stop_all();

    // Restart 1: restoration rebuilt the lock map, and the visible balance is
    // byte-identical to the one the node held before the stop.
    let (registry, node) = restart(&store, None, &desk_id);
    advance(&node, CN_1530);
    assert_eq!(status(&node, "r1-s1-1"), OrderStatus::Accepted);
    assert_eq!(venue_order_id(&node, "r1-s1-1"), venue_id);
    assert_eq!(
        balance_cny(&node, &desk_id),
        rested,
        "restoration reproduced the node's own figure"
    );
    assert_eq!(
        lock_map_len(&node, &desk_id),
        1,
        "…and rebuilt the map behind it"
    );

    cancel(&store, &registry, &desk_id, "r1-s1-1", "r1-s1-cancel");
    assert_eq!(status(&node, "r1-s1-1"), OrderStatus::Canceled);
    assert_eq!(
        balance_cny(&node, &desk_id),
        free_before_order,
        "free is back to the pre-order free, locked is zero, total unchanged"
    );
    let after_cancel = kinds(&store, &desk_id, "r1-s1-1");
    assert_eq!(
        after_cancel,
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ]
    );
    assert!(trade::open_orders(&node).unwrap().is_empty());
    assert_eq!(fills(&store), 0);
    registry.stop_all();

    // Restart 2: a closed order is not in `orders_open`, so the snapshot carries
    // nothing to lock and nothing to replay.
    let (registry, node) = restart(&store, None, &desk_id);
    advance(&node, CN_1530);
    assert_eq!(
        balance_cny(&node, &desk_id),
        free_before_order,
        "the released state survives the next restart"
    );
    assert_eq!(lock_map_len(&node, &desk_id), 0);
    assert_eq!(
        kinds(&store, &desk_id, "r1-s1-1"),
        after_cancel,
        "no duplicate terminal event"
    );
    let history = trade::history_orders(&store, &desk_id).expect("the history reads");
    assert_eq!(history.len(), 1, "the chain appears once: {history:?}");
    assert_eq!(history[0]["client_order_id"], "r1-s1-1");
    assert_eq!(history[0]["status"], "CANCELED");
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// S2 — partial fill, restart, cancel the remainder
// ---------------------------------------------------------------------------

/// S2: fill part of a BUY against a book whose ask is smaller than the order,
/// persist, restart, cancel the remainder. Only the reservation is released; the
/// position, the executed cost and the fee stay exactly as NautilusTrader
/// produced them. Total cash is compared **immediately before and after the
/// cancel**, not against the pre-fill total.
#[test]
fn s2_partial_fill_releases_only_the_unfilled_reservation() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "r1-s2");
    let desk_id = handle.desk_id().to_owned();

    rest_buy(&store, &registry, &desk_id, "r1-s2-1", 100, "1600.00");
    assert_eq!(
        balance_cny(&node, &desk_id),
        "500000.00 CNY|160000.00 CNY|340000.00 CNY"
    );

    // An ask of 60 against a resting 100: `determine_limit_price_and_volume`
    // sizes the fill from the book level
    // (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:3950-3965`).
    let at = CN_0935 + 60 * SECOND_NS;
    advance(&node, at);
    clock::publish_book(&node, moutai(), ("1500.00", 100), ("1500.00", 60), at);
    within(10, "the partial fill lands", || {
        status(&node, "r1-s2-1") == OrderStatus::PartiallyFilled
    });
    assert_eq!(fills(&store), 1, "exactly one fill");

    let (qty, price, commission): (String, String, String) = store
        .call(|conn| {
            conn.query_row("SELECT quantity, price, commission FROM fills", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
        })
        .expect("the fill row");
    assert_eq!(qty, "60", "the book's own size");
    let position_qty = trade::open_positions(&node).unwrap();
    assert_eq!(position_qty.len(), 1, "{position_qty:?}");
    assert_eq!(position_qty[0]["quantity"], "60");

    let after_fill = balance_cny(&node, &desk_id);
    assert_ne!(
        after_fill, "500000.00 CNY|160000.00 CNY|340000.00 CNY",
        "the fill moved the account"
    );
    registry.stop_all();

    let (registry, node) = restart(&store, None, &desk_id);
    advance(&node, CN_1530);
    assert_eq!(
        balance_cny(&node, &desk_id),
        after_fill,
        "restoration reproduced the post-fill figure, reservation included"
    );
    assert_eq!(lock_map_len(&node, &desk_id), 1);
    let restored_position = trade::open_positions(&node).unwrap();
    assert_eq!(
        restored_position[0]["quantity"], "60",
        "the position is back"
    );

    // Two observations about restoring a *partially filled* order, both outside
    // R1's fix and neither caused by it. They are asserted so they cannot drift.
    //
    // 1. It comes back `ACCEPTED`, not `PARTIALLY_FILLED`: unlike an accepted
    //    order, `(PartiallyFilled, Accepted) => Accepted` is a **legal**
    //    transition in the pinned crate
    //    (`nautilus-model-0.62.0/src/orders/mod.rs:287`), so the re-hand's
    //    `OrderAccepted` is applied rather than dropped. `filled_qty` survives,
    //    which is what the reservation and the cancel depend on.
    let restored_order = trade::open_orders(&node).unwrap();
    assert_eq!(restored_order.len(), 1, "{restored_order:?}");
    assert_eq!(restored_order[0]["status"], "ACCEPTED");
    assert_eq!(restored_order[0]["filled_quantity"], "60");
    assert_eq!(restored_order[0]["quantity"], "100");
    // 2. Because that event is applied it is also **published**, so
    //    `crate::trade::capture_order` stores a second `OrderAccepted` — stamped
    //    at the restarted node's clock, which sorts it *before* the fill — and
    //    the stored chain stops replaying. This is the slice-012 family of
    //    defect (a stored chain `OrderAny::from_events` cannot rebuild), for
    //    partially filled orders across a restart.
    assert_eq!(
        kinds(&store, &desk_id, "r1-s2-1"),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderAccepted",
            "OrderFilled"
        ],
        "DEFECT (separate from R1): restoration replayed a duplicate OrderAccepted"
    );
    assert!(
        trade::history_orders(&store, &desk_id)
            .expect("the history reads")
            .is_empty(),
        "DEFECT (separate from R1): the duplicated chain drops out of history_orders"
    );

    let before_cancel = balance_cny(&node, &desk_id);
    cancel(&store, &registry, &desk_id, "r1-s2-1", "r1-s2-cancel");
    assert_eq!(status(&node, "r1-s2-1"), OrderStatus::Canceled);
    let after = balance_cny(&node, &desk_id);

    let total_before = before_cancel.split('|').next().unwrap();
    let total_after = after.split('|').next().unwrap();
    assert_eq!(
        total_before, total_after,
        "the cancel moved no cash: {before_cancel} -> {after}"
    );
    assert_eq!(
        after,
        format!("{total_after}|0.00 CNY|{total_after}"),
        "only the unfilled reservation was released"
    );

    // The executed facts are untouched: same fill row, same position, same fee.
    assert_eq!(fills(&store), 1);
    let (qty2, price2, commission2): (String, String, String) = store
        .call(|conn| {
            conn.query_row("SELECT quantity, price, commission FROM fills", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
        })
        .expect("the fill row");
    assert_eq!(
        (qty2, price2, commission2),
        (qty, price, commission),
        "the fill NautilusTrader produced is unchanged"
    );
    let kept = trade::open_positions(&node).unwrap();
    assert_eq!(kept[0]["quantity"], "60", "the position is retained");
    assert_eq!(
        kinds(&store, &desk_id, "r1-s2-1")
            .last()
            .map(String::as_str),
        Some("OrderCanceled"),
        "one native terminal event"
    );
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// S3 — two restored orders in one account
// ---------------------------------------------------------------------------

/// S3: two resting BUYs on the same instrument and account. Cancelling the first
/// releases only its reservation; cancelling it again is refused with no event;
/// a further restart holds the second order's lock and releases it on its own
/// cancel.
///
/// The discriminating step is the **second** restart. With two open orders the
/// crate self-heals on the first cancel — `AccountsManager::update_balance_locked`
/// takes its non-empty branch, recomputes the sum from the one order left, and
/// repopulates the map on the way (`manager.rs:474-480`). It is the restart that
/// leaves a lone open order with an empty map, and it is that order's cancel
/// that strands without the repair.
#[test]
fn s3_two_restored_orders_release_independently() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "r1-s3");
    let desk_id = handle.desk_id().to_owned();

    rest_buy(&store, &registry, &desk_id, "r1-s3-a", 100, "1600.00");
    rest_buy(&store, &registry, &desk_id, "r1-s3-b", 100, "1400.00");
    assert_eq!(
        balance_cny(&node, &desk_id),
        "500000.00 CNY|300000.00 CNY|200000.00 CNY",
        "160000 + 140000, summed by NautilusTrader into one map entry"
    );
    registry.stop_all();

    let (registry, node) = restart(&store, None, &desk_id);
    advance(&node, CN_1530);
    assert_eq!(
        balance_cny(&node, &desk_id),
        "500000.00 CNY|300000.00 CNY|200000.00 CNY",
        "both reservations came back"
    );
    assert_eq!(
        lock_map_len(&node, &desk_id),
        1,
        "one (instrument, CNY) key"
    );

    cancel(&store, &registry, &desk_id, "r1-s3-a", "r1-s3-cancel-a");
    assert_eq!(
        balance_cny(&node, &desk_id),
        "500000.00 CNY|140000.00 CNY|360000.00 CNY",
        "only the first order's 160000 was released"
    );
    assert_eq!(status(&node, "r1-s3-b"), OrderStatus::Accepted);

    // Cancelling a closed order is refused before the node is touched, and
    // nothing else moves.
    let again = trade::cancel(
        &store,
        &registry,
        &desk_id,
        "r1-s3-a",
        r#"{"action_id":"r1-s3-cancel-a-again"}"#,
        &trade::Source::Session,
    )
    .expect_err("a closed order cannot be cancelled twice");
    assert!(
        matches!(again, trade::TradeError::OrderNotFound(_)),
        "{again:?}"
    );
    assert_eq!(
        balance_cny(&node, &desk_id),
        "500000.00 CNY|140000.00 CNY|360000.00 CNY",
        "the repeat released nothing further"
    );
    let a_chain = kinds(&store, &desk_id, "r1-s3-a");
    assert_eq!(
        a_chain,
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ],
        "no duplicate terminal event"
    );
    registry.stop_all();

    // The second restart: one open order, and the map must be rebuilt for it
    // alone — the case the repair exists for.
    let (registry, node) = restart(&store, None, &desk_id);
    advance(&node, CN_1530);
    assert_eq!(
        balance_cny(&node, &desk_id),
        "500000.00 CNY|140000.00 CNY|360000.00 CNY",
        "the surviving order still holds its cash"
    );
    assert_eq!(lock_map_len(&node, &desk_id), 1);
    assert_eq!(
        kinds(&store, &desk_id, "r1-s3-a"),
        a_chain,
        "the cancelled order replayed nothing"
    );

    cancel(&store, &registry, &desk_id, "r1-s3-b", "r1-s3-cancel-b");
    assert_eq!(
        balance_cny(&node, &desk_id),
        OPENING,
        "and it releases on its own cancel"
    );
    assert_eq!(fills(&store), 0);
    let history = trade::history_orders(&store, &desk_id).expect("the history reads");
    assert_eq!(history.len(), 2, "two chains, once each: {history:?}");
    assert!(
        history.iter().all(|order| order["status"] == "CANCELED"),
        "{history:?}"
    );
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// S4 — the F2/F3 session-end recovery ordering
// ---------------------------------------------------------------------------

/// S4: the real ordering. The order rests before 14:57; the desk stops; it
/// restarts past the deadline behind a **held** first publish so the crossing
/// quote is queued but unprocessed; the restore job sends the same `CancelOrder`
/// `crate::trade::cancel` sends, with no market data at all. The order ends
/// `OrderCanceled` once, the reservation is released, and the crossing quote
/// that lands afterwards fills nothing.
#[test]
fn s4_session_end_recovery_releases_with_no_data() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "r1-s4");
    let desk_id = handle.desk_id().to_owned();

    rest_buy(&store, &registry, &desk_id, "r1-s4-1", 100, "1600.00");
    let rested = balance_cny(&node, &desk_id);
    assert_eq!(rested, "500000.00 CNY|160000.00 CNY|340000.00 CNY");
    advance(&node, CN_1457 - SECOND_NS);
    registry.stop_all();

    // The restart is past 14:57 and the feed answers nothing until released, so
    // the crossing 1500.00 body sits on the wire while restoration decides.
    let (base, release) = held_server(feed::chart_body(
        "600519.SS",
        "CNY",
        "1500.00",
        1_788_917_700,
    ));
    let (registry, node) = restart(&store, Some(FeedBase::standin(base)), &desk_id);
    assert_eq!(
        status(&node, "r1-s4-1"),
        OrderStatus::Accepted,
        "restoration finished with no quote in the book"
    );
    assert!(
        node.call(|context| context.cache.borrow().quote(&moutai_id()).is_none())
            .unwrap(),
        "and no market data reached the node"
    );
    assert_eq!(
        balance_cny(&node, &desk_id),
        rested,
        "the reservation came back with the account"
    );

    advance(&node, CN_1530);
    cancel_at(&node, "r1-s4-1", CN_1530);
    within(10, "the restored order is terminated", || {
        status(&node, "r1-s4-1") == OrderStatus::Canceled
    });
    assert_eq!(
        balance_cny(&node, &desk_id),
        OPENING,
        "the day-lifetime cancel released the reservation with no data at all"
    );
    let terminated = kinds(&store, &desk_id, "r1-s4-1");
    assert_eq!(
        terminated,
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ]
    );

    // Only now does the held crossing quote land.
    release.store(true, Ordering::SeqCst);
    await_ask(&node, "1500.00");
    assert_eq!(fills(&store), 0, "the crossing quote filled nothing");
    assert_eq!(
        kinds(&store, &desk_id, "r1-s4-1"),
        terminated,
        "and no event follows the terminal one"
    );
    assert_eq!(
        balance_cny(&node, &desk_id),
        OPENING,
        "the released state stands"
    );
    let history = trade::history_orders(&store, &desk_id).expect("the history reads");
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0]["status"], "CANCELED");
    registry.stop_all();
}
