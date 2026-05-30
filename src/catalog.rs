//! Static catalog of the served datasets and the analytical views over them.
//!
//! This module carries the *names*, *layout*, *human descriptions*, and the
//! security **allowlist** that gates `run_query`. The authoritative column
//! list is introspected from the live serving database at runtime
//! (`describe_schema`), so it can never drift from the data — this file owns
//! only what the engine can't infer: semantics, grain, and what's allowed.

/// How a dataset is laid out under the data root.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Daily date-partitioned time series: `<name>/date=*/*.parquet`.
    TimeSeries,
    /// Latest-per-ticker snapshot: `<name>/latest.parquet`.
    Snapshot,
}

/// A base table loaded from Parquet.
pub struct Dataset {
    pub name: &'static str,
    pub kind: Kind,
    pub doc: &'static str,
}

/// A view computed in the serving database over the base tables.
pub struct View {
    pub name: &'static str,
    /// Base tables this view needs; the view is created only if all are loaded.
    pub requires: &'static [&'static str],
    pub doc: &'static str,
}

/// The 12 base tables. Keyed by `ticker` (+ `date` for time series).
pub const DATASETS: &[Dataset] = &[
    Dataset {
        name: "prices",
        kind: Kind::TimeSeries,
        doc: "Daily OHLCV (Yahoo-adjusted). Grain: ticker+date. Primary price source.",
    },
    Dataset {
        name: "eod_summary",
        kind: Kind::TimeSeries,
        doc: "Official IDX end-of-day summary incl. foreign_buy/foreign_sell and traded value. Grain: ticker+date.",
    },
    Dataset {
        name: "indicators",
        kind: Kind::TimeSeries,
        doc: "Daily technical indicators (RSI, MACD, SMA/EMA, Bollinger, ATR, VWAP). Grain: ticker+date; NULL during warmup.",
    },
    Dataset {
        name: "fundamentals",
        kind: Kind::TimeSeries,
        doc: "Daily fundamentals snapshot: market_cap, enterprise_value, shares_outstanding, free_float_pct, latest dividend. Grain: ticker+date.",
    },
    Dataset {
        name: "broker_activity",
        kind: Kind::TimeSeries,
        doc: "Per-broker buy/sell flow (bandarmology). Grain: ticker+date+broker_code+side (B|S). value & volume_lot are positive magnitudes; net = buy - sell.",
    },
    Dataset {
        name: "broker_distribution",
        kind: Kind::TimeSeries,
        doc: "Broker-to-broker distribution graph (who traded against whom), as delivered by the ETL — typically exploded edges (ticker, date, side, source/counterparty broker, value, volume). Call describe_schema for the live columns.",
    },
    Dataset {
        name: "broker_rankings",
        kind: Kind::TimeSeries,
        doc: "Market-wide broker league table by traded value/volume/frequency. Grain: broker_code+date. NO ticker column.",
    },
    Dataset {
        name: "announcements",
        kind: Kind::TimeSeries,
        doc: "IDX corporate announcements and news. Grain: one row per announcement. source = announcement|news; titles are Indonesian.",
    },
    Dataset {
        name: "companies",
        kind: Kind::Snapshot,
        doc: "Company profile: name, sector, sub_sector, exchange, listing board, IPO date, background. Latest per ticker.",
    },
    Dataset {
        name: "summary",
        kind: Kind::Snapshot,
        doc: "Yahoo fundamentals snapshot. Latest per ticker. WARNING: PE/price_to_book and other valuation ratios are unreliable for IDX names.",
    },
    Dataset {
        name: "analyst",
        kind: Kind::Snapshot,
        doc: "Analyst recommendation counts and price/EPS/revenue targets. Latest per ticker.",
    },
    Dataset {
        name: "ownership",
        kind: Kind::Snapshot,
        doc: "KSEI depository holders with >=1% of shares, local/foreign split. Latest per ticker.",
    },
];

/// Analytical views built over the base tables (composable building blocks).
pub const VIEWS: &[View] = &[
    View {
        name: "latest",
        requires: &[
            "companies",
            "prices",
            "indicators",
            "fundamentals",
            "summary",
        ],
        doc: "One row per ticker: latest close/volume, fundamentals, yf ratios, and key indicators joined. Built for cross-sectional screening.",
    },
    View {
        name: "returns",
        requires: &["prices"],
        doc: "One row per ticker: trailing % returns (ret_1w/1m/3m/6m/ytd/1y/3y) and annualized cagr_3y from close prices.",
    },
    View {
        name: "broker_net",
        requires: &["broker_activity"],
        doc: "One row per ticker+date+broker_code: buy/sell/net value and volume. Base for accumulation, flip, and market-maker analysis.",
    },
];

/// Numeric fields `screen_stocks` may filter/sort on — must exist on `latest`.
pub const SCREEN_FIELDS_NUM: &[&str] = &[
    "market_cap",
    "enterprise_value",
    "shares_outstanding",
    "free_float_pct",
    "trailing_pe",
    "forward_pe",
    "price_to_book",
    "dividend_yield",
    "beta",
    "return_on_equity",
    "profit_margins",
    "close",
    "volume",
    "rsi_14",
    "sma_50",
    "sma_200",
];

/// Text fields `screen_stocks` may filter on (equality / IN only).
pub const SCREEN_FIELDS_TEXT: &[&str] = &["sector"];

/// Table functions permitted inside `run_query`. File/network readers
/// (`read_parquet`, `read_csv`, `glob`, …) are deliberately excluded — they
/// are also blocked at the engine, this is the early, clear-error layer.
const SAFE_TABLE_FUNCTIONS: &[&str] = &[
    "generate_series",
    "range",
    "unnest",
    "values",
    "repeat",
    "json_each",
];

/// True if `name` is a base table or view a query may reference.
#[must_use]
pub fn is_allowed_table(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    DATASETS.iter().any(|d| d.name == n) || VIEWS.iter().any(|v| v.name == n)
}

/// True if `name` is a table function permitted inside `run_query`.
#[must_use]
pub fn is_safe_table_function(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SAFE_TABLE_FUNCTIONS.contains(&n.as_str())
}

/// Human description for a table or view, if known.
#[must_use]
pub fn doc_for(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    DATASETS
        .iter()
        .find(|d| d.name == n)
        .map(|d| d.doc)
        .or_else(|| VIEWS.iter().find(|v| v.name == n).map(|v| v.doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_unique() {
        let mut seen = HashSet::new();
        for name in DATASETS
            .iter()
            .map(|d| d.name)
            .chain(VIEWS.iter().map(|v| v.name))
        {
            assert!(seen.insert(name), "duplicate catalog name: {name}");
        }
    }

    #[test]
    fn allowlist_covers_all_datasets_and_views() {
        for d in DATASETS {
            assert!(is_allowed_table(d.name), "{} not allowed", d.name);
            assert!(is_allowed_table(&d.name.to_uppercase()), "case-insensitive");
        }
        for v in VIEWS {
            assert!(is_allowed_table(v.name), "{} not allowed", v.name);
        }
    }

    #[test]
    fn view_requirements_reference_real_datasets() {
        for v in VIEWS {
            for req in v.requires {
                assert!(
                    DATASETS.iter().any(|d| &d.name == req),
                    "view {} requires unknown dataset {req}",
                    v.name
                );
            }
        }
    }

    #[test]
    fn unknown_and_function_tables_are_rejected() {
        assert!(!is_allowed_table("read_parquet"));
        assert!(!is_allowed_table("duckdb_settings"));
        assert!(!is_allowed_table("pg_tables"));
        assert!(!is_safe_table_function("read_parquet"));
        assert!(!is_safe_table_function("read_csv_auto"));
        assert!(is_safe_table_function("generate_series"));
    }
}
