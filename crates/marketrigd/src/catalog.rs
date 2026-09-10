//! The compiled-in instrument catalog: MarketRig's whole tradable universe.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §3, per R1-2.

use rust_decimal::{Decimal, RoundingStrategy};
use serde::Serialize;

/// A market key (§3): an entry's calendar key ([`crate::feed`]) and fee key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Market {
    Us,
    Hk,
    Cn,
}

impl Market {
    /// The one currency this market's instruments trade against (§4.1).
    pub fn currency(self) -> &'static str {
        match self {
            Market::Us => "USD",
            Market::Hk => "HKD",
            Market::Cn => "CNY",
        }
    }
}

/// The supported A-share boards (`a-share-engine` SPEC §2.2, §3.2, per AE-3,
/// AE-5): the band percentage and the order caps follow from the board alone.
/// STAR and Beijing listings are not admitted by this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Board {
    Main,
    ChiNext,
}

/// Which cap applies to a quantity (§3.2). The two order types MarketRig
/// admits, in MarketRig's own vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    Limit,
    Market,
}

impl Board {
    /// The daily price band, whole percent (§2.2).
    pub fn band_percent(self) -> u32 {
        match self {
            Board::Main => 10,
            Board::ChiNext => 20,
        }
    }

    /// The per-order share cap (§3.2).
    pub fn limit_cap(self, kind: OrderKind) -> u64 {
        match (self, kind) {
            (Board::Main, _) => 1_000_000,
            (Board::ChiNext, OrderKind::Limit) => 300_000,
            (Board::ChiNext, OrderKind::Market) => 150_000,
        }
    }
}

/// The inclusive daily band around a provider reference (§2.2), exact decimal
/// arithmetic throughout: round half up to the tick, then guarantee at least one
/// tick of room on each side, and never a nonpositive lower bound.
pub fn band(prev_close: Decimal, tick: Decimal, board: Board) -> (Decimal, Decimal) {
    let percent = Decimal::from(board.band_percent()) / Decimal::from(100);
    let to_tick = |value: Decimal| {
        (value / tick).round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero) * tick
    };
    let mut up = to_tick(prev_close * (Decimal::ONE + percent));
    let mut down = to_tick(prev_close * (Decimal::ONE - percent));
    if up - prev_close < tick {
        up = prev_close + tick;
    }
    if prev_close - down < tick {
        down = prev_close - tick;
    }
    (up, down.max(tick))
}

/// One catalog entry (§3). `price_increment` is decimal text and stays text: it
/// feeds instrument construction and precision, never a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The NautilusTrader identifier, `SYMBOL.VENUE`.
    pub instrument_id: &'static str,
    pub yahoo_symbol: &'static str,
    /// The HiThink `thscode`, present exactly on `CN` entries (feature SPEC
    /// `hithink-a-share` §2.1): `XSHG → .SH`, `XSHE → .SZ`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hithink_symbol: Option<&'static str>,
    /// The A-share board, present exactly on `CN` entries (`a-share-engine`
    /// SPEC §2.2): it is what derives the band and the order caps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<Board>,
    pub market: Market,
    pub currency: &'static str,
    /// The fixed tick, decimal text.
    pub price_increment: &'static str,
    /// The order-quantity multiple.
    pub lot_size: u32,
}

/// Column order of the [`ENTRIES`] table below — the arguments *are* the
/// table's columns, which is why there are as many as [`Entry`] has fields.
#[allow(clippy::too_many_arguments)]
const fn entry(
    instrument_id: &'static str,
    yahoo_symbol: &'static str,
    hithink_symbol: Option<&'static str>,
    board: Option<Board>,
    market: Market,
    currency: &'static str,
    price_increment: &'static str,
    lot_size: u32,
) -> Entry {
    Entry {
        instrument_id,
        yahoo_symbol,
        hithink_symbol,
        board,
        market,
        currency,
        price_increment,
        lot_size,
    }
}

/// The starter set (§3). Every tick and lot was verified against its venue on
/// 2026-09-01, per R1-2:
///
/// - **US (XNAS)** — Reg NMS Rule 612 keeps the minimum increment at $0.01 for NMS
///   stocks at or above $1.00, and lots are single shares. The $0.005 increment
///   effective 2025-11-03 applies only to *quoting* tick-constrained stocks and
///   only loosens the ladder, so an authored $0.01 tick stays valid everywhere.
/// - **HK (XHKG)** — the tick is the HKEX spread-table entry for the stock's
///   prevailing price band, so each row records its band evidence. Phase 1 of the
///   spread reduction (effective 2025-08-04) narrowed only the $10–$20 and $20–$50
///   bands, Phase 2 only $0.25–$10; the $50–$100 (0.05), $100–$200 (0.10) and
///   $200–$500 (0.20) bands this table uses are unchanged.
/// - **CN (XSHG/XSHE)** — both exchanges quote A shares in RMB 0.01 and take
///   auction orders in multiples of 100 shares.
///
/// ponytail: fixed ticks against a price-banded ladder — a Hong Kong stock that
/// crosses a band boundary, or any board-lot change (HKEX has a trading-unit
/// reform in consultation), needs this table revisited. The upgrade path R1-2
/// records is per-band tick logic behind [`find`].
pub static ENTRIES: &[Entry] = &[
    entry(
        "AAPL.XNAS",
        "AAPL",
        None,
        None,
        Market::Us,
        "USD",
        "0.01",
        1,
    ),
    entry(
        "MSFT.XNAS",
        "MSFT",
        None,
        None,
        Market::Us,
        "USD",
        "0.01",
        1,
    ),
    entry(
        "NVDA.XNAS",
        "NVDA",
        None,
        None,
        Market::Us,
        "USD",
        "0.01",
        1,
    ),
    entry(
        "AMZN.XNAS",
        "AMZN",
        None,
        None,
        Market::Us,
        "USD",
        "0.01",
        1,
    ),
    entry(
        "TSLA.XNAS",
        "TSLA",
        None,
        None,
        Market::Us,
        "USD",
        "0.01",
        1,
    ),
    // Tencent: HK$444.40 on 2026-09-01 → band $200–$500 → tick 0.20; board lot 100.
    entry(
        "0700.XHKG",
        "0700.HK",
        None,
        None,
        Market::Hk,
        "HKD",
        "0.20",
        100,
    ),
    // Alibaba: HK$110.40 on 2026-09-01 → band $100–$200 → tick 0.10; board lot 100.
    entry(
        "9988.XHKG",
        "9988.HK",
        None,
        None,
        Market::Hk,
        "HKD",
        "0.10",
        100,
    ),
    // HSBC: HK$160.60 on 2026-09-01 → band $100–$200 → tick 0.10; board lot 400.
    // Feature SPEC §3 authored 0.05 (the $50–$100 band); the venue wins, per R1-2.
    entry(
        "0005.XHKG",
        "0005.HK",
        None,
        None,
        Market::Hk,
        "HKD",
        "0.10",
        400,
    ),
    // AIA: HK$75.95 on 2026-09-01 → band $50–$100 → tick 0.05; board lot 200.
    entry(
        "1299.XHKG",
        "1299.HK",
        None,
        None,
        Market::Hk,
        "HKD",
        "0.05",
        200,
    ),
    // Meituan: HK$76.65 on 2026-09-01 → band $50–$100 → tick 0.05; board lot 100.
    // Feature SPEC §3 authored 0.10 (the $100–$200 band); the venue wins, per R1-2.
    entry(
        "3690.XHKG",
        "3690.HK",
        None,
        None,
        Market::Hk,
        "HKD",
        "0.05",
        100,
    ),
    // The CN entries carry HiThink's own thscode beside the Yahoo symbol
    // (feature SPEC `hithink-a-share` §2.1, per HT-2).
    cn("600519.XSHG", "600519.SS", "600519.SH", Board::Main),
    cn("601318.XSHG", "601318.SS", "601318.SH", Board::Main),
    cn("000001.XSHE", "000001.SZ", "000001.SZ", Board::Main),
    cn("000858.XSHE", "000858.SZ", "000858.SZ", Board::Main),
    // 300xxx is ChiNext: a 20% band and the lower caps (§2.2, §3.2).
    cn("300750.XSHE", "300750.SZ", "300750.SZ", Board::ChiNext),
];

/// A `CN` row: RMB 0.01 ticks and 100-share lots on both exchanges.
const fn cn(
    instrument_id: &'static str,
    yahoo_symbol: &'static str,
    hithink_symbol: &'static str,
    board: Board,
) -> Entry {
    entry(
        instrument_id,
        yahoo_symbol,
        Some(hithink_symbol),
        Some(board),
        Market::Cn,
        "CNY",
        "0.01",
        100,
    )
}

/// Looks an instrument up by its `SYMBOL.VENUE` identifier. `None` is the caller's
/// `INSTRUMENT_UNKNOWN` (§3); the routes that own the code do the mapping.
pub fn find(instrument_id: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|e| e.instrument_id == instrument_id)
}

#[cfg(test)]
#[test]
fn entries_valid() {
    use std::collections::HashSet;

    use rust_decimal::Decimal;

    assert_eq!(ENTRIES.len(), 15, "the §3 starter set is fifteen entries");

    let mut ids = HashSet::new();
    for e in ENTRIES {
        let id = e.instrument_id;
        assert!(ids.insert(id), "{id} appears twice");
        assert!(!e.yahoo_symbol.is_empty(), "{id} has no Yahoo symbol");

        let tick: Decimal = e
            .price_increment
            .parse()
            .unwrap_or_else(|_| panic!("{id} tick {:?} is not decimal text", e.price_increment));
        assert!(tick > Decimal::ZERO, "{id} tick must be positive");
        assert!(e.lot_size > 0, "{id} lot must be positive");

        assert_eq!(e.currency, e.market.currency(), "{id} currency vs market");
        assert!(
            !crate::feed::calendar(e.market).1.is_empty(),
            "{id} has no calendar sessions"
        );
        assert_eq!(find(id), Some(e), "{id} is not findable by its own id");

        // The HiThink thscode is present exactly on CN entries, and it is the
        // Nautilus symbol under the venue's own suffix (feature SPEC
        // `hithink-a-share` §2.1, per HT-2).
        assert_eq!(
            e.market == Market::Cn,
            e.hithink_symbol.is_some(),
            "{id}: a thscode belongs to the CN entries and to no other"
        );
        // So does the board, and 300xxx is the ChiNext one (§2.2).
        assert_eq!(
            e.market == Market::Cn,
            e.board.is_some(),
            "{id}: a board belongs to the CN entries and to no other"
        );
        assert_eq!(
            e.board,
            match e.market {
                Market::Cn if id.starts_with("300") => Some(Board::ChiNext),
                Market::Cn => Some(Board::Main),
                _ => None,
            },
            "{id} board"
        );
        if let Some(thscode) = e.hithink_symbol {
            let (symbol, venue) = id.split_once('.').expect("SYMBOL.VENUE");
            let suffix = match venue {
                "XSHG" => ".SH",
                "XSHE" => ".SZ",
                other => panic!("{id}: {other} is not a CN venue"),
            };
            assert_eq!(thscode, format!("{symbol}{suffix}"), "{id} thscode");
        }
    }

    assert_eq!(find("NOPE.XNAS"), None);
    assert_eq!(
        crate::feed::cn_entries().count(),
        5,
        "the CN leg HiThink serves is five entries"
    );
}

/// The §2.2 band, boards and caps: the ratios, the rounding corners, and the
/// two floors that keep at least one tick of room on each side.
#[cfg(test)]
#[test]
fn band_and_caps() {
    let d = |text: &str| text.parse::<Decimal>().unwrap();
    let tick = d("0.01");
    let cn = |id: &str| find(id).unwrap();

    // Main board 10%, ChiNext 20%, on the round reference.
    assert_eq!(band(d("10.00"), tick, Board::Main), (d("11.00"), d("9.00")));
    assert_eq!(
        band(d("10.00"), tick, Board::ChiNext),
        (d("12.00"), d("8.00"))
    );
    assert_eq!(cn("600519.XSHG").board.unwrap().band_percent(), 10);
    assert_eq!(cn("300750.XSHE").board.unwrap().band_percent(), 20);

    // Round half up to the tick, never half-even and never truncation:
    // 11.785 * 1.1 = 12.9635 → 12.96; * 0.9 = 10.6065 → 10.61.
    assert_eq!(
        band(d("11.785"), tick, Board::Main),
        (d("12.96"), d("10.61"))
    );
    // 1309.30 → 1440.23 / 1178.37, the F7 reference (unrounded 1440.230/1178.370).
    assert_eq!(
        band(d("1309.30"), tick, Board::Main),
        (d("1440.23"), d("1178.37"))
    );

    // A reference so small that the percentage is under one tick: each side is
    // pushed a whole tick away, and the lower bound never reaches zero.
    assert_eq!(band(d("0.03"), tick, Board::Main), (d("0.04"), d("0.02")));
    assert_eq!(band(d("0.01"), tick, Board::Main), (d("0.02"), d("0.01")));
    // 0.05 * 1.1 = 0.055 → 0.06 is already a tick away; the floor does not fire.
    assert_eq!(band(d("0.05"), tick, Board::Main), (d("0.06"), d("0.04")));
    // A coarse tick makes the same thing happen far from zero: 100.00 ± 10%
    // rounds to 110 / 90 on a 0.20 tick, but 1.00 ± 10% is 1.00 either way.
    assert_eq!(
        band(d("1.00"), d("0.20"), Board::Main),
        (d("1.20"), d("0.80"))
    );

    // The §3.2 caps, by board and order kind.
    for (board, limit, market) in [
        (Board::Main, 1_000_000, 1_000_000),
        (Board::ChiNext, 300_000, 150_000),
    ] {
        assert_eq!(board.limit_cap(OrderKind::Limit), limit);
        assert_eq!(board.limit_cap(OrderKind::Market), market);
    }
}
