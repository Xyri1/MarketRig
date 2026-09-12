//! The CN execution boundary: T+1 sellability, the supported session window,
//! the day-lifetime deadline, the per-node critical section that publishes an
//! executable book and admits an order without the two overlapping, and the
//! AE-9 sampled-observation rule that decides what is published at all.
//!
//! Every decision here reads the **node clock** (`NodeContext.clock`), never
//! `crate::store::now_ns()`: the session window, the owning trading date of a
//! restored order and the 14:57 deadline are all judged on the same time source
//! the sandbox stamps its events with (feature SPEC `a-share-engine` §5.1,
//! §5.3).
//!
//! The state in [`CnExec`] is per node, in memory only, and touched exclusively
//! on the node thread — through [`Node::call`] from another thread, and directly
//! from the polling task, which *is* that thread. Nothing here is persisted: a
//! restart is a [`CnExec::reset`] (feature SPEC §2.5).
//!
//! Contract: `sdd/features/a-share-engine/SPEC.md` §1, §2, §3, §5.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, TimeZone, Timelike, Weekday};
use chrono_tz::Tz;
use nautilus_common::cache::{Cache, CacheView};
use nautilus_common::clock::Clock;
use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::messages::DataEvent;
use nautilus_common::timer::{TimeEvent, TimeEventCallback};
use nautilus_core::UnixNanos;
use nautilus_model::data::{Data, InstrumentStatus, QuoteTick};
use nautilus_model::enums::{MarketStatusAction, OrderSide, PositionSide};
use nautilus_model::identifiers::{ClientOrderId, InstrumentId, TraderId};
use nautilus_model::orders::{Order, OrderAny};
use nautilus_model::types::{Price, Quantity};
use rust_decimal::Decimal;

use crate::catalog::Entry;
use crate::feed::Observed;
use crate::hithink::{AShareFeed, Reason};
use crate::node::{Node, NodeContext};

/// How long a caller waits for a publication to be visible in the node's cache,
/// and for the instrument to stop being busy. Both are bounded so a boundary
/// alert or a cancel never waits on a failed publish (feature SPEC §2.5).
const WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const WAIT_POLL: Duration = Duration::from_millis(2);

/// The supported continuous sessions as minutes from Shanghai midnight,
/// half-open: [09:30,11:30) and [13:00,14:57) (feature SPEC §5.1). This is the
/// *execution* window and is deliberately narrower than [`crate::feed`]'s
/// market-phase table, whose 15:00 close is the market phase.
const MORNING: (u32, u32) = (9 * 60 + 30, 11 * 60 + 30);
const AFTERNOON: (u32, u32) = (13 * 60, 14 * 60 + 57);

/// The day-lifetime boundary, 14:57 Asia/Shanghai (feature SPEC §5.1).
const DEADLINE_MINUTE: (u32, u32) = (14, 57);

/// The four instants a kernel-clock alert is armed for (feature SPEC §5.1).
const BOUNDARIES: [(u32, u32); 4] = [(9, 30), (11, 30), (13, 0), (14, 57)];

/// The one alert name the node re-arms at every boundary.
const ALERT: &str = "cn-session-boundary";

/// The A-share lot, which is also the SELL odd-remainder modulus (§3.1).
const LOT: i64 = 100;

fn shanghai(at_ns: u64) -> DateTime<Tz> {
    DateTime::from_timestamp_nanos(at_ns.min(i64::MAX as u64) as i64)
        .with_timezone(&Tz::Asia__Shanghai)
}

/// The Shanghai calendar date of an instant, `yyyyMMdd` — the form the day
/// comparisons and the provider's own calendar use. One derivation, shared with
/// the provider's own dating of its answers.
pub fn shanghai_date(at_ns: u64) -> String {
    crate::hithink::shanghai_date(at_ns.min(i64::MAX as u64) as i64)
}

/// The instant of `hour:minute` Asia/Shanghai on the date `at_ns` falls in.
fn at_shanghai(at_ns: u64, hour: u32, minute: u32) -> u64 {
    let naive = shanghai(at_ns)
        .date_naive()
        .and_hms_opt(hour, minute, 0)
        .expect("a valid time of day");
    Tz::Asia__Shanghai
        .from_local_datetime(&naive)
        .earliest()
        .and_then(|at| at.timestamp_nanos_opt())
        .map_or(at_ns, |ns| ns.max(0) as u64)
}

/// 14:57 Asia/Shanghai on the date `at_ns` falls in.
pub fn deadline_ns(at_ns: u64) -> u64 {
    at_shanghai(at_ns, DEADLINE_MINUTE.0, DEADLINE_MINUTE.1)
}

/// Midnight Asia/Shanghai opening the day `at_ns` falls in — the half-open
/// interval's lower bound for §1.1's T+1 term.
pub fn day_start_ns(at_ns: u64) -> u64 {
    at_shanghai(at_ns, 0, 0)
}

/// Is `at_ns` inside a supported continuous session on a weekday? The confirmed
/// trading day itself is the provider's answer and arrives with readiness; this
/// is the clock half of the gate.
pub fn in_session(at_ns: u64) -> bool {
    let local = shanghai(at_ns);
    if matches!(local.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let minute = local.hour() * 60 + local.minute();
    [MORNING, AFTERNOON]
        .iter()
        .any(|&(open, close)| minute >= open && minute < close)
}

/// Is `at_ns` the midday break — the one out-of-session interval that suspends
/// fills without ending the trading day (§5.1)?
fn in_lunch(at_ns: u64) -> bool {
    let local = shanghai(at_ns);
    if matches!(local.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let minute = local.hour() * 60 + local.minute();
    minute >= MORNING.1 && minute < AFTERNOON.0
}

/// The supported session as the refusal names it (§5.1).
pub const SESSION_WINDOW: &str =
    "outside the supported session [09:30,11:30) and [13:00,14:57) Asia/Shanghai";

/// The `InstrumentStatus` a CN instrument carries at `at_ns` (feature SPEC
/// §5.3): `Trading` inside a supported session with usable data, `Pause` inside
/// one without it and across the midday break, `Close` otherwise.
pub fn status_for(at_ns: u64, ready: bool) -> MarketStatusAction {
    match (in_session(at_ns), ready) {
        (true, true) => MarketStatusAction::Trading,
        (true, false) => MarketStatusAction::Pause,
        (false, _) if in_lunch(at_ns) => MarketStatusAction::Pause,
        (false, _) => MarketStatusAction::Close,
    }
}

/// The trading date a restored order belongs to: its own `ts_accepted` when the
/// venue accepted it, its `ts_init` otherwise (feature SPEC §5.3; F3 (6) — the
/// snapshot payload carries both, `book_snapshots.written_at_ns` is MarketRig's
/// wall clock and says nothing about the trading date).
pub fn owning_date(order: &OrderAny) -> String {
    let ts = order
        .ts_accepted()
        .unwrap_or_else(|| order.ts_init())
        .as_u64();
    shanghai_date(ts)
}

/// Has this order outlived its trading day at `now_ns`? Either it belongs to an
/// earlier Shanghai date, or today's 14:57 has passed. `yyyyMMdd` orders
/// lexicographically, so the comparison is the date comparison.
///
/// An order dated *after* `now_ns` is not expired: only a clock that moved
/// backwards can produce one, and terminating it would destroy a live order.
pub fn expired(order: &OrderAny, now_ns: u64) -> bool {
    owning_date(order).as_str() < shanghai_date(now_ns).as_str() || now_ns >= deadline_ns(now_ns)
}

// ---------------------------------------------------------------------------
// T+1 sellability (§1.1, AE-2)
// ---------------------------------------------------------------------------

/// §1.1's three derived quantities and their result, read from the node cache
/// alone and summed as exact decimals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eligibility {
    pub position: Decimal,
    pub locked: Decimal,
    pub reserved: Decimal,
    pub sellable: Decimal,
}

impl Eligibility {
    /// §1.2's refusal sentence.
    fn refusal(&self, quantity: Decimal, instrument_id: &str) -> String {
        format!(
            "quantity {quantity} exceeds sellable {} for {instrument_id}: {} bought today are \
             locked by T+1; {} reserved by outstanding sells",
            self.sellable, self.locked, self.reserved
        )
    }
}

/// §1.1 from one borrow of the node's cache, with no store read at all (F6):
///
/// - **position** — the open long quantity in the instrument.
/// - **locked** — today's BUY fills on that position. `Position::events` is a
///   public `Vec<OrderFilled>`, each carrying `order_side`, `last_qty` and
///   `ts_event`, so the day's half-open interval is a filter over it.
/// - **reserved** — the remaining quantity of every SELL for the instrument that
///   is not closed. `orders_open` is **wrong** here: it excludes `INITIALIZED`,
///   and MarketRig's own submit is still `INITIALIZED` when `place` returns.
pub fn sellable(cache: &Cache, instrument_id: InstrumentId, now_ns: u64) -> Eligibility {
    let day_start_ns = day_start_ns(now_ns);
    let positions = cache.positions_open(None, Some(&instrument_id), None, None, None);
    let position: Decimal = positions
        .iter()
        .filter(|p| p.side == PositionSide::Long)
        .map(|p| p.quantity.as_decimal())
        .sum();
    let locked: Decimal = positions
        .iter()
        .flat_map(|p| p.events.iter())
        .filter(|fill| fill.order_side == OrderSide::Buy && fill.ts_event.as_u64() >= day_start_ns)
        .map(|fill| fill.last_qty.as_decimal())
        .sum();
    let reserved: Decimal = cache
        .orders(
            None,
            Some(&instrument_id),
            None,
            None,
            Some(OrderSide::Sell),
        )
        .iter()
        .filter(|order| !order.is_closed())
        .map(|order| order.leaves_qty().as_decimal())
        .sum();
    Eligibility {
        position,
        locked,
        reserved,
        sellable: (position - locked - reserved).max(Decimal::ZERO),
    }
}

// ---------------------------------------------------------------------------
// Per-node execution state
// ---------------------------------------------------------------------------

/// The day's readiness facts for one instrument (§2.1, §2.2): the accepted
/// provider reference, the band derived from it, and the Shanghai date they were
/// established for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub prev_close: Decimal,
    pub limit_up: Decimal,
    pub limit_down: Decimal,
    pub band_date: String,
}

/// AE-9's per-order metadata: the accepted observation sequence and cumulative
/// volume at execution-time admission. Daemon-side only, never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Baseline {
    pub seq: u64,
    pub volume: Decimal,
}

/// One resting CN LIMIT order as the observation rule reads it.
#[derive(Debug, Clone, Copy)]
pub struct Resting {
    pub client_order_id: ClientOrderId,
    pub side: OrderSide,
    pub limit: Decimal,
    pub remaining: Decimal,
}

/// Every non-closed LIMIT order on one CN instrument, from the node cache.
pub fn resting_limits(cache: &Cache, instrument_id: InstrumentId) -> Vec<Resting> {
    cache
        .orders(None, Some(&instrument_id), None, None, None)
        .iter()
        .filter(|order| !order.is_closed())
        .filter_map(|order| {
            Some(Resting {
                client_order_id: order.client_order_id(),
                side: order.order_side(),
                limit: order.price()?.as_decimal(),
                remaining: order.leaves_qty().as_decimal(),
            })
        })
        .collect()
}

/// BUY fills at `last <= limit`, SELL at `last >= limit` (§2.5).
fn compatible(side: OrderSide, last: Decimal, limit: Decimal) -> bool {
    match side {
        OrderSide::Buy => last <= limit,
        _ => last >= limit,
    }
}

/// One CN instrument's execution state on this node.
#[derive(Debug)]
pub struct Inst {
    /// A crossing or sized book is in flight: admission waits rather than
    /// filling against it (F8 item 2).
    pub busy: bool,
    /// The last `ts_event` published for this instrument, so every publish is
    /// strictly monotonic (F5: a stamp below the book's own skips the update).
    pub ts_last: u64,
    /// `Err` leaves the instrument non-executable until a recovery clears it.
    /// A node start and a provider switch both put it back to
    /// `Err(NoCalendar)` under *either* feed — see [`block`] — because only a
    /// confirmed same-day trading calendar opens CN execution, Yahoo CN
    /// included (§2.1, AE-7).
    pub readiness: Result<(), Reason>,
    /// The Shanghai date readiness was established on. Rollover invalidates it
    /// (§2.1): a boundary alert or an admission on a later date reads it as
    /// `NO_CALENDAR` until the next poll re-establishes the day, so nothing is
    /// admitted on a new day against yesterday's evidence.
    pub ready_date: Option<String>,
    /// The day's accepted reference and band, present exactly while readiness
    /// holds under HiThink.
    pub ready: Option<Ready>,
    /// The last published `InstrumentStatus`, so a status is published on change
    /// only (§5.3 republishes it on every start regardless).
    pub status: Option<MarketStatusAction>,
    /// The last accepted observation's price — the book's idle level and what a
    /// MARKET executes against (§2.5).
    pub idle_last: Option<Decimal>,
    /// Accepted observations so far, and the preceding accepted observation's
    /// cumulative volume (§2.5).
    pub seq: u64,
    pub prev_volume: Option<Decimal>,
    baselines: HashMap<ClientOrderId, Baseline>,
    /// The Shanghai date on which this instrument's provider reference changed:
    /// it stays blocked until the next day (§2.1).
    blocked_date: Option<String>,
}

impl Default for Inst {
    fn default() -> Inst {
        Inst {
            busy: false,
            ts_last: 0,
            readiness: Ok(()),
            ready_date: None,
            ready: None,
            status: None,
            idle_last: None,
            seq: 0,
            prev_volume: None,
            baselines: HashMap::new(),
            blocked_date: None,
        }
    }
}

impl Inst {
    /// Readiness as of `now_ns`: held readiness from an earlier Shanghai date is
    /// `NO_CALENDAR` (§2.1 rollover) until a poll on the new day re-establishes
    /// it. Readiness set without a date (feed failure paths, module checks)
    /// carries no day claim and is read as it stands.
    pub fn ready_at(&self, now_ns: u64) -> Result<(), Reason> {
        self.readiness?;
        match &self.ready_date {
            Some(date) if *date != shanghai_date(now_ns) => Err(Reason::NoCalendar),
            _ => Ok(()),
        }
    }

    /// Discards every temporal baseline, leaving readiness and the band alone
    /// (§2.5): restart, feed recovery, provider switch, volume decrease.
    fn forget(&mut self) {
        self.seq = 0;
        self.prev_volume = None;
        self.baselines.clear();
    }

    /// The status to publish, `None` when it has not changed.
    fn set_status(&mut self, action: MarketStatusAction) -> Option<MarketStatusAction> {
        if self.status == Some(action) {
            return None;
        }
        self.status = Some(action);
        Some(action)
    }
}

/// The node's whole CN execution state, plus the release latch that holds the
/// first feed publish until recovery has decided (feature SPEC §5.3).
#[derive(Debug)]
pub struct CnExec {
    /// False until `Registry::start` has restored, reconciled and re-gated the
    /// desk. The polling tasks neither fetch nor publish before it is true.
    pub released: bool,
    /// The provider the last cycle ran under: a change is a provider switch
    /// (§5.3), and it is what decides whether MARKET takes AE-9's sized
    /// publication or Yahoo's native two-sided quote.
    pub feed: AShareFeed,
    /// A module-check seam, and only that. The checks in this crate drive a bare
    /// node with no HiThink provider and no polling task, publishing onto the
    /// data path by hand, so nothing in them could install the confirmed
    /// calendar a real cycle installs. Such a desk assumes today's, or every one
    /// of them would be asserting AE-7's block instead of the node mechanics it
    /// is about. `crate::node::controlled_registry` sets it; the AE-7 checks use
    /// `controlled_registry_uncalendared` and get the production default back.
    #[cfg(test)]
    pub assume_calendar: bool,
    /// The second module-check seam, and only that: does a boundary alert sweep
    /// the orders past their day lifetime? The `feasibility` checks wind one
    /// node's clock across Shanghai days without ever restarting it — a state
    /// production cannot reach, because `Registry::start` reconciles every
    /// prior-day CN order before the first poll — so the sweep would terminate
    /// the very resting orders they characterize the sandbox with. The checks
    /// that *are* about the session boundaries turn it on ([`desk`]); production
    /// always sweeps.
    #[cfg(test)]
    pub sweep: bool,
    per: HashMap<InstrumentId, Inst>,
}

impl Default for CnExec {
    fn default() -> CnExec {
        CnExec {
            released: false,
            feed: AShareFeed::Yahoo,
            #[cfg(test)]
            assume_calendar: false,
            #[cfg(test)]
            sweep: false,
            per: HashMap::new(),
        }
    }
}

impl CnExec {
    pub fn new() -> CnExec {
        CnExec::default()
    }

    /// A calendar verdict as this node reads it — the production answer, except
    /// on a module-check desk that [`CnExec::assume_calendar`] speaks for.
    pub fn calendar_verdict(&self, day: Result<(), Reason>) -> Result<(), Reason> {
        #[cfg(test)]
        if self.assume_calendar {
            return Ok(());
        }
        day
    }

    /// Does a boundary end the day for the orders that outlived it? Always, in
    /// production; see [`CnExec::sweep`] for the module-check seam.
    fn sweeps(&self) -> bool {
        #[cfg(test)]
        return self.sweep;
        #[cfg(not(test))]
        true
    }

    pub fn inst(&mut self, instrument_id: InstrumentId) -> &mut Inst {
        self.per.entry(instrument_id).or_default()
    }

    /// Restart, feed recovery, provider switch, volume decrease: every temporal
    /// baseline is discarded (feature SPEC §2.5). Nothing is persisted, so a
    /// restart *is* this.
    pub fn reset(&mut self) {
        self.per.clear();
    }

    /// The strictly monotonic stamp for the next publish on this instrument.
    pub fn stamp(&mut self, instrument_id: InstrumentId, candidate_ns: u64) -> u64 {
        let inst = self.inst(instrument_id);
        let ts = candidate_ns.max(inst.ts_last + 1);
        inst.ts_last = ts;
        ts
    }

    /// Execution-time admission's own half of AE-9: the order's baseline is the
    /// node's latest accepted observation (§2.5).
    pub fn baseline(&mut self, instrument_id: InstrumentId, client_order_id: ClientOrderId) {
        let inst = self.inst(instrument_id);
        let baseline = Baseline {
            seq: inst.seq,
            volume: inst.prev_volume.unwrap_or_default(),
        };
        inst.baselines.insert(client_order_id, baseline);
    }

    #[cfg(test)]
    pub fn baseline_of(&mut self, instrument_id: InstrumentId, id: &str) -> Option<Baseline> {
        self.inst(instrument_id)
            .baselines
            .get(&ClientOrderId::from(id))
            .copied()
    }

    /// One accepted provider observation, turned into what the node is told
    /// (§2.5, AE-9 — `feasibility/f8_restart.rs::Gate`, ported). Pure: the caller
    /// reads the cache and the clock, and executes the [`Plan`].
    pub fn observe(
        &mut self,
        observed: &Observed,
        evidence: Result<(), Reason>,
        now_ns: u64,
        resting: &[Resting],
    ) -> Plan {
        let entry = observed.entry;
        let instrument_id = InstrumentId::from(entry.instrument_id);
        let today = shanghai_date(now_ns);
        let ready = self.readiness(observed, evidence, &today);
        let last = observed.last;
        let inst = self.inst(instrument_id);

        // (a) Unreadiness pauses the instrument and publishes nothing else; the
        // awareness row `crate::feed` already wrote stands.
        let ready = match ready {
            Err(reason) => {
                inst.readiness = Err(reason);
                inst.ready = None;
                inst.forget();
                let status = inst.set_status(status_for(now_ns, false));
                return Plan {
                    status,
                    books: Vec::new(),
                };
            }
            Ok(ready) => ready,
        };
        inst.readiness = Ok(());
        inst.ready_date = Some(today.clone());
        inst.ready = Some(ready.clone());
        inst.idle_last = Some(last);

        // (c) A same-day cumulative-volume decrease pauses, discards, and uses
        // the next valid observation only to re-establish (§2.5).
        if inst.prev_volume.is_some_and(|prev| observed.volume < prev) {
            inst.forget();
            let status = inst.set_status(MarketStatusAction::Pause);
            return Plan {
                status,
                books: vec![Book::Idle(last)],
            };
        }

        // (b) The session gate. Outside a session the observation is awareness
        // only: it is not accepted, and the book it leaves behind is idle. The
        // same-day baseline survives the midday break (§2.5).
        let action = status_for(now_ns, true);
        let status = inst.set_status(action);
        if action != MarketStatusAction::Trading {
            return Plan {
                status,
                books: vec![Book::Idle(last)],
            };
        }

        // (d) The observation is accepted.
        inst.seq += 1;
        let seq = inst.seq;
        let grown = inst.prev_volume.is_some_and(|prev| observed.volume > prev);
        let mut released: Vec<&Resting> = Vec::new();
        if grown {
            released = resting
                .iter()
                .filter(|order| {
                    inst.baselines
                        .get(&order.client_order_id)
                        .is_some_and(|base| seq > base.seq && observed.volume > base.volume)
                        && compatible(order.side, last, order.limit)
                })
                .collect();
        }
        // Direction suppression against the observed condition (§2.3): at the
        // upper limit no BUY crosses, at the lower limit no SELL crosses.
        let suppress_buy = last == ready.limit_up;
        let suppress_sell = last == ready.limit_down;
        let side_size = |side: OrderSide| -> Decimal {
            released
                .iter()
                .filter(|order| order.side == side)
                .map(|order| order.remaining)
                .sum()
        };
        let buys = if suppress_buy {
            Decimal::ZERO
        } else {
            side_size(OrderSide::Buy)
        };
        let sells = if suppress_sell {
            Decimal::ZERO
        } else {
            side_size(OrderSide::Sell)
        };

        // The preceding-observation baseline advances on every accepted
        // observation, whatever it released — an incompatible price with more
        // volume advances it too (§2.5).
        inst.prev_volume = Some(observed.volume);
        for order in resting {
            inst.baselines
                .entry(order.client_order_id)
                .or_insert(Baseline {
                    seq,
                    volume: observed.volume,
                });
        }
        inst.baselines
            .retain(|id, _| resting.iter().any(|order| order.client_order_id == *id));

        let mut books = Vec::new();
        if buys > Decimal::ZERO {
            books.push(Book::Crossing {
                bid: (last, Decimal::ZERO),
                ask: (last, buys),
                idle: last,
            });
        }
        if sells > Decimal::ZERO {
            books.push(Book::Crossing {
                bid: (last, sells),
                ask: (last, Decimal::ZERO),
                idle: last,
            });
        }
        if books.is_empty() {
            books.push(Book::Idle(last));
        }
        Plan { status, books }
    }

    /// §2.1's readiness for one observation: the poll's own data verdict, the
    /// day's calendar and bar evidence, an unchanged positive reference, and the
    /// band derived from it.
    fn readiness(
        &mut self,
        observed: &Observed,
        evidence: Result<(), Reason>,
        today: &str,
    ) -> Result<Ready, Reason> {
        let entry = observed.entry;
        let instrument_id = InstrumentId::from(entry.instrument_id);
        observed.ok?;
        evidence?;
        let (limit_up, limit_down) = entry.band(observed.prev_close).ok_or(Reason::NoReference)?;
        {
            let inst = self.inst(instrument_id);
            // A reference that changed during the day blocks that instrument
            // until the next one; no automatic intraday rebasing (§2.1).
            if inst.blocked_date.as_deref() == Some(today) {
                return Err(Reason::ReferenceChanged);
            }
            inst.blocked_date = None;
            if let Some(held) = &inst.ready
                && held.band_date == today
                && held.prev_close != observed.prev_close
            {
                inst.blocked_date = Some(today.to_owned());
                return Err(Reason::ReferenceChanged);
            }
        }
        Ok(Ready {
            prev_close: observed.prev_close,
            limit_up,
            limit_down,
            band_date: today.to_owned(),
        })
    }
}

/// The state's handle as the node thread holds it.
pub type Exec = Rc<RefCell<CnExec>>;

/// One book the poller publishes for an accepted observation (§2.5): the idle
/// book has zero-sized sides and crosses nothing; a crossing book is up for
/// exactly one confirmed tick and is then restored to idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Book {
    Idle(Decimal),
    Crossing {
        bid: (Decimal, Decimal),
        ask: (Decimal, Decimal),
        idle: Decimal,
    },
}

/// What one observation asks the node for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The `InstrumentStatus` to publish, `None` when it has not changed.
    pub status: Option<MarketStatusAction>,
    pub books: Vec<Book>,
}

// ---------------------------------------------------------------------------
// Publishing (node thread)
// ---------------------------------------------------------------------------

/// One `InstrumentStatus` on the data path — the session gate the sandbox's own
/// status handler reads (F1; `feasibility/f8_restart.rs::publish_status`).
pub fn publish_status(entry: &'static Entry, action: MarketStatusAction, ts_ns: u64) {
    let status = InstrumentStatus::new(
        InstrumentId::from(entry.instrument_id),
        action,
        UnixNanos::from(ts_ns),
        UnixNanos::from(ts_ns),
        None,
        None,
        None,
        None,
        None,
    );
    if let Err(e) = get_data_event_sender().send(DataEvent::Data(Data::InstrumentStatus(status))) {
        tracing::error!("the node did not take the CN instrument status: {e}");
    }
}

/// One book for a CN instrument, at the catalog's own precision. A zero size
/// clears that side of the ladder (F4), which is what the idle book is made of.
pub fn publish_quote(
    entry: &'static Entry,
    bid: (Decimal, Decimal),
    ask: (Decimal, Decimal),
    ts_ns: u64,
) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let price = |value: Decimal| {
        Price::from(crate::feed::at_precision(value, entry.price_increment).as_str())
    };
    let size = |value: Decimal| Quantity::from(value.normalize().to_string().as_str());
    let tick = QuoteTick::new(
        instrument_id,
        price(bid.0),
        price(ask.0),
        size(bid.1),
        size(ask.1),
        UnixNanos::from(ts_ns),
        UnixNanos::from(ts_ns),
    );
    if let Err(e) = get_data_event_sender().send(DataEvent::Data(Data::Quote(tick))) {
        tracing::error!("the node did not take the CN quote: {e}");
    }
}

/// A publication that could not be confirmed leaves the instrument
/// non-executable and `Pause`d, so nothing can fill against whatever book is
/// standing; the next successful recovery clears and re-baselines it (§2.5).
fn fail_local(exec: &mut CnExec, entry: &'static Entry, reason: Reason, ts_ns: u64) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    {
        let inst = exec.inst(instrument_id);
        inst.readiness = Err(reason);
        inst.ready = None;
        inst.busy = false;
        inst.forget();
    }
    if exec
        .inst(instrument_id)
        .set_status(MarketStatusAction::Pause)
        .is_some()
    {
        let ts = exec.stamp(instrument_id, ts_ns);
        publish_status(entry, MarketStatusAction::Pause, ts);
    }
}

/// Re-publishes one instrument's session gate whatever it was (§5.3: the gate is
/// per-`OrderMatchingEngine` state that no snapshot carries, so every start
/// re-establishes it).
pub fn republish_status(exec: &mut CnExec, entry: &'static Entry, at_ns: u64) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let ready = exec.inst(instrument_id).ready_at(at_ns).is_ok();
    let action = status_for(at_ns, ready);
    exec.inst(instrument_id).set_status(action);
    let ts = exec.stamp(instrument_id, at_ns);
    publish_status(entry, action, ts);
}

/// The status one instrument's poll cycle asks for, published on change only.
pub fn gate(exec: &mut CnExec, entry: &'static Entry, at_ns: u64) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let ready = exec.inst(instrument_id).ready_at(at_ns).is_ok();
    let action = status_for(at_ns, ready);
    if exec.inst(instrument_id).set_status(action).is_some() {
        let ts = exec.stamp(instrument_id, at_ns);
        publish_status(entry, action, ts);
    }
}

/// [`fail_local`] from another thread — what a failed confirmation does.
pub fn mark_failed(node: &Node, entry: &'static Entry, reason: Reason, ts_ns: u64) {
    let _ =
        node.call(move |context| fail_local(&mut context.cn.borrow_mut(), entry, reason, ts_ns));
}

/// The whole CN leg's feed failure (§2.1, §5.3): every instrument is blocked
/// with the reason, its baselines discarded, so recovery re-baselines before
/// anything can match.
pub fn feed_failed(exec: &Exec, entries: &[&'static Entry], reason: Reason, ts_ns: u64) {
    let mut exec = exec.borrow_mut();
    for entry in entries {
        fail_local(&mut exec, entry, reason, ts_ns);
    }
}

/// A provider switch (§5.3): block CN execution, clear the stale executable
/// state with a strictly monotonic stamp while still paused, and discard every
/// temporal baseline. The next provider's first observation only re-establishes.
pub fn switch_provider(exec: &Exec, entries: &[&'static Entry], feed: AShareFeed, ts_ns: u64) {
    let mut exec = exec.borrow_mut();
    exec.feed = feed;
    for entry in entries {
        let instrument_id = InstrumentId::from(entry.instrument_id);
        let last = exec.inst(instrument_id).idle_last;
        if exec
            .inst(instrument_id)
            .set_status(MarketStatusAction::Pause)
            .is_some()
        {
            let ts = exec.stamp(instrument_id, ts_ns);
            publish_status(entry, MarketStatusAction::Pause, ts);
        }
        if let Some(last) = last {
            let ts = exec.stamp(instrument_id, ts_ns);
            publish_quote(entry, (last, Decimal::ZERO), (last, Decimal::ZERO), ts);
        }
    }
    exec.reset();
    exec.feed = feed;
    block(&mut exec);
}

/// Readiness starts unavailable, under either feed (§2.1, AE-7): only a
/// confirmed same-day calendar — which Yahoo CN needs exactly as HiThink does —
/// can open CN execution, and only the next successful cycle can install one.
/// A node start and a provider switch both come through here.
pub fn block(exec: &mut CnExec) {
    #[cfg(test)]
    if exec.assume_calendar {
        return;
    }
    for entry in crate::feed::cn_entries() {
        exec.inst(InstrumentId::from(entry.instrument_id)).readiness = Err(Reason::NoCalendar);
    }
}

/// One poll cycle's whole treatment of one CN item, on the node thread: read
/// the resting set, apply the AE-9 rule, publish what it planned — unless a
/// MARKET admission owns the instrument right now.
///
/// The critical section is two-directional (F8 item 2). A MARKET's sized book
/// stands from [`size_for_market`] until [`restore_idle`], across the off-thread
/// `confirm`, `place` and `settle`; a cycle that observed in between would
/// advance the baselines *and* republish an idle or crossing book over it, and
/// the MARKET would then meet a zero-size book after the resting LIMITs its own
/// publication already filled.
///
/// So a busy instrument sits the cycle out **before** [`CnExec::observe`] runs:
/// nothing is observed, so no `seq` or `prev_volume` advance is lost, and the
/// next cycle judges its fresher snapshot against the same baselines it would
/// have. The awareness row this snapshot already wrote stands either way, and
/// the busy window is a single order's placement — bounded by the same timeout
/// [`admit`] waits out.
pub async fn cycle_observation(
    observed: &Observed,
    evidence: Result<(), Reason>,
    cache: &CacheView,
    exec: &Exec,
    at_ns: u64,
) {
    let entry = observed.entry;
    let instrument_id = InstrumentId::from(entry.instrument_id);
    if exec.borrow_mut().inst(instrument_id).busy {
        return;
    }
    let plan = {
        let resting = resting_limits(&cache.borrow(), instrument_id);
        exec.borrow_mut()
            .observe(observed, evidence, at_ns, &resting)
    };
    execute(entry, plan, cache, exec, at_ns).await;
}

/// Publishes what one observation planned, on the node thread, from the polling
/// task (§2.5). A crossing book is up for exactly one confirmed tick with the
/// instrument marked busy, and the idle book is restored and confirmed before
/// admission is released again.
///
/// The status goes out *before* a crossing book (a reopening observation that
/// also qualifies must be `Trading` before the cross iterates) and *after* an
/// idle one (a block must clear the stale executable state before it is trusted,
/// F5).
pub async fn execute(
    entry: &'static Entry,
    plan: Plan,
    cache: &CacheView,
    exec: &Exec,
    at_ns: u64,
) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let crossing = plan
        .books
        .iter()
        .any(|book| matches!(book, Book::Crossing { .. }));
    let status = |exec: &Exec| {
        if let Some(action) = plan.status {
            let ts = exec.borrow_mut().stamp(instrument_id, at_ns);
            publish_status(entry, action, ts);
        }
    };
    if crossing {
        status(exec);
    }
    for book in &plan.books {
        match *book {
            Book::Idle(price) => {
                let ts = exec.borrow_mut().stamp(instrument_id, at_ns);
                publish_quote(entry, (price, Decimal::ZERO), (price, Decimal::ZERO), ts);
            }
            Book::Crossing { bid, ask, idle } => {
                let ts = {
                    let mut exec = exec.borrow_mut();
                    let ts = exec.stamp(instrument_id, at_ns);
                    exec.inst(instrument_id).busy = true;
                    ts
                };
                publish_quote(entry, bid, ask, ts);
                let mut outcome = confirmed(cache, instrument_id, ts).await;
                if outcome.is_ok() {
                    let ts = exec.borrow_mut().stamp(instrument_id, at_ns);
                    publish_quote(entry, (idle, Decimal::ZERO), (idle, Decimal::ZERO), ts);
                    outcome = confirmed(cache, instrument_id, ts).await;
                }
                exec.borrow_mut().inst(instrument_id).busy = false;
                if outcome.is_err() {
                    fail_local(
                        &mut exec.borrow_mut(),
                        entry,
                        Reason::PublicationFailed,
                        at_ns,
                    );
                    return;
                }
            }
        }
    }
    if !crossing {
        status(exec);
    }
}

/// Waits, **on the node thread**, until the instrument's cached quote carries
/// `ts_ns`. The wait yields to the runner that has to process the publish, which
/// is why it cannot be [`confirm`]'s blocking sleep.
async fn confirmed(cache: &CacheView, instrument_id: InstrumentId, ts_ns: u64) -> Result<(), ()> {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        if cache
            .borrow()
            .quote(&instrument_id)
            .is_some_and(|quote| quote.ts_event.as_u64() == ts_ns)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            tracing::error!(
                instrument_id = %instrument_id,
                "the node did not confirm a CN publication"
            );
            return Err(());
        }
        tokio::time::sleep(WAIT_POLL).await;
    }
}

// ---------------------------------------------------------------------------
// The critical section (off the node thread)
// ---------------------------------------------------------------------------

/// Waits until the instrument's cached quote carries `ts_ns` — the proof that
/// the engine processed the publish rather than that the daemon sent it.
///
/// ponytail: a blocking 2 ms poll, because this caller is *not* the node thread
/// and must not block it; the poller's own wait is [`confirmed`]. The upgrade
/// path is one async form if the two ever converge.
pub fn confirm(node: &Node, instrument_id: InstrumentId, ts_ns: u64) -> Result<(), Reason> {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let landed = node
            .call(move |context| {
                context
                    .cache
                    .borrow()
                    .quote(&instrument_id)
                    .is_some_and(|quote| quote.ts_event.as_u64() == ts_ns)
            })
            .map_err(|_| Reason::NodeNotStarted)?;
        if landed {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Reason::PublicationFailed);
        }
        thread::sleep(WAIT_POLL);
    }
}

/// Restores the idle book, confirms it, and clears the busy flag — the second
/// half of every crossing or sized publication (§2.5).
pub fn restore_idle(
    node: &Node,
    entry: &'static Entry,
    idle_price: Decimal,
    ts_ns: u64,
) -> Result<(), Reason> {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let idle = node
        .call(move |context| {
            let ts = context.cn.borrow_mut().stamp(instrument_id, ts_ns);
            publish_quote(
                entry,
                (idle_price, Decimal::ZERO),
                (idle_price, Decimal::ZERO),
                ts,
            );
            ts
        })
        .map_err(|_| Reason::NodeNotStarted)?;
    let confirmed = confirm(node, instrument_id, idle);
    let _ = node.call(move |context| context.cn.borrow_mut().inst(instrument_id).busy = false);
    if let Err(reason) = confirmed {
        mark_failed(node, entry, reason, ts_ns);
        return Err(reason);
    }
    Ok(())
}

/// Runs `admission` on the node thread once the instrument is neither busy nor
/// unready — one closure that reads the cache and hands the order, so nothing
/// can be published between the check and the placement (feature SPEC §2.5).
///
/// ponytail: a per-node flag plus a bounded 2 ms poll, not an async mutex,
/// because the publish it waits for is completed *by* the node runner this
/// caller must not block. The upgrade path is a proper async guard if the wait
/// latency ever matters.
pub fn admit<T, F>(node: &Node, instrument_id: InstrumentId, admission: F) -> Result<T, Reason>
where
    T: Send + 'static,
    F: FnOnce(&NodeContext) -> T + Clone + Send + 'static,
{
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let attempt = admission.clone();
        let outcome = node
            .call(move |context| {
                {
                    let now_ns = context.clock.borrow().timestamp_ns().as_u64();
                    let mut exec = context.cn.borrow_mut();
                    let inst = exec.inst(instrument_id);
                    if inst.busy {
                        return Err(Reason::PublicationPending);
                    }
                    // The pre-check must answer with the readiness [`check`]
                    // will, or an approval passes here, records its decision,
                    // and is then refused by `check` — which §5.2 forbids: an
                    // unavailable instrument leaves the action pending with no
                    // decision at all. Inside a session that is `ready_at`,
                    // which reads held evidence from an earlier Shanghai date
                    // as `NO_CALENDAR` (§2.1). Outside one it is the raw
                    // readiness, so `check`'s `ORDER_INVALID` for the session
                    // window still wins over a `MARKET_UNAVAILABLE` that is
                    // only a rollover because the market is shut (gate A1).
                    if in_session(now_ns) {
                        inst.ready_at(now_ns)?;
                    } else {
                        inst.readiness?;
                    }
                }
                Ok(attempt(context))
            })
            .map_err(|_| Reason::NodeNotStarted)?;
        match outcome {
            Ok(value) => return Ok(value),
            Err(Reason::PublicationPending) if Instant::now() < deadline => {
                thread::sleep(WAIT_POLL)
            }
            Err(reason) => return Err(reason),
        }
    }
}

// ---------------------------------------------------------------------------
// Execution-time admission (§1.2, §2.4, §5.1)
// ---------------------------------------------------------------------------

/// Why an admitted order is refused: a form-independent eligibility failure
/// (`ORDER_INVALID`) or a temporary unavailability (`MARKET_UNAVAILABLE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Invalid(String),
    Unavailable(Reason),
}

/// One admitted order's terms, as the execution-time checks read them.
#[derive(Debug, Clone, Copy)]
pub struct Intent {
    pub entry: &'static Entry,
    pub side: OrderSide,
    pub quantity: Decimal,
    /// `Some` exactly for a LIMIT.
    pub price: Option<Decimal>,
}

/// The §5.1 session, §2 readiness, §2.4 band and §1.2/§3.1 quantity checks, in
/// that order, from the node cache and the node clock — inside the admission
/// closure, so nothing can be published between them and the placement.
pub fn check(context: &NodeContext, intent: Intent) -> Result<(), Refusal> {
    let now_ns = context.clock.borrow().timestamp_ns().as_u64();
    let entry = intent.entry;
    let instrument_id = InstrumentId::from(entry.instrument_id);
    if !in_session(now_ns) {
        return Err(Refusal::Invalid(format!(
            "{} is {SESSION_WINDOW}",
            entry.instrument_id
        )));
    }
    let ready = {
        let mut exec = context.cn.borrow_mut();
        let inst = exec.inst(instrument_id);
        inst.ready_at(now_ns).map_err(Refusal::Unavailable)?;
        inst.ready.clone()
    };
    // §2.4: a LIMIT price outside the inclusive band is refused, naming the
    // instrument, reference, date and bounds. Yahoo CN has no band at all.
    if let (Some(price), Some(ready)) = (intent.price, &ready)
        && (price > ready.limit_up || price < ready.limit_down)
    {
        return Err(Refusal::Invalid(format!(
            "price {price} for {} is outside the {} band [{}, {}] around reference {}",
            entry.instrument_id,
            ready.band_date,
            ready.limit_down,
            ready.limit_up,
            ready.prev_close
        )));
    }
    if intent.side == OrderSide::Sell {
        let eligibility = sellable(&context.cache.borrow(), instrument_id, now_ns);
        if intent.quantity > eligibility.sellable {
            return Err(Refusal::Invalid(
                eligibility.refusal(intent.quantity, entry.instrument_id),
            ));
        }
        // §3.1: a SELL is a whole lot, or exactly the odd remainder of the
        // sellable quantity — which cannot be split across submissions, because
        // the reservation moves the remainder with it.
        let lot = Decimal::from(LOT);
        if intent.quantity % lot != Decimal::ZERO
            && intent.quantity % lot != eligibility.sellable % lot
        {
            return Err(Refusal::Invalid(format!(
                "quantity {} for {} is neither a multiple of {LOT} nor the whole odd remainder \
                 of sellable {}",
                intent.quantity, entry.instrument_id, eligibility.sellable
            )));
        }
    }
    Ok(())
}

/// AE-9's MARKET exception (§2.5): the observation a MARKET executes against is
/// published as a sized book, so the whole requested quantity fills at the last
/// price with no remainder slippage — and every compatible resting LIMIT on that
/// instrument fills first, at its own limit price, which is the accepted
/// consequence F8 item 3 established.
///
/// Each side is sized independently for its own compatible resting quantity plus
/// the MARKET's own request when it takes that side; a suppressed side is sized
/// zero and the sandbox answers its own `No market` (§2.3).
///
/// Answers `None` under Yahoo CN, whose synthesized two-sided quote is the book
/// the sandbox matches against already, and when no usable observation stands.
pub fn size_for_market(context: &NodeContext, intent: Intent, ts_ns: u64) -> Option<u64> {
    let entry = intent.entry;
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let (last, ready) = {
        let mut exec = context.cn.borrow_mut();
        if exec.feed != AShareFeed::Hithink {
            return None;
        }
        let inst = exec.inst(instrument_id);
        (inst.idle_last?, inst.ready.clone()?)
    };
    let resting = resting_limits(&context.cache.borrow(), instrument_id);
    let side_size = |side: OrderSide| -> Decimal {
        let taken = if intent.side == side {
            intent.quantity
        } else {
            Decimal::ZERO
        };
        taken
            + resting
                .iter()
                .filter(|order| order.side == side && compatible(order.side, last, order.limit))
                .map(|order| order.remaining)
                .sum::<Decimal>()
    };
    let ask = if last == ready.limit_up {
        Decimal::ZERO
    } else {
        side_size(OrderSide::Buy)
    };
    let bid = if last == ready.limit_down {
        Decimal::ZERO
    } else {
        side_size(OrderSide::Sell)
    };
    let sent = {
        let mut exec = context.cn.borrow_mut();
        let ts = exec.stamp(instrument_id, ts_ns);
        exec.inst(instrument_id).busy = true;
        ts
    };
    publish_quote(entry, (last, bid), (last, ask), sent);
    Some(sent)
}

// ---------------------------------------------------------------------------
// The desk's execution view of a CN instrument (§2.1, §2.3)
// ---------------------------------------------------------------------------

/// What one desk may do with one CN instrument *right now*, as the desk-scoped
/// quote and book resources carry it (§2.1: "expose execution
/// availability/reason independently from health and phase").
///
/// This is deliberately not `health` and not `market_phase`: `health: LIVE` says
/// an observation arrived, the phase says what the market calendar calls the
/// hour, and neither authorizes an order. `availability` is the only field that
/// answers whether this desk's node would admit one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Execution {
    /// `OPEN`, `PAUSED`, `CLOSED` or `UNAVAILABLE`.
    pub availability: &'static str,
    /// Why it is `UNAVAILABLE`; `null` otherwise.
    pub reason: Option<Reason>,
    /// The Shanghai date the band is *inferred* to describe — receipt, never a
    /// source timestamp (§2.1). `null` under Yahoo CN, which has no band.
    pub band_date: Option<String>,
    /// Always true: the date above is inferred from receipt and is not certified
    /// by the provider (§2.1).
    pub band_date_inferred: bool,
    /// Age since the observation was **received**, never market-data age
    /// (§2.1). `null` while nothing was ever observed.
    pub receipt_age_ms: Option<i64>,
    /// Always `UNKNOWN`: no bounded source freshness is promised (§2.1).
    pub source_delay: &'static str,
    /// `HITHINK_SAMPLED` or `YAHOO_SIMPLIFIED` (§2.1, §2.5).
    pub fill_policy: &'static str,
}

pub const OPEN: &str = "OPEN";
pub const PAUSED: &str = "PAUSED";
pub const CLOSED: &str = "CLOSED";
pub const UNAVAILABLE: &str = "UNAVAILABLE";
pub const HITHINK_SAMPLED: &str = "HITHINK_SAMPLED";
pub const YAHOO_SIMPLIFIED: &str = "YAHOO_SIMPLIFIED";

impl Execution {
    /// The view of a desk whose node could not be reached at all: no node, no
    /// admission, whatever the installation-wide observation says.
    fn unreachable() -> Execution {
        Execution {
            availability: UNAVAILABLE,
            reason: Some(Reason::NodeNotStarted),
            band_date: None,
            band_date_inferred: true,
            receipt_age_ms: None,
            source_delay: "UNKNOWN",
            fill_policy: YAHOO_SIMPLIFIED,
        }
    }
}

/// Every CN instrument's execution view for this desk, in one node call.
///
/// This *reads* the node; it never starts one. The caller has already decided
/// whether the desk's node exists — a read is not a reason to start it — and a
/// node that cannot be reached answers `UNAVAILABLE` / `NODE_NOT_STARTED`.
pub fn executions(node: &Node) -> HashMap<&'static str, Execution> {
    node.call(|context| {
        let now_ns = context.clock.borrow().timestamp_ns().as_u64();
        let mut exec = context.cn.borrow_mut();
        let fill_policy = if exec.feed == AShareFeed::Hithink {
            HITHINK_SAMPLED
        } else {
            YAHOO_SIMPLIFIED
        };
        crate::feed::cn_entries()
            .map(|entry| {
                let inst = exec.inst(InstrumentId::from(entry.instrument_id));
                let (availability, reason) = match inst.ready_at(now_ns) {
                    Err(reason) => (UNAVAILABLE, Some(reason)),
                    Ok(()) => {
                        let status = inst.status.unwrap_or_else(|| status_for(now_ns, true));
                        let label = match status {
                            MarketStatusAction::Trading => OPEN,
                            MarketStatusAction::Pause => PAUSED,
                            _ => CLOSED,
                        };
                        (label, None)
                    }
                };
                let view = Execution {
                    availability,
                    reason,
                    band_date: inst.ready.as_ref().map(|ready| ready.band_date.clone()),
                    band_date_inferred: true,
                    receipt_age_ms: None,
                    source_delay: "UNKNOWN",
                    fill_policy,
                };
                (entry.instrument_id, view)
            })
            .collect()
    })
    .unwrap_or_else(|_| {
        crate::feed::cn_entries()
            .map(|entry| (entry.instrument_id, Execution::unreachable()))
            .collect()
    })
}

/// Merges [`executions`] into a desk-scoped market read. US and HK entries are
/// untouched: they carry no `execution` object at all (§2.1).
pub fn attach<'a>(
    node: &Node,
    observations: impl IntoIterator<Item = &'a mut crate::feed::Observation>,
) {
    let views = executions(node);
    for observation in observations {
        if let Some(view) = views.get(observation.instrument_id) {
            let mut view = view.clone();
            // Receipt age, not market-data age — the observation already
            // counts it from `received_at_ns` (§2.1).
            view.receipt_age_ms = observation.age_ms;
            observation.execution = Some(view);
        }
    }
}

/// The last accepted observation's price, which the idle book is restored to.
pub fn idle_last(context: &NodeContext, entry: &'static Entry) -> Option<Decimal> {
    context
        .cn
        .borrow_mut()
        .inst(InstrumentId::from(entry.instrument_id))
        .idle_last
}

// ---------------------------------------------------------------------------
// Session boundaries (§5.1, AE-7)
// ---------------------------------------------------------------------------

/// The next 09:30 / 11:30 / 13:00 / 14:57 Asia/Shanghai strictly after `at_ns`.
fn next_boundary_ns(at_ns: u64) -> u64 {
    BOUNDARIES
        .iter()
        .map(|&(hour, minute)| at_shanghai(at_ns, hour, minute))
        .find(|&ns| ns > at_ns)
        .unwrap_or_else(|| {
            at_shanghai(at_ns + 86_400_000_000_000, BOUNDARIES[0].0, BOUNDARIES[0].1)
        })
}

/// Arms the one kernel-clock alert that carries CN execution across a session
/// boundary without a quote (§5.1, F2): 11:30 pauses, 13:00 and 09:30
/// re-evaluate, 14:57 closes and cancels every remaining CN order. The alert
/// re-arms itself on every firing, so a dead feed cannot leave it unarmed.
pub fn arm_session_alert(context: &NodeContext) {
    let now_ns = context.clock.borrow().timestamp_ns().as_u64();
    arm(
        Rc::clone(&context.clock),
        CacheView::new(Rc::clone(&context.cache)),
        Rc::clone(&context.cn),
        context.trader_id,
        now_ns,
    );
}

fn arm(
    clock: Rc<RefCell<dyn Clock>>,
    cache: CacheView,
    exec: Exec,
    trader_id: TraderId,
    now_ns: u64,
) {
    let at = next_boundary_ns(now_ns);
    let again = (Rc::clone(&clock), cache.clone(), Rc::clone(&exec));
    let callback: Rc<dyn Fn(TimeEvent)> = Rc::new(move |_event| {
        let (clock, cache, exec) = &again;
        // Everything a boundary decides — the gate, the expiry sweep, the
        // cancel stamps and the next alert — reads the clock's own instant, not
        // the alert's: one clock step can cross several boundaries (a jump, a
        // suspended laptop) and only one alert is ever armed, so the alert that
        // fires says nothing about which boundaries were passed.
        let now = clock.borrow().timestamp_ns().as_u64();
        boundary(cache, exec, trader_id, now);
        arm(
            Rc::clone(clock),
            cache.clone(),
            Rc::clone(exec),
            trader_id,
            now,
        );
    });
    if let Err(e) = clock.borrow_mut().set_time_alert_ns(
        ALERT,
        UnixNanos::from(at),
        Some(TimeEventCallback::from(callback)),
        Some(false),
    ) {
        tracing::error!("the CN session alert could not be armed: {e}");
    }
}

/// What a boundary does, on the node thread and without a quote: re-gate every
/// CN instrument, and terminate every CN order that has outlived its trading
/// day (§5.1).
///
/// The sweep runs at *every* boundary, 09:30 included, and asks [`expired`] per
/// order rather than asking which boundary fired: a clock that stepped from
/// 11:00 to 15:30, or a daemon that slept overnight, fires whichever single
/// alert was armed, and the orders it stepped over must still end their day.
fn boundary(cache: &CacheView, exec: &Exec, trader_id: TraderId, now_ns: u64) {
    {
        let mut exec = exec.borrow_mut();
        for entry in crate::feed::cn_entries() {
            gate(&mut exec, entry, now_ns);
        }
        if !exec.sweeps() {
            return;
        }
    }
    // The full order list filtered `!is_closed()`, not `orders_open`, which
    // excludes `INITIALIZED` — the same reason [`sellable`] reads it that way.
    let expiring: Vec<OrderAny> = cache
        .borrow()
        .orders(None, None, None, None, None)
        .into_iter()
        .filter(|order| {
            !order.is_closed()
                && crate::catalog::find(order.instrument_id().to_string().as_str())
                    .is_some_and(|entry| entry.market == crate::catalog::Market::Cn)
                && expired(order, now_ns)
        })
        .map(|order| order.cloned())
        .collect();
    for order in &expiring {
        crate::trade::cancel_on_node(trader_id, order, now_ns);
    }
}

// ---------------------------------------------------------------------------
// Checks (feature SPEC `a-share-engine` §6: A1 eligibility, A2 quantities,
// A3 fill policy, A4 session and expiry, A6 sampled execution)
//
// Every execution check runs the real `LiveNode` and the real sandbox on the
// controlled clock, publishing through the production publisher.
// ---------------------------------------------------------------------------

#[cfg(test)]
use crate::node::{
    CN_0935, DAY_NS, Registry, SECOND_NS, controlled_registry, fill_count, order_status, publish,
    reservation, within,
};
#[cfg(test)]
use crate::store::Store;
#[cfg(test)]
use crate::trade::{self, TradeError};
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
fn pingan() -> &'static Entry {
    crate::catalog::find("000001.XSHE").expect("the CN catalog entry")
}

/// A started desk on the controlled clock, running the HiThink fill policy —
/// the mode every AE-9 check is about. No feed: every observation is handed to
/// the production rule by [`observe_on_node`].
#[cfg(test)]
fn desk(
    store: &Store,
    name: &'static str,
    start_ns: u64,
) -> (Registry, crate::node::ClockHandle, Arc<Node>) {
    let (registry, handle) = controlled_registry(store, None, name, start_ns);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    node.call(|context| {
        let mut exec = context.cn.borrow_mut();
        exec.feed = AShareFeed::Hithink;
        // A CN desk under the production day-lifetime rule.
        exec.sweep = true;
    })
    .expect("the node answers");
    (registry, handle, node)
}

/// One HiThink snapshot item, as `crate::feed::poll_hithink_observed` parses it.
#[cfg(test)]
fn snapshot(entry: &'static Entry, last: &str, prev_close: &str, volume: u64) -> Observed {
    Observed {
        entry,
        last: last.parse().expect("decimal text"),
        prev_close: prev_close.parse().expect("decimal text"),
        volume: Decimal::from(volume),
        ok: Ok(()),
        accepted: Some(last.to_owned()),
        received_at_ns: 0,
    }
}

/// Hands one item to the production observation path — the very call `cn_cycle`
/// makes — on the node thread, without waiting for it. The answer fires when
/// that call has returned; a caller that does not care drops it.
#[cfg(test)]
fn observation(
    node: &Node,
    observed: Observed,
    evidence: Result<(), Reason>,
    at_ns: u64,
) -> std::sync::mpsc::Receiver<()> {
    let (applied, done) = std::sync::mpsc::channel();
    node.call(move |context| {
        let cache = CacheView::new(Rc::clone(&context.cache));
        let exec = Rc::clone(&context.cn);
        tokio::task::spawn_local(async move {
            cycle_observation(&observed, evidence, &cache, &exec, at_ns).await;
            let _ = applied.send(());
        });
    })
    .expect("the node answers");
    done
}

/// [`observation`], waiting for the cycle itself to have run — state the node
/// already held (an unready instrument, a stamp from an earlier observation)
/// says nothing about whether *this* item was applied, so nothing here infers
/// it. The cycle's own publications are bounded by [`WAIT_TIMEOUT`].
#[cfg(test)]
#[track_caller]
fn observe_on_node(node: &Node, observed: Observed, evidence: Result<(), Reason>, at_ns: u64) {
    observation(node, observed, evidence, at_ns)
        .recv_timeout(Duration::from_secs(10))
        .expect("the observation's cycle ran");
}

/// One order through the production submit path.
#[cfg(test)]
#[expect(clippy::too_many_arguments, reason = "one call shape for every check")]
fn order(
    store: &Store,
    registry: &Registry,
    desk_id: &str,
    action_id: &str,
    instrument_id: &str,
    side: &str,
    quantity: u32,
    price: Option<&str>,
) -> Result<Value, TradeError> {
    let (kind, price) = match price {
        Some(price) => ("LIMIT", format!("\"{price}\"")),
        None => ("MARKET", "null".to_owned()),
    };
    let body = format!(
        r#"{{"action_id":"{action_id}","instrument_id":"{instrument_id}","side":"{side}",
            "type":"{kind}","quantity":"{quantity}","price":{price}}}"#
    );
    trade::submit(store, registry, desk_id, &body, &trade::Source::Session)
        .map(|(record, _)| record.outcome.expect("a placed order answers"))
}

/// Every captured fill for one order: `(quantity, price, commission)`, exactly
/// as NautilusTrader emitted them.
#[cfg(test)]
fn fills(store: &Store, client_order_id: &str) -> Vec<(String, String, String)> {
    let order = client_order_id.to_owned();
    store
        .call(move |conn| {
            conn.prepare(
                "SELECT quantity, price, commission FROM fills WHERE client_order_id = ?1 \
                 ORDER BY occurred_at_ns, id",
            )?
            .query_map([order], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
        })
        .expect("the fills read")
}

#[cfg(test)]
fn liquidity(store: &Store, client_order_id: &str) -> String {
    let order = client_order_id.to_owned();
    store
        .call(move |conn| {
            conn.query_row(
                "SELECT payload FROM order_events WHERE client_order_id = ?1 AND kind = 'OrderFilled'",
                [order],
                |r| r.get::<_, String>(0),
            )
        })
        .expect("the fill payload")
}

// ---------------------------------------------------------------------------
// A4 — the session window and the day lifetime, as arithmetic
// ---------------------------------------------------------------------------

#[cfg(test)]
/// §2.1: rollover invalidates readiness. Evidence established on one Shanghai
/// day reads `NO_CALENDAR` the next day until a poll re-establishes it, so a
/// boundary alert or an admission on the new day never trusts yesterday's band.
#[test]
fn readiness_does_not_survive_the_shanghai_rollover() {
    let mut inst = Inst::default();
    assert_eq!(
        inst.ready_at(CN_0935),
        Ok(()),
        "no day claim: read as it stands"
    );
    inst.readiness = Ok(());
    inst.ready_date = Some(shanghai_date(CN_0935));
    assert_eq!(
        inst.ready_at(CN_0935 + 6 * 3_600 * SECOND_NS),
        Ok(()),
        "same day"
    );
    assert_eq!(
        inst.ready_at(CN_0935 + DAY_NS),
        Err(Reason::NoCalendar),
        "next day"
    );
    inst.readiness = Err(Reason::FeedLost);
    assert_eq!(
        inst.ready_at(CN_0935),
        Err(Reason::FeedLost),
        "a block wins"
    );
}

#[test]
fn session_window_is_the_execution_window() {
    // 09:35 is inside the morning session; 11:30 and 14:57 are already out,
    // because both intervals are half-open.
    assert!(in_session(CN_0935));
    assert!(!in_session(CN_0935 + 6_900 * SECOND_NS), "11:30");
    assert!(in_session(CN_0935 + 6_899 * SECOND_NS), "11:29:59");
    assert!(!in_session(CN_0935 + 9_300 * SECOND_NS), "12:10, lunch");
    assert!(in_session(CN_0935 + 12_300 * SECOND_NS), "13:00");
    assert!(!in_session(CN_0935 + 19_320 * SECOND_NS), "14:57");
    assert!(in_session(CN_0935 + 19_319 * SECOND_NS), "14:56:59");

    // The midday break pauses; everything else outside a session closes.
    assert_eq!(status_for(CN_0935, true), MarketStatusAction::Trading);
    assert_eq!(status_for(CN_0935, false), MarketStatusAction::Pause);
    assert_eq!(
        status_for(CN_0935 + 9_300 * SECOND_NS, true),
        MarketStatusAction::Pause,
        "lunch"
    );
    assert_eq!(
        status_for(CN_0935 + 19_320 * SECOND_NS, true),
        MarketStatusAction::Close,
        "14:57"
    );
    assert_eq!(
        status_for(CN_0935 - 3_600 * SECOND_NS, true),
        MarketStatusAction::Close,
        "before the open"
    );

    // The deadline and the day start are the instant's own Shanghai date.
    assert_eq!(deadline_ns(CN_0935), CN_0935 + 19_320 * SECOND_NS);
    assert_eq!(
        deadline_ns(CN_0935 + 21_300 * SECOND_NS),
        CN_0935 + 19_320 * SECOND_NS,
        "15:30 still names its own day's 14:57"
    );
    assert_eq!(day_start_ns(CN_0935), CN_0935 - 34_500 * SECOND_NS);
    assert_eq!(shanghai_date(CN_0935), "20260909");

    // A weekend and a night are never sessions, whatever the hour.
    let saturday = CN_0935 + 4 * DAY_NS;
    assert_eq!(shanghai_date(saturday), "20260913");
    assert!(!in_session(saturday));
    assert!(!in_session(CN_0935 + 15 * 3_600 * SECOND_NS), "00:35");

    // The next armed boundary is always the next of the four.
    assert_eq!(next_boundary_ns(CN_0935), CN_0935 + 6_900 * SECOND_NS);
    assert_eq!(
        next_boundary_ns(CN_0935 + 6_900 * SECOND_NS),
        CN_0935 + 12_300 * SECOND_NS
    );
    assert_eq!(
        next_boundary_ns(CN_0935 + 19_320 * SECOND_NS),
        CN_0935 + DAY_NS - 300 * SECOND_NS,
        "after 14:57 the next boundary is tomorrow's 09:30"
    );
}

#[cfg(test)]
#[test]
fn stamps_are_strictly_monotonic() {
    let id = InstrumentId::from("600519.XSHG");
    let mut exec = CnExec::new();
    assert_eq!(exec.stamp(id, 100), 100);
    assert_eq!(
        exec.stamp(id, 100),
        101,
        "a repeated instant still advances"
    );
    assert_eq!(
        exec.stamp(id, 50),
        102,
        "and a backwards one never regresses"
    );
    assert_eq!(exec.stamp(id, 500), 500);
    exec.reset();
    assert_eq!(exec.stamp(id, 50), 50, "a reset forgets the instrument");
}

// ---------------------------------------------------------------------------
// A2 — the board caps (§3.2); the cap table itself is `catalog::band_and_caps`
// ---------------------------------------------------------------------------

/// §1.2's structural half, through the production submit path: a CN BUY is a
/// whole lot, a CN SELL is not held to that rule, and both boards' caps are
/// enforced by order type.
#[cfg(test)]
#[test]
fn cn_structural_quantities_are_lots_and_caps() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-form", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    publish(&node, pingan(), ("9.90", 0), ("9.90", 0), CN_0935);

    let refused = |action: &str, instrument: &str, side: &str, quantity: u32, price| {
        order(
            &store, &registry, &desk_id, action, instrument, side, quantity, price,
        )
        .expect_err("refused")
        .to_string()
    };
    assert_eq!(
        refused("cn-form-1", "000001.XSHE", "BUY", 150, Some("9.00")),
        "The order is not well formed: quantity \"150\" is not a positive multiple \
         of the 100 lot of 000001.XSHE."
    );
    assert_eq!(
        refused("cn-form-2", "000001.XSHE", "SELL", 0, None),
        "The order is not well formed: quantity \"0\" is not a positive whole number \
         of shares of 000001.XSHE."
    );
    assert_eq!(
        refused("cn-form-3", "000001.XSHE", "BUY", 1_000_100, Some("9.00")),
        "The order is not well formed: quantity \"1000100\" exceeds the LIMIT cap \
         of 1000000 shares for 000001.XSHE."
    );
    assert_eq!(
        refused("cn-form-4", "300750.XSHE", "BUY", 300_100, Some("9.00")),
        "The order is not well formed: quantity \"300100\" exceeds the LIMIT cap \
         of 300000 shares for 300750.XSHE."
    );
    assert_eq!(
        refused("cn-form-5", "300750.XSHE", "BUY", 150_100, None),
        "The order is not well formed: quantity \"150100\" exceeds the MARKET cap \
         of 150000 shares for 300750.XSHE."
    );
    // A CN SELL below one lot is structurally fine; §3.1 judges it against the
    // sellable quantity instead.
    assert_eq!(
        refused("cn-form-6", "000001.XSHE", "SELL", 50, None),
        "The order is not well formed: quantity 50 exceeds sellable 0 for \
         000001.XSHE: 0 bought today are locked by T+1; 0 reserved by outstanding sells."
    );
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// A1 — T+1, reservations, the odd remainder, and approval revalidation
// ---------------------------------------------------------------------------

/// §1.1, §3.1: shares bought today are locked until the next Shanghai day; a
/// resting SELL reserves what it has not yet sold; a partial fill reduces the
/// reservation; and only a terminal cancel releases it.
#[cfg(test)]
#[test]
fn todays_buy_is_locked_and_the_next_shanghai_day_sells_it() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-t1", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(pingan().instrument_id);

    publish(&node, pingan(), ("10.00", 300), ("10.00", 300), CN_0935);
    let bought = order(
        &store,
        &registry,
        &desk_id,
        "cn-t1-buy",
        "000001.XSHE",
        "BUY",
        300,
        None,
    )
    .expect("the market buy fills");
    assert_eq!(bought["status"], "FILLED", "{bought}");

    let eligibility = node
        .call(move |context| sellable(&context.cache.borrow(), instrument_id, CN_0935))
        .unwrap();
    assert_eq!(
        eligibility,
        Eligibility {
            position: Decimal::from(300),
            locked: Decimal::from(300),
            reserved: Decimal::ZERO,
            sellable: Decimal::ZERO,
        }
    );
    assert_eq!(
        order(
            &store,
            &registry,
            &desk_id,
            "cn-t1-sell",
            "000001.XSHE",
            "SELL",
            200,
            None
        )
        .expect_err("today's shares are locked")
        .to_string(),
        "The order is not well formed: quantity 200 exceeds sellable 0 for 000001.XSHE: \
         300 bought today are locked by T+1; 0 reserved by outstanding sells."
    );

    // The next Shanghai day: the lock has lapsed, and a resting SELL reserves.
    let next = CN_0935 + DAY_NS;
    node.advance_to(next).expect("the clock advances");
    observe_on_node(
        &node,
        snapshot(pingan(), "10.00", "10.00", 1000),
        Ok(()),
        next,
    );
    let rested = order(
        &store,
        &registry,
        &desk_id,
        "cn-t1-sell-2",
        "000001.XSHE",
        "SELL",
        200,
        Some("10.50"),
    )
    .expect("the next day's sell is accepted");
    assert_eq!(rested["status"], "ACCEPTED", "{rested}");
    let reserved = node
        .call(move |context| sellable(&context.cache.borrow(), instrument_id, next))
        .unwrap();
    assert_eq!(
        (reserved.locked, reserved.reserved, reserved.sellable),
        (Decimal::ZERO, Decimal::from(200), Decimal::from(100))
    );

    // A partial fill reduces the reservation; the cancel releases the rest, and
    // only once the terminal event is in.
    publish(
        &node,
        pingan(),
        ("10.50", 50),
        ("10.50", 0),
        next + SECOND_NS,
    );
    within(10, "the partial fill lands", || {
        order_status(&node, "cn-t1-sell-2").as_deref() == Some("PARTIALLY_FILLED")
    });
    let partial = node
        .call(move |context| sellable(&context.cache.borrow(), instrument_id, next))
        .unwrap();
    assert_eq!(
        (partial.position, partial.reserved, partial.sellable),
        (Decimal::from(250), Decimal::from(150), Decimal::from(100)),
        "the filled 50 left the reservation and the position"
    );
    trade::cancel(
        &store,
        &registry,
        &desk_id,
        "cn-t1-sell-2",
        r#"{"action_id":"cn-t1-undo"}"#,
        &trade::Source::Session,
    )
    .expect("the cancel is accepted");
    let released = node
        .call(move |context| sellable(&context.cache.borrow(), instrument_id, next))
        .unwrap();
    assert_eq!(
        (released.reserved, released.sellable),
        (Decimal::ZERO, Decimal::from(250)),
        "OrderCanceled released the reservation"
    );
    registry.stop_all();
}

/// §1.1: two threads racing the same shares. The eligibility read and the
/// placement are one admission closure, so exactly one is accepted.
#[cfg(test)]
#[test]
fn competing_sells_cannot_reserve_the_same_shares() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-race", CN_0935 - DAY_NS);
    let desk_id = handle.desk_id().to_owned();
    publish(
        &node,
        pingan(),
        ("10.00", 300),
        ("10.00", 300),
        CN_0935 - DAY_NS,
    );
    order(
        &store,
        &registry,
        &desk_id,
        "cn-race-seed",
        "000001.XSHE",
        "BUY",
        300,
        None,
    )
    .expect("yesterday's buy fills");
    node.advance_to(CN_0935).expect("the clock advances");
    publish(&node, pingan(), ("10.00", 0), ("10.00", 0), CN_0935);

    let registry = Arc::new(registry);
    let barrier = std::sync::Barrier::new(2);
    let accepted: Vec<bool> = std::thread::scope(|scope| {
        let racers: Vec<_> = ["cn-race-a", "cn-race-b"]
            .into_iter()
            .map(|action| {
                let (store, registry, desk_id, barrier) =
                    (&store, Arc::clone(&registry), desk_id.clone(), &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    order(
                        store,
                        &registry,
                        &desk_id,
                        action,
                        "000001.XSHE",
                        "SELL",
                        300,
                        Some("10.50"),
                    )
                    .is_ok()
                })
            })
            .collect();
        racers.into_iter().map(|r| r.join().unwrap()).collect()
    });
    assert_eq!(
        accepted.iter().filter(|ok| **ok).count(),
        1,
        "exactly one sell reserved the 300 shares: {accepted:?}"
    );
    registry.stop_all();
}

/// §3.1's odd-remainder table: a SELL is a whole lot, or exactly the remainder
/// the sellable quantity carries — which cannot be split across submissions.
#[cfg(test)]
#[test]
fn the_odd_remainder_is_sold_whole_or_in_lots() {
    let ok = |quantity: i64, sellable: i64| {
        let (quantity, sellable) = (Decimal::from(quantity), Decimal::from(sellable));
        quantity <= sellable
            && (quantity % Decimal::from(LOT) == Decimal::ZERO
                || quantity % Decimal::from(LOT) == sellable % Decimal::from(LOT))
    };
    for quantity in [50, 100, 150, 200, 250] {
        assert!(ok(quantity, 250), "sellable 250 permits {quantity}");
    }
    assert!(!ok(125, 250), "sellable 250 refuses 125");
    assert!(!ok(300, 250), "and refuses more than it holds");
    for quantity in [50, 100, 150] {
        assert!(ok(quantity, 150), "sellable 150 permits {quantity}");
    }
    for quantity in [100, 200] {
        assert!(ok(quantity, 200), "sellable 200 permits {quantity}");
    }
    for quantity in [50, 150] {
        assert!(!ok(quantity, 200), "sellable 200 refuses {quantity}");
    }
}

/// §5.2: an approval whose node is unavailable leaves the action `PENDING`
/// with no decision recorded; an approval whose eligibility has lapsed resolves
/// as a terminal refused action with no sandbox order.
#[cfg(test)]
#[test]
fn an_approval_reruns_the_execution_checks() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-approval", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(pingan().instrument_id);
    publish(&node, pingan(), ("9.90", 0), ("9.90", 0), CN_0935);
    store
        .unit(|tx| {
            tx.execute(
                "UPDATE installation_settings SET paper_order_policy = 'REQUIRE_APPROVAL' \
                 WHERE id = 1",
                [],
            )
        })
        .expect("the policy a `PUT /settings/policies` would have written");

    let (pending, submitted) = trade::submit(
        &store,
        &registry,
        &desk_id,
        r#"{"action_id":"cn-approval-1","instrument_id":"000001.XSHE",
            "side":"BUY","type":"LIMIT","quantity":"100","price":"10.00"}"#,
        &trade::Source::Session,
    )
    .expect("a gated order is recorded, not refused");
    assert_eq!(submitted, trade::Submitted::Pending);

    // The instrument becomes unexecutable under the pending action.
    node.call(move |context| {
        context.cn.borrow_mut().inst(instrument_id).readiness = Err(Reason::FeedLost);
    })
    .unwrap();
    let blocked = trade::decide(
        &store,
        &registry,
        &desk_id,
        &pending.id,
        crate::policy::Decision::Approve,
    )
    .expect_err("an unavailable instrument cannot decide the action");
    assert_eq!(blocked.code(), "MARKET_UNAVAILABLE");
    let row = |action: &str| {
        trade::history_actions(&store, &desk_id)
            .expect("the actions read")
            .into_iter()
            .find(|row| row.action_id == action)
            .expect("the row")
    };
    assert_eq!(
        row("cn-approval-1").approval,
        "PENDING",
        "no approval decision was recorded"
    );

    // Recovery, then an approval whose eligibility has lapsed: a SELL of shares
    // the desk does not own.
    node.call(move |context| context.cn.borrow_mut().inst(instrument_id).readiness = Ok(()))
        .unwrap();
    let (sell, _) = trade::submit(
        &store,
        &registry,
        &desk_id,
        r#"{"action_id":"cn-approval-2","instrument_id":"000001.XSHE",
            "side":"SELL","type":"LIMIT","quantity":"100","price":"10.00"}"#,
        &trade::Source::Session,
    )
    .expect("a gated sell is recorded");
    trade::decide(
        &store,
        &registry,
        &desk_id,
        &sell.id,
        crate::policy::Decision::Approve,
    )
    .expect("an eligibility refusal is not a failed decision");
    let refused = row("cn-approval-2");
    assert_eq!(refused.approval, "APPROVED", "the decision itself stands");
    let outcome = refused.outcome.clone().expect("a terminal outcome");
    assert_eq!(outcome["failure_code"], "ORDER_INVALID", "{outcome}");
    assert_eq!(
        outcome["reason"],
        "quantity 100 exceeds sellable 0 for 000001.XSHE: 0 bought today are locked \
         by T+1; 0 reserved by outstanding sells",
        "{outcome}"
    );
    assert!(
        order_status(&node, "cn-approval-2").is_none(),
        "and no sandbox order was created"
    );
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// A6 — the sampled LIMIT trigger (§2.5, AE-9)
// ---------------------------------------------------------------------------

/// AE-9, both directions: an admitted LIMIT waits for an observation whose
/// volume exceeds both its own baseline and the preceding accepted observation,
/// and whose price crosses it; then the whole remaining quantity fills at the
/// order's own limit price, as MAKER, inside the band.
#[cfg(test)]
#[test]
fn a_limit_fills_only_on_a_qualifying_observation() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-ae9", CN_0935 - DAY_NS);
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 - DAY_NS + SECOND_NS;

    // Day D−1: the inventory the SELL leg needs.
    publish(&node, pingan(), ("10.00", 100), ("10.00", 100), at);
    order(
        &store,
        &registry,
        &desk_id,
        "cn-ae9-seed",
        "000001.XSHE",
        "BUY",
        100,
        None,
    )
    .expect("yesterday's buy fills");

    // Day D, observation 1 — last 9.90, cumulative volume 1000.
    at = CN_0935 + SECOND_NS;
    node.advance_to(at).expect("the clock advances");
    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 1000), Ok(()), at);

    let buy = order(
        &store,
        &registry,
        &desk_id,
        "cn-ae9-buy",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the compatible limit buy is accepted");
    assert_eq!(
        buy["status"], "ACCEPTED",
        "even a compatible LIMIT rests: {buy}"
    );
    let sell = order(
        &store,
        &registry,
        &desk_id,
        "cn-ae9-sell",
        "000001.XSHE",
        "SELL",
        100,
        Some("9.80"),
    )
    .expect("the compatible limit sell is accepted");
    assert_eq!(sell["status"], "ACCEPTED", "{sell}");

    // Observation 2 — 9.95 / volume 1000: compatible both ways, equal volume.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.95", "10.00", 1000), Ok(()), at);
    assert!(
        fills(&store, "cn-ae9-buy").is_empty() && fills(&store, "cn-ae9-sell").is_empty(),
        "equal volume never triggers"
    );

    // Observation 3 — 9.95 / volume 1001: qualifying on both sides.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.95", "10.00", 1001), Ok(()), at);
    within(10, "both resting orders fill", || {
        order_status(&node, "cn-ae9-buy").as_deref() == Some("FILLED")
            && order_status(&node, "cn-ae9-sell").as_deref() == Some("FILLED")
    });
    assert_eq!(
        fills(&store, "cn-ae9-buy"),
        vec![("100".to_owned(), "10.00".to_owned(), "0.30".to_owned())],
        "the full remaining quantity at the order's own limit price, 3 bp"
    );
    assert_eq!(
        fills(&store, "cn-ae9-sell"),
        vec![("100".to_owned(), "9.80".to_owned(), "0.29".to_owned())],
    );
    assert!(
        liquidity(&store, "cn-ae9-buy").contains(r#""liquidity_side":"MAKER""#),
        "the resting order is the maker"
    );

    // Every fill is inside the day's band, and the book is idle again.
    let (up, down) = crate::catalog::band(
        Decimal::from(10),
        Decimal::new(1, 2),
        crate::catalog::Board::Main,
    );
    for (_, price, _) in fills(&store, "cn-ae9-buy")
        .into_iter()
        .chain(fills(&store, "cn-ae9-sell"))
    {
        let price: Decimal = price.parse().unwrap();
        assert!(
            price <= up && price >= down,
            "{price} is inside [{down},{up}]"
        );
    }
    let idle = node
        .call(|context| {
            let cache = context.cache.borrow();
            let quote = cache
                .quote(&InstrumentId::from(pingan().instrument_id))
                .expect("a quote");
            (quote.bid_size.as_decimal(), quote.ask_size.as_decimal())
        })
        .unwrap();
    assert_eq!(
        idle,
        (Decimal::ZERO, Decimal::ZERO),
        "the idle book is back"
    );

    let history = trade::history_orders(&store, &desk_id).expect("the history reads");
    assert!(
        history
            .iter()
            .filter(
                |o| o["client_order_id"] == "cn-ae9-buy" || o["client_order_id"] == "cn-ae9-sell"
            )
            .all(|o| o["status"] == "FILLED"),
        "{history:?}"
    );
    registry.stop_all();
}

/// AE-9's temporal rules that are not the happy path: an observation received
/// before admission cannot trigger the order; increasing volume with an
/// incompatible price advances the preceding-observation baseline without
/// filling; two orders admitted at different baselines are released
/// independently; and a cumulative-volume decrease pauses and re-baselines.
#[cfg(test)]
#[test]
fn baselines_are_per_order_and_a_volume_decrease_resets_them() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-baseline", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(pingan().instrument_id);
    let mut at = CN_0935 + SECOND_NS;

    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 1000), Ok(()), at);
    // A second observation *before* admission: more volume, compatible price.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 1100), Ok(()), at);

    order(
        &store,
        &registry,
        &desk_id,
        "cn-baseline-a",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the limit buy rests");
    assert_eq!(
        node.call(move |context| context
            .cn
            .borrow_mut()
            .baseline_of(instrument_id, "cn-baseline-a"))
            .unwrap(),
        Some(Baseline {
            seq: 2,
            volume: Decimal::from(1100)
        }),
        "admission takes the node's latest accepted observation"
    );

    // The pre-admission observation's own volume cannot trigger it.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 1100), Ok(()), at);
    assert_eq!(
        fill_count(&store),
        0,
        "an equal-volume re-read never triggers"
    );

    // More volume, incompatible price (10.50 > the 10.00 limit): no fill, and
    // the preceding-observation baseline advances anyway.
    at += SECOND_NS;
    observe_on_node(
        &node,
        snapshot(pingan(), "10.50", "10.00", 1200),
        Ok(()),
        at,
    );
    assert_eq!(fill_count(&store), 0, "an incompatible price never fills");
    // A second order, admitted against that advanced baseline.
    order(
        &store,
        &registry,
        &desk_id,
        "cn-baseline-b",
        "000001.XSHE",
        "BUY",
        100,
        Some("9.50"),
    )
    .expect("the second limit buy rests");

    // A volume decrease: pause, discard, and no fill on the reset itself.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.40", "10.00", 900), Ok(()), at);
    assert_eq!(fill_count(&store), 0, "no fill on the reset");
    let after = node
        .call(move |context| {
            let mut exec = context.cn.borrow_mut();
            let inst = exec.inst(instrument_id);
            (inst.prev_volume, inst.status)
        })
        .unwrap();
    assert_eq!(
        after,
        (None, Some(MarketStatusAction::Pause)),
        "the temporal baselines are discarded and the instrument paused"
    );

    // The next valid observation only re-establishes, even with more volume
    // than either discarded baseline and a price compatible with both orders.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.40", "10.00", 5000), Ok(()), at);
    assert_eq!(
        fill_count(&store),
        0,
        "the reset observation re-baselines only"
    );

    // And the one after it releases both, each at its own limit price.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.40", "10.00", 5001), Ok(()), at);
    within(10, "both orders fill at their own limits", || {
        order_status(&node, "cn-baseline-a").as_deref() == Some("FILLED")
            && order_status(&node, "cn-baseline-b").as_deref() == Some("FILLED")
    });
    assert_eq!(
        fills(&store, "cn-baseline-a")[0].1,
        "10.00",
        "each at its own limit"
    );
    assert_eq!(fills(&store, "cn-baseline-b")[0].1, "9.50");
    registry.stop_all();
}

/// §2.3: at the upper limit no BUY crosses, at the lower limit no SELL does,
/// and the opposite direction is untouched. §2.4: a LIMIT priced outside the
/// inclusive band is refused, naming the reference, date and bounds.
#[cfg(test)]
#[test]
fn direction_suppression_and_the_band_hold_both_boundaries() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-band", CN_0935 - DAY_NS);
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 - DAY_NS + SECOND_NS;
    publish(&node, pingan(), ("10.00", 200), ("10.00", 200), at);
    order(
        &store,
        &registry,
        &desk_id,
        "cn-band-seed",
        "000001.XSHE",
        "BUY",
        200,
        None,
    )
    .expect("yesterday's buy fills");

    at = CN_0935 + SECOND_NS;
    node.advance_to(at).expect("the clock advances");
    observe_on_node(
        &node,
        snapshot(pingan(), "10.00", "10.00", 1000),
        Ok(()),
        at,
    );

    // The band around 10.00 on the main board is [9.00, 11.00], inclusive.
    assert_eq!(
        order(
            &store,
            &registry,
            &desk_id,
            "cn-band-out",
            "000001.XSHE",
            "BUY",
            100,
            Some("11.01"),
        )
        .expect_err("outside the band")
        .to_string(),
        "The order is not well formed: price 11.01 for 000001.XSHE is outside the \
         20260909 band [9.00, 11.00] around reference 10.00."
    );
    for (action, side, price) in [
        ("cn-band-buy", "BUY", "11.00"),
        ("cn-band-sell", "SELL", "9.00"),
    ] {
        let accepted = order(
            &store,
            &registry,
            &desk_id,
            action,
            "000001.XSHE",
            side,
            100,
            Some(price),
        )
        .expect("a boundary-priced LIMIT is admitted");
        assert_eq!(accepted["status"], "ACCEPTED", "{accepted}");
    }

    // The last runs to the upper limit: the BUY is suppressed, the SELL is not.
    at += SECOND_NS;
    observe_on_node(
        &node,
        snapshot(pingan(), "11.00", "10.00", 1001),
        Ok(()),
        at,
    );
    within(10, "the sell fills at the upper limit", || {
        order_status(&node, "cn-band-sell").as_deref() == Some("FILLED")
    });
    assert_eq!(
        order_status(&node, "cn-band-buy").as_deref(),
        Some("ACCEPTED"),
        "no BUY crosses at the upper limit"
    );

    // And to the lower limit, where the mirror holds: the SELL is gone, so the
    // remaining BUY is the one that must not be suppressed.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.00", "10.00", 1002), Ok(()), at);
    within(10, "the buy fills at the lower limit", || {
        order_status(&node, "cn-band-buy").as_deref() == Some("FILLED")
    });
    assert_eq!(
        fills(&store, "cn-band-buy")[0].1,
        "11.00",
        "at its own limit price"
    );
    registry.stop_all();
}

/// §2.5, §5.3: a restart, a feed loss and a provider switch each discard the
/// temporal baselines, and the first observation after any of them only
/// re-establishes — even when it carries more volume than the discarded
/// baseline and crosses the restored order.
#[cfg(test)]
#[test]
fn recovery_re_baselines_without_filling() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-recovery", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let mut at = CN_0935 + SECOND_NS;

    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 1000), Ok(()), at);
    order(
        &store,
        &registry,
        &desk_id,
        "cn-recovery-1",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the limit buy rests");
    registry.stop_all();

    // --- the restart -------------------------------------------------------
    let (registry, node) = crate::node::restart_at(&store, &desk_id, CN_0935 + 10 * SECOND_NS);
    node.call(|context| context.cn.borrow_mut().feed = AShareFeed::Hithink)
        .unwrap();
    assert_eq!(
        order_status(&node, "cn-recovery-1").as_deref(),
        Some("ACCEPTED"),
        "the same-day order came back resting"
    );
    at = CN_0935 + 20 * SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 1002), Ok(()), at);
    assert_eq!(
        fill_count(&store),
        0,
        "the first post-restart observation only re-establishes"
    );

    // --- a feed failure ----------------------------------------------------
    at += SECOND_NS;
    node.call(move |context| feed_failed(&context.cn, &[pingan()], Reason::FeedLost, at))
        .unwrap();
    assert_eq!(
        order(
            &store,
            &registry,
            &desk_id,
            "cn-recovery-2",
            "000001.XSHE",
            "BUY",
            100,
            Some("10.00"),
        )
        .expect_err("a lost feed admits nothing")
        .code(),
        "MARKET_UNAVAILABLE"
    );
    // A qualifying observation while blocked releases nothing.
    at += SECOND_NS;
    observe_on_node(
        &node,
        snapshot(pingan(), "9.90", "10.00", 2000),
        Err(Reason::FeedLost),
        at,
    );
    assert_eq!(
        fill_count(&store),
        0,
        "a blocked instrument matches nothing"
    );

    // Recovery re-establishes and still does not fill.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 3000), Ok(()), at);
    assert_eq!(fill_count(&store), 0, "recovery re-baselines");
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 3001), Ok(()), at);
    within(
        10,
        "the first qualifying observation after recovery fills",
        || order_status(&node, "cn-recovery-1").as_deref() == Some("FILLED"),
    );
    assert_eq!(fills(&store, "cn-recovery-1")[0].1, "10.00");
    registry.stop_all();
}

/// §5.3: a provider switch blocks, clears the stale executable state with a
/// monotonic stamp, discards the baselines, and only then lets the new
/// provider's observations through — never a fill off the old book.
#[cfg(test)]
#[test]
fn a_provider_switch_never_fills_off_the_old_book() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-switch", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let at = CN_0935 + SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 1000), Ok(()), at);
    order(
        &store,
        &registry,
        &desk_id,
        "cn-switch-1",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the limit buy rests");

    // The switch, as `cn_cycle` performs it.
    let forgotten = node
        .call(move |context| {
            switch_provider(
                &context.cn,
                &[pingan()],
                AShareFeed::Yahoo,
                CN_0935 + 2 * SECOND_NS,
            );
            context
                .cn
                .borrow_mut()
                .inst(InstrumentId::from(pingan().instrument_id))
                .prev_volume
        })
        .unwrap();
    assert!(
        forgotten.is_none(),
        "the switch discarded the temporal baselines"
    );
    assert_eq!(fill_count(&store), 0);

    // Yahoo's first quote, stamped older than the last HiThink receipt — the F5
    // hazard. The cleared book has nothing to match.
    publish(
        &node,
        pingan(),
        ("9.50", 100),
        ("9.50", 100),
        CN_0935 - 60 * SECOND_NS,
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fill_count(&store), 0, "no fill off the old provider's book");
    assert_eq!(
        order_status(&node, "cn-switch-1").as_deref(),
        Some("ACCEPTED")
    );
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// A3 — the MARKET exception (§2.5)
// ---------------------------------------------------------------------------

/// AE-9's MARKET exception: the publication is sized for the request and for
/// every compatible resting LIMIT, so the MARKET fills its whole quantity at
/// last with no remainder slippage, the compatible resting LIMIT fills first at
/// its own limit price, and a price-incompatible one is untouched.
#[cfg(test)]
#[test]
fn a_market_fills_at_last_after_the_compatible_resting_limits() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-market", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let at = CN_0935 + SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.95", "10.00", 1000), Ok(()), at);

    for (action, price) in [("cn-market-hi", "10.00"), ("cn-market-lo", "9.50")] {
        let rested = order(
            &store,
            &registry,
            &desk_id,
            action,
            "000001.XSHE",
            "BUY",
            100,
            Some(price),
        )
        .expect("the limit buy rests");
        assert_eq!(rested["status"], "ACCEPTED", "{rested}");
    }

    let filled = order(
        &store,
        &registry,
        &desk_id,
        "cn-market-1",
        "000001.XSHE",
        "BUY",
        300,
        None,
    )
    .expect("the market buy is accepted");
    assert_eq!(filled["status"], "FILLED", "{filled}");
    assert_eq!(
        fills(&store, "cn-market-1"),
        vec![("300".to_owned(), "9.95".to_owned(), "0.90".to_owned())],
        "the whole quantity at last, no slip"
    );
    assert!(
        liquidity(&store, "cn-market-1").contains(r#""liquidity_side":"TAKER""#),
        "the MARKET takes"
    );
    assert_eq!(
        fills(&store, "cn-market-hi"),
        vec![("100".to_owned(), "10.00".to_owned(), "0.30".to_owned())],
        "the compatible resting LIMIT filled first, at its own limit price"
    );
    assert_eq!(
        order_status(&node, "cn-market-lo").as_deref(),
        Some("ACCEPTED"),
        "the price-incompatible resting LIMIT is untouched"
    );

    // The idle book is restored and the instrument released.
    let released = node
        .call(|context| {
            let cache = context.cache.borrow();
            let quote = cache
                .quote(&InstrumentId::from(pingan().instrument_id))
                .expect("a quote");
            let busy = context
                .cn
                .borrow_mut()
                .inst(InstrumentId::from(pingan().instrument_id))
                .busy;
            (quote.ask_size.as_decimal(), busy)
        })
        .unwrap();
    assert_eq!(released, (Decimal::ZERO, false));
    registry.stop_all();
}

/// §2.5, F8 item 2: the critical section holds in both directions. While a
/// MARKET's sized book stands — from the sizing publication until the idle book
/// is restored — a poll cycle observing the same instrument publishes nothing
/// over it and advances no baseline; once the instrument is released the very
/// next cycle publishes again, and a MARKET still fills its whole quantity.
#[cfg(test)]
#[test]
fn a_poll_cycle_never_republishes_over_a_sized_market_book() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-busy", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(pingan().instrument_id);
    let at = CN_0935 + SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.95", "10.00", 1000), Ok(()), at);
    order(
        &store,
        &registry,
        &desk_id,
        "cn-busy-rest",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the compatible limit buy rests");

    // The MARKET's own half of the section: the sized publication, which owns
    // the instrument until `restore_idle`.
    let intent = Intent {
        entry: pingan(),
        side: OrderSide::Buy,
        quantity: Decimal::from(300),
        price: None,
    };
    let sent = node
        .call(move |context| size_for_market(context, intent, at + SECOND_NS))
        .expect("the node answers")
        .expect("a sized book");
    confirm(&node, instrument_id, sent).expect("the sized book landed");

    // A poll cycle for the same instrument, with a qualifying observation.
    observation(
        &node,
        snapshot(pingan(), "9.90", "10.00", 5000),
        Ok(()),
        at + 2 * SECOND_NS,
    );
    thread::sleep(Duration::from_millis(100));
    let held = node
        .call(move |context| {
            let sizes = {
                let cache = context.cache.borrow();
                let quote = cache.quote(&instrument_id).expect("a quote");
                (
                    quote.ts_event.as_u64(),
                    quote.bid_size.as_decimal(),
                    quote.ask_size.as_decimal(),
                )
            };
            let mut exec = context.cn.borrow_mut();
            let inst = exec.inst(instrument_id);
            (sizes, inst.seq, inst.prev_volume)
        })
        .expect("the node answers");
    assert_eq!(
        held,
        (
            (sent, Decimal::ZERO, Decimal::from(400)),
            1,
            Some(Decimal::from(1000))
        ),
        "the sized book stands untouched and no baseline advanced"
    );

    // Released, the next cycle publishes again.
    restore_idle(&node, pingan(), Decimal::new(995, 2), sent + 1).expect("the idle book is back");
    observe_on_node(
        &node,
        snapshot(pingan(), "9.95", "10.00", 5001),
        Ok(()),
        at + 3 * SECOND_NS,
    );
    assert_eq!(
        node.call(move |context| context.cn.borrow_mut().inst(instrument_id).seq)
            .expect("the node answers"),
        2,
        "the released instrument is observed again"
    );

    // And the MARKET still fills its whole requested quantity at last.
    let filled = order(
        &store,
        &registry,
        &desk_id,
        "cn-busy-market",
        "000001.XSHE",
        "BUY",
        300,
        None,
    )
    .expect("the market buy is accepted");
    assert_eq!(filled["status"], "FILLED", "{filled}");
    assert_eq!(
        fills(&store, "cn-busy-market"),
        vec![("300".to_owned(), "9.95".to_owned(), "0.90".to_owned())],
        "the whole quantity at last, no slip"
    );
    registry.stop_all();
}

/// §2.3, §2.5: a MARKET into a suppressed side gets the sandbox's own
/// no-market rejection; a MARKET the risk engine denies leaves the resting
/// fills its own publication produced in place, and the idle book restored.
#[cfg(test)]
#[test]
fn a_suppressed_or_denied_market_still_restores_the_idle_book() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-market-deny", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(pingan().instrument_id);
    let mut at = CN_0935 + SECOND_NS;

    // At the upper limit the ask is suppressed: the BUY has no market.
    observe_on_node(
        &node,
        snapshot(pingan(), "11.00", "10.00", 1000),
        Ok(()),
        at,
    );
    let refused = order(
        &store,
        &registry,
        &desk_id,
        "cn-market-up",
        "000001.XSHE",
        "BUY",
        100,
        None,
    )
    .expect_err("a market buy at the upper limit is refused");
    let TradeError::Rejected(reason) = &refused else {
        panic!("the sandbox refused natively: {refused:?}");
    };
    assert_eq!(reason, "No market for 000001.XSHE");

    // Insufficient cash: the resting LIMIT the sizing publish filled keeps its
    // fill, and nothing is rolled back.
    at += SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.95", "10.00", 1001), Ok(()), at);
    order(
        &store,
        &registry,
        &desk_id,
        "cn-market-rest",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the limit buy rests");
    let denied = order(
        &store,
        &registry,
        &desk_id,
        "cn-market-cash",
        "000001.XSHE",
        "BUY",
        100_000,
        None,
    )
    .expect_err("100,000 shares at 9.95 exceed the desk's free cash");
    assert_eq!(denied.code(), "ORDER_REJECTED", "{denied}");
    assert_eq!(
        fills(&store, "cn-market-rest"),
        vec![("100".to_owned(), "10.00".to_owned(), "0.30".to_owned())],
        "the resting order's own fill stands after the denial"
    );
    assert!(fills(&store, "cn-market-cash").is_empty());

    let released = node
        .call(move |context| {
            let cache = context.cache.borrow();
            let quote = cache.quote(&instrument_id).expect("a quote");
            let busy = context.cn.borrow_mut().inst(instrument_id).busy;
            (
                quote.bid_size.as_decimal(),
                quote.ask_size.as_decimal(),
                busy,
            )
        })
        .unwrap();
    assert_eq!(
        released,
        (Decimal::ZERO, Decimal::ZERO, false),
        "the idle book is restored on every path, denial included"
    );
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// A4 — the boundary alerts (§5.1, AE-7)
// ---------------------------------------------------------------------------

/// §5.1: the day lifetime ends at 14:57 on a kernel-clock alert, with no quote
/// — the reservation is released, a later crossing observation fills nothing,
/// and the US venue is untouched throughout. The 11:30 alert pauses without
/// expiring anything, and 13:00 resumes.
#[cfg(test)]
#[test]
fn the_boundary_alerts_pause_at_lunch_and_cancel_at_1457() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-boundary", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(pingan().instrument_id);
    let apple = crate::catalog::find("AAPL.XNAS").expect("the US catalog entry");
    let mut at = CN_0935 + SECOND_NS;

    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 1000), Ok(()), at);
    order(
        &store,
        &registry,
        &desk_id,
        "cn-boundary-1",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the limit buy rests");
    assert_eq!(
        reservation(&node, "XSHE", &desk_id),
        ("500000.00 CNY|1000.00 CNY|499000.00 CNY".to_owned(), 1)
    );
    publish(&node, apple, ("310.00", 0), ("310.00", 0), at);
    order(
        &store,
        &registry,
        &desk_id,
        "us-boundary-1",
        "AAPL.XNAS",
        "BUY",
        10,
        Some("300.00"),
    )
    .expect("the US limit buy rests");

    // 11:30 — the alert pauses without expiring anything, and a crossing
    // observation during the break matches nothing.
    let fired = node
        .advance_to(CN_0935 + 6_900 * SECOND_NS)
        .expect("the clock advances");
    assert!(fired.iter().any(|name| name == ALERT), "{fired:?}");
    at = CN_0935 + 6_960 * SECOND_NS;
    observe_on_node(&node, snapshot(pingan(), "9.90", "10.00", 5000), Ok(()), at);
    assert_eq!(fill_count(&store), 0, "the lunch gate holds the crossing");
    assert_eq!(
        order_status(&node, "cn-boundary-1").as_deref(),
        Some("ACCEPTED"),
        "lunch suspends fills without expiring orders"
    );

    // 13:00 — the alert resumes, and 14:57 terminates the day, with no quote at
    // all between them.
    let fired = node
        .advance_to(CN_0935 + 12_300 * SECOND_NS)
        .expect("the clock advances");
    assert!(fired.iter().any(|name| name == ALERT), "{fired:?}");
    assert_eq!(
        node.call(move |context| context.cn.borrow_mut().inst(instrument_id).status)
            .unwrap(),
        Some(MarketStatusAction::Trading),
        "the afternoon reopens"
    );
    let deadline = CN_0935 + 19_320 * SECOND_NS;
    let fired = node.advance_to(deadline).expect("the clock advances");
    assert!(fired.iter().any(|name| name == ALERT), "{fired:?}");
    within(10, "the day lifetime ended without a tick", || {
        order_status(&node, "cn-boundary-1").as_deref() == Some("CANCELED")
    });
    assert_eq!(
        reservation(&node, "XSHE", &desk_id),
        ("500000.00 CNY|0.00 CNY|500000.00 CNY".to_owned(), 0),
        "OrderCanceled released the reservation"
    );
    let terminal = crate::node::kinds(&store, &desk_id, "cn-boundary-1");
    assert_eq!(
        terminal,
        [
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ],
        "one chain, one terminal event"
    );

    // A later crossing quote fills nothing, and a new CN order is refused; the
    // US order is neither cancelled nor gated, and its own quote still fills it.
    publish(
        &node,
        pingan(),
        ("9.50", 100),
        ("9.50", 100),
        deadline + SECOND_NS,
    );
    publish(
        &node,
        apple,
        ("290.00", 100),
        ("290.00", 100),
        deadline + SECOND_NS,
    );
    within(10, "the US order fills during the CN closure", || {
        order_status(&node, "us-boundary-1").as_deref() == Some("FILLED")
    });
    assert!(
        fills(&store, "cn-boundary-1").is_empty(),
        "the cancelled CN order filled nothing"
    );
    assert_eq!(
        order(
            &store,
            &registry,
            &desk_id,
            "cn-boundary-2",
            "000001.XSHE",
            "BUY",
            100,
            Some("10.00"),
        )
        .expect_err("past 14:57 nothing is admitted")
        .to_string(),
        "The order is not well formed: 000001.XSHE is outside the supported session \
         [09:30,11:30) and [13:00,14:57) Asia/Shanghai."
    );
    registry.stop_all();
}

/// §5.1: one clock step can cross several boundaries — a jump, a suspended
/// daemon — and only one alert is ever armed. Whichever fires, every order past
/// its day lifetime ends, exactly once, and its reservation is released.
#[cfg(test)]
#[test]
fn a_clock_step_over_1457_cancels_on_whichever_boundary_fires() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-jump", CN_0935);
    let desk_id = handle.desk_id().to_owned();

    // 11:00, with only the 11:30 alert armed.
    let eleven = CN_0935 + 5_100 * SECOND_NS;
    node.advance_to(eleven).expect("the clock advances");
    observe_on_node(
        &node,
        snapshot(pingan(), "9.90", "10.00", 1000),
        Ok(()),
        eleven,
    );
    order(
        &store,
        &registry,
        &desk_id,
        "cn-jump-1",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the limit buy rests");

    // One step to 15:30: 11:30 fires, 14:57 was never armed, and the order is
    // still terminated.
    let fired = node
        .advance_to(CN_0935 + 21_300 * SECOND_NS)
        .expect("the clock advances");
    assert!(fired.iter().any(|name| name == ALERT), "{fired:?}");
    within(10, "the stepped-over deadline still ends the day", || {
        order_status(&node, "cn-jump-1").as_deref() == Some("CANCELED")
    });
    assert_eq!(
        crate::node::kinds(&store, &desk_id, "cn-jump-1"),
        [
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ],
        "exactly one cancel"
    );
    assert_eq!(
        reservation(&node, "XSHE", &desk_id),
        ("500000.00 CNY|0.00 CNY|500000.00 CNY".to_owned(), 0),
        "OrderCanceled released the reservation"
    );
    registry.stop_all();
}

/// §5.1, §2.1: a daemon that slept through the close wakes on the next day's
/// first boundary. The prior day's order is terminated, and the instrument is
/// not `Trading` for the new day until a poll re-establishes readiness.
#[cfg(test)]
#[test]
fn an_overnight_step_cancels_the_prior_days_order_and_reopens_unready() {
    let (_dir, store) = crate::store::open_temp();
    let two = CN_0935 + 15_900 * SECOND_NS; // 14:00 Asia/Shanghai
    let (registry, handle, node) = desk(&store, "cn-overnight", two);
    let desk_id = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(pingan().instrument_id);
    observe_on_node(
        &node,
        snapshot(pingan(), "9.90", "10.00", 1000),
        Ok(()),
        two,
    );
    order(
        &store,
        &registry,
        &desk_id,
        "cn-overnight-1",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the limit buy rests");

    // One step to the next day's 09:35: the 14:57 alert fires a day late.
    let next = CN_0935 + DAY_NS;
    let fired = node.advance_to(next).expect("the clock advances");
    assert!(fired.iter().any(|name| name == ALERT), "{fired:?}");
    within(10, "the prior day's order is terminated", || {
        order_status(&node, "cn-overnight-1").as_deref() == Some("CANCELED")
    });
    assert_eq!(
        node.call(move |context| {
            let mut exec = context.cn.borrow_mut();
            let inst = exec.inst(instrument_id);
            (inst.ready_at(next), inst.status)
        })
        .expect("the node answers"),
        (Err(Reason::NoCalendar), Some(MarketStatusAction::Pause)),
        "yesterday's evidence does not open the new day"
    );

    // The new day's first accepted observation is what reopens it.
    observe_on_node(
        &node,
        snapshot(pingan(), "9.90", "10.00", 1000),
        Ok(()),
        next,
    );
    assert_eq!(
        node.call(move |context| context.cn.borrow_mut().inst(instrument_id).status)
            .expect("the node answers"),
        Some(MarketStatusAction::Trading)
    );
    registry.stop_all();
}

/// §5.2: an approval whose readiness lapsed with the Shanghai rollover — before
/// any poll on the new day — leaves the action `PENDING` with no decision
/// recorded at all, because the pre-check reads the same readiness the
/// execution-time checks will.
#[cfg(test)]
#[test]
fn an_approval_after_a_rollover_records_no_decision() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-rollover", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    observe_on_node(
        &node,
        snapshot(pingan(), "9.90", "10.00", 1000),
        Ok(()),
        CN_0935,
    );
    store
        .unit(|tx| {
            tx.execute(
                "UPDATE installation_settings SET paper_order_policy = 'REQUIRE_APPROVAL' \
                 WHERE id = 1",
                [],
            )
        })
        .expect("the policy a `PUT /settings/policies` would have written");
    let (pending, submitted) = trade::submit(
        &store,
        &registry,
        &desk_id,
        r#"{"action_id":"cn-rollover-1","instrument_id":"000001.XSHE",
            "side":"BUY","type":"LIMIT","quantity":"100","price":"10.00"}"#,
        &trade::Source::Session,
    )
    .expect("a gated order is recorded");
    assert_eq!(submitted, trade::Submitted::Pending);

    // The next Shanghai day, inside the session, before any poll.
    node.advance_to(CN_0935 + DAY_NS)
        .expect("the clock advances");
    let blocked = trade::decide(
        &store,
        &registry,
        &desk_id,
        &pending.id,
        crate::policy::Decision::Approve,
    )
    .expect_err("a rolled-over readiness cannot decide the action");
    assert_eq!(blocked.code(), "MARKET_UNAVAILABLE");
    assert_eq!(
        blocked.to_string(),
        "The desk's market plane is unavailable: the desk cannot trade 000001.XSHE \
         right now: NO_CALENDAR."
    );
    assert_eq!(
        trade::history_actions(&store, &desk_id)
            .expect("the actions read")
            .into_iter()
            .find(|row| row.action_id == "cn-rollover-1")
            .expect("the row")
            .approval,
        "PENDING",
        "no approval decision was recorded"
    );
    assert_eq!(
        store
            .call(|conn| conn.query_row(
                "SELECT count(*) FROM operational_events WHERE kind = 'APPROVAL_DECIDED'",
                [],
                |r| r.get::<_, i64>(0)
            ))
            .expect("the events read"),
        0,
        "and no APPROVAL_DECIDED event"
    );
    registry.stop_all();
}

/// §5.1, §2.1: a night, a weekend and an unconfirmed trading day each block CN
/// execution, and a crossing observation accepted just before 11:30 cannot
/// stamp a fill at or after the boundary.
#[cfg(test)]
#[test]
fn nights_weekends_and_unproven_days_admit_nothing() {
    let (_dir, store) = crate::store::open_temp();
    let night = CN_0935 + 15 * 3_600 * SECOND_NS;
    let (registry, handle, node) = desk(&store, "cn-closed", night);
    let desk_id = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(pingan().instrument_id);

    let session = |action: &str| {
        order(
            &store,
            &registry,
            &desk_id,
            action,
            "000001.XSHE",
            "BUY",
            100,
            Some("10.00"),
        )
        .expect_err("out of session")
        .to_string()
    };
    let refusal = "The order is not well formed: 000001.XSHE is outside the supported \
                   session [09:30,11:30) and [13:00,14:57) Asia/Shanghai.";
    assert_eq!(session("cn-closed-night"), refusal);

    // The weekend: 09:35 on the following Saturday.
    let saturday = CN_0935 + 4 * DAY_NS;
    node.advance_to(saturday).expect("the clock advances");
    assert_eq!(session("cn-closed-weekend"), refusal);

    // A day the provider did not confirm — the holiday case — is unavailable,
    // not malformed.
    let monday = CN_0935 + 6 * DAY_NS;
    node.advance_to(monday).expect("the clock advances");
    node.call(move |context| {
        context.cn.borrow_mut().inst(instrument_id).readiness = Err(Reason::NotTradingDay);
    })
    .unwrap();
    let blocked = order(
        &store,
        &registry,
        &desk_id,
        "cn-closed-holiday",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect_err("an unconfirmed trading day admits nothing");
    assert_eq!(blocked.code(), "MARKET_UNAVAILABLE");
    assert_eq!(
        blocked.to_string(),
        "The desk's market plane is unavailable: the desk cannot trade 000001.XSHE \
         right now: NOT_TRADING_DAY."
    );
    registry.stop_all();
}

/// §5.1: at a boundary, timer, submission, data delivery and matching must be
/// ordered so no fill carries an out-of-session instant. A qualifying
/// observation accepted at 11:29:59 fills; the alert then closes the window, and
/// the fill's own stamp is inside the session.
#[cfg(test)]
#[test]
fn no_fill_is_stamped_at_or_after_a_boundary() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-edge", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let close = CN_0935 + 6_900 * SECOND_NS;

    observe_on_node(
        &node,
        snapshot(pingan(), "9.90", "10.00", 1000),
        Ok(()),
        CN_0935 + SECOND_NS,
    );
    order(
        &store,
        &registry,
        &desk_id,
        "cn-edge-1",
        "000001.XSHE",
        "BUY",
        100,
        Some("10.00"),
    )
    .expect("the limit buy rests");

    // 11:29:59 — the last accepted observation of the morning.
    let last = close - SECOND_NS;
    node.advance_to(last).expect("the clock advances");
    observe_on_node(
        &node,
        snapshot(pingan(), "9.90", "10.00", 1001),
        Ok(()),
        last,
    );
    within(10, "the pre-boundary observation fills", || {
        order_status(&node, "cn-edge-1").as_deref() == Some("FILLED")
    });
    let stamps: Vec<i64> = store
        .call(|conn| {
            conn.prepare("SELECT occurred_at_ns FROM order_events WHERE kind = 'OrderFilled'")?
                .query_map([], |r| r.get(0))?
                .collect()
        })
        .expect("the fill stamps");
    assert!(
        stamps.iter().all(|ns| (*ns as u64) < close),
        "every fill is stamped inside the session: {stamps:?}"
    );

    // The boundary itself adds nothing.
    let fired = node.advance_to(close).expect("the clock advances");
    assert!(fired.iter().any(|name| name == ALERT), "{fired:?}");
    assert_eq!(fill_count(&store), 1);
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// The surface (feature SPEC `a-share-engine` §1.3, §2.1, §2.3, §4 —
// "Surface and seed"): the CN-only eligibility fields, the per-desk execution
// object, and AE-7's calendar gate under the Yahoo feed.
// ---------------------------------------------------------------------------

/// A desk on the production calendar rule (AE-7): CN execution is unavailable
/// until a confirmed same-day trading calendar opens it.
#[cfg(test)]
fn uncalendared_desk(
    store: &Store,
    name: &'static str,
    start_ns: u64,
) -> (Registry, crate::node::ClockHandle, Arc<Node>) {
    let (registry, handle) =
        crate::node::controlled_registry_uncalendared(store, None, name, start_ns);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    (registry, handle, node)
}

/// What a first successful poll cycle installs: the confirmed trading day, and
/// the session gate that follows from it. The status and the book go down the
/// same data channel, so a book that has landed proves the status ahead of it
/// was processed.
#[cfg(test)]
fn confirm_calendar(node: &Node, entry: &'static Entry, price: &str, at_ns: u64) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    node.call(move |context| {
        let mut exec = context.cn.borrow_mut();
        exec.inst(instrument_id).readiness = Ok(());
        gate(&mut exec, entry, at_ns);
    })
    .expect("the node answers");
    publish(node, entry, (price, 300), (price, 300), at_ns + 1);
}

/// AE-7: Yahoo CN is explicitly simplified, not ungated. A desk with no HiThink
/// provider has no confirmed trading day to read, so CN execution stays
/// `NO_CALENDAR` — while awareness keeps working — and the confirmed calendar a
/// poll cycle installs is what opens it.
#[cfg(test)]
#[test]
fn yahoo_cn_needs_the_confirmed_calendar_too() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = uncalendared_desk(&store, "cn-no-calendar", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let instrument_id = InstrumentId::from(pingan().instrument_id);
    publish(&node, pingan(), ("9.90", 300), ("9.90", 300), CN_0935);

    // Awareness is untouched: the book is there to read.
    assert!(
        node.call(move |context| context.cache.borrow().quote(&instrument_id).is_some())
            .unwrap(),
        "the quote still reaches the desk"
    );
    assert_eq!(
        order(
            &store,
            &registry,
            &desk_id,
            "cn-nocal-1",
            "000001.XSHE",
            "BUY",
            100,
            None,
        )
        .expect_err("no calendar, no execution")
        .to_string(),
        "The desk's market plane is unavailable: the desk cannot trade 000001.XSHE \
         right now: NO_CALENDAR."
    );

    // What `cn_cycle`'s Yahoo branch assigns from `Hithink::trading_day` once
    // the provider has confirmed today. The session is open, so the same order
    // is now admitted.
    confirm_calendar(&node, pingan(), "9.90", CN_0935 + SECOND_NS);
    let placed = order(
        &store,
        &registry,
        &desk_id,
        "cn-nocal-2",
        "000001.XSHE",
        "BUY",
        100,
        None,
    )
    .expect("the confirmed trading day admits it");
    assert_eq!(placed["status"], "FILLED", "{placed}");
    registry.stop_all();
}

/// §1.3: a **current** CN position carries the three share-eligibility
/// projections; US omits them, and no historical record gains them.
#[cfg(test)]
#[test]
fn current_cn_positions_carry_the_eligibility_projections() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk(&store, "cn-projection", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let apple = crate::catalog::find("AAPL.XNAS").expect("the US catalog entry");

    publish(&node, pingan(), ("10.00", 300), ("10.00", 300), CN_0935);
    publish(&node, apple, ("200.00", 10), ("200.00", 10), CN_0935);
    for (action, instrument, quantity) in [
        ("cn-proj-buy", "000001.XSHE", 300u32),
        ("us-proj-buy", "AAPL.XNAS", 10),
    ] {
        let filled = order(
            &store, &registry, &desk_id, action, instrument, "BUY", quantity, None,
        )
        .expect("the market buy fills");
        assert_eq!(filled["status"], "FILLED", "{filled}");
    }

    let positions = trade::open_positions(&node).expect("the positions read");
    let of = |instrument: &str| {
        positions
            .iter()
            .find(|p| p["instrument_id"] == instrument)
            .unwrap_or_else(|| panic!("a position in {instrument}"))
            .clone()
    };
    let cn = of("000001.XSHE");
    assert_eq!(cn["sellable_quantity"], "0", "{cn}");
    assert_eq!(cn["locked_quantity"], "300", "{cn}");
    assert_eq!(cn["reserved_quantity"], "0", "{cn}");
    let us = of("AAPL.XNAS");
    for field in ["sellable_quantity", "locked_quantity", "reserved_quantity"] {
        assert!(us.get(field).is_none(), "US carries no {field}: {us}");
    }

    // A resting SELL reserves; the projection moves with it, and the history
    // does not gain a field.
    let rested = order(
        &store,
        &registry,
        &desk_id,
        "cn-proj-sell",
        "000001.XSHE",
        "SELL",
        100,
        Some("11.00"),
    );
    assert!(rested.is_err(), "today's buy is locked: {rested:?}");
    for fill in trade::history_fills(&store, &desk_id).expect("the fills read") {
        for field in ["sellable_quantity", "locked_quantity", "reserved_quantity"] {
            assert!(
                fill.get(field).is_none(),
                "a fill carries no {field}: {fill}"
            );
        }
    }
    registry.stop_all();
}

/// §2.1, §2.3: a CN quote carries this desk's own execution view — availability
/// and reason, independent of health and phase — and US carries none.
#[cfg(test)]
#[test]
fn cn_quotes_carry_this_desks_execution_view() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = uncalendared_desk(&store, "cn-execution", CN_0935);
    let _ = handle;
    node.call(|context| context.cn.borrow_mut().feed = AShareFeed::Hithink)
        .expect("the node answers");
    let market = crate::feed::MarketState::new();
    let received_at_ns = CN_0935 as i64;
    for (entry, price) in [
        (pingan(), "9.90"),
        (
            crate::catalog::find("AAPL.XNAS").expect("the US catalog entry"),
            "200.00",
        ),
    ] {
        market.accept(
            entry,
            &crate::feed::ChartQuote {
                price: price.parse().expect("decimal text"),
                currency: entry.currency.to_owned(),
                source_time_ns: received_at_ns,
            },
            received_at_ns,
        );
    }
    let read = |node: &Node| {
        let mut quotes = market.read_all(received_at_ns + 2_000_000_000);
        attach(node, &mut quotes);
        quotes
    };
    let view = |quotes: &[crate::feed::Observation], id: &str| {
        quotes
            .iter()
            .find(|o| o.instrument_id == id)
            .expect("a catalog entry")
            .execution
            .clone()
    };

    // Before any accepted observation the instrument is unready: `UNAVAILABLE`
    // with the reason, whatever the phase and whatever `health` says.
    let quotes = read(&node);
    let cn = view(&quotes, "000001.XSHE").expect("a CN entry carries one");
    assert_eq!(cn.availability, UNAVAILABLE);
    assert_eq!(cn.reason, Some(Reason::NoCalendar));
    assert_eq!(cn.fill_policy, HITHINK_SAMPLED);
    assert_eq!(cn.source_delay, "UNKNOWN");
    assert!(cn.band_date_inferred);
    assert_eq!(
        cn.receipt_age_ms,
        Some(2_000),
        "the age counts from receipt, never from a source timestamp"
    );
    assert!(
        view(&quotes, "AAPL.XNAS").is_none(),
        "US carries no execution object"
    );

    // One accepted observation inside the session opens it.
    observe_on_node(
        &node,
        snapshot(pingan(), "9.90", "10.00", 1_000),
        Ok(()),
        CN_0935,
    );
    let open = view(&read(&node), "000001.XSHE").expect("a CN entry carries one");
    assert_eq!(open.availability, OPEN);
    assert_eq!(open.reason, None);
    assert_eq!(
        open.band_date.as_deref(),
        Some(shanghai_date(CN_0935).as_str())
    );

    // The midday break pauses it without ending the day.
    let lunch = CN_0935 + 7_500 * SECOND_NS;
    assert!(in_lunch(lunch), "11:40 Asia/Shanghai");
    observe_on_node(
        &node,
        snapshot(pingan(), "9.90", "10.00", 1_000),
        Ok(()),
        lunch,
    );
    assert_eq!(
        view(&read(&node), "000001.XSHE")
            .expect("a CN entry carries one")
            .availability,
        PAUSED
    );

    // A desk whose node is gone answers `NODE_NOT_STARTED` rather than guessing.
    registry.stop_all();
    let gone = view(&read(&node), "000001.XSHE").expect("a CN entry carries one");
    assert_eq!(gone.availability, UNAVAILABLE);
    assert_eq!(gone.reason, Some(Reason::NodeNotStarted));
}
