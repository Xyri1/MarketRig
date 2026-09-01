//! The compiled-in instrument catalog: MarketRig's whole tradable universe.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §3, per R1-2.

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

/// One catalog entry (§3). `price_increment` is decimal text and stays text: it
/// feeds instrument construction and precision, never a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The NautilusTrader identifier, `SYMBOL.VENUE`.
    pub instrument_id: &'static str,
    pub yahoo_symbol: &'static str,
    pub market: Market,
    pub currency: &'static str,
    /// The fixed tick, decimal text.
    pub price_increment: &'static str,
    /// The order-quantity multiple.
    pub lot_size: u32,
}

/// Column order of the [`ENTRIES`] table below.
const fn entry(
    instrument_id: &'static str,
    yahoo_symbol: &'static str,
    market: Market,
    currency: &'static str,
    price_increment: &'static str,
    lot_size: u32,
) -> Entry {
    Entry {
        instrument_id,
        yahoo_symbol,
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
    entry("AAPL.XNAS", "AAPL", Market::Us, "USD", "0.01", 1),
    entry("MSFT.XNAS", "MSFT", Market::Us, "USD", "0.01", 1),
    entry("NVDA.XNAS", "NVDA", Market::Us, "USD", "0.01", 1),
    entry("AMZN.XNAS", "AMZN", Market::Us, "USD", "0.01", 1),
    entry("TSLA.XNAS", "TSLA", Market::Us, "USD", "0.01", 1),
    // Tencent: HK$444.40 on 2026-09-01 → band $200–$500 → tick 0.20; board lot 100.
    entry("0700.XHKG", "0700.HK", Market::Hk, "HKD", "0.20", 100),
    // Alibaba: HK$110.40 on 2026-09-01 → band $100–$200 → tick 0.10; board lot 100.
    entry("9988.XHKG", "9988.HK", Market::Hk, "HKD", "0.10", 100),
    // HSBC: HK$160.60 on 2026-09-01 → band $100–$200 → tick 0.10; board lot 400.
    // Feature SPEC §3 authored 0.05 (the $50–$100 band); the venue wins, per R1-2.
    entry("0005.XHKG", "0005.HK", Market::Hk, "HKD", "0.10", 400),
    // AIA: HK$75.95 on 2026-09-01 → band $50–$100 → tick 0.05; board lot 200.
    entry("1299.XHKG", "1299.HK", Market::Hk, "HKD", "0.05", 200),
    // Meituan: HK$76.65 on 2026-09-01 → band $50–$100 → tick 0.05; board lot 100.
    // Feature SPEC §3 authored 0.10 (the $100–$200 band); the venue wins, per R1-2.
    entry("3690.XHKG", "3690.HK", Market::Hk, "HKD", "0.05", 100),
    entry("600519.XSHG", "600519.SS", Market::Cn, "CNY", "0.01", 100),
    entry("601318.XSHG", "601318.SS", Market::Cn, "CNY", "0.01", 100),
    entry("000001.XSHE", "000001.SZ", Market::Cn, "CNY", "0.01", 100),
    entry("000858.XSHE", "000858.SZ", Market::Cn, "CNY", "0.01", 100),
    entry("300750.XSHE", "300750.SZ", Market::Cn, "CNY", "0.01", 100),
];

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
    }

    assert_eq!(find("NOPE.XNAS"), None);
}
