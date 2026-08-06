//! Indicator expression grammar → joint analytics SQL.
//!
//! Parses the MCDX indicator grammar and compiles it to the shared CTE pipeline
//! that analytics runs against `data`.
//!
//! Series literals carry the bar bucket and an optional emit domain:
//!
//! - `[close.1d; $from:$to]` — daily closes over an absolute ms range
//! - `[close.1h]` — full possible hourly series from available data
//! - `[binance:close.1d]` — unaggregated source-qualified closes
//! - `AVG([close.1d], 14)[-1]` — last SMA value (postfix result slice)
//! - `{ close: [close.1h], ema: EMA([close.1h], 14) }[$from:$to]` — batch with shared emit range
//! - `[close.1d@$benchmark]` — qualified asset
//!
//! Java consumers: enable `--features jni`, build the JAR under `java/` (see README).

mod ast;
mod compile;
mod error;
mod interval;
mod json_api;
mod lex;
mod parse;
mod result_map;
mod sem;

#[cfg(feature = "jni")]
mod jni_bridge;

use sha2::{Digest, Sha256};

pub use ast::{
    AssetRef, BatchExpr, BinOp, CallOp, DomainBound, EmitCount, EmitEnd, Expr, IndexSelector,
    LookbackBound, Series, SeriesDomain,
};
pub use compile::{
    compile, compile_batch, compile_expr, BindValue, CompiledQuery, CompileRequest, Scaffolds,
};
pub use error::{Error, ErrorCode};
pub use interval::{interval_ms, IntervalError};
pub use json_api::{
    compile_json, BindValueJson, CompileRequestJson, CompileResponseJson, CompiledQueryJson,
    DomainJson, ErrorJson, ParamValueJson, ScaffoldsJson,
};
pub use lex::tokenize;
pub use parse::{parse_batch, parse_expr};
pub use result_map::{
    map_sql_row, sql_columns, IndicatorComputeRow, MapRowError, SqlValue,
};
pub use sem::{analyze, Analysis, Domain, ParamValue};

/// Fingerprint used for bare / aggregated bars (`params_hash` of empty params).
/// SHA-256 of the JSON object `{}` — matches analytics empty-params convention.
pub const EMPTY_PARAMS_HASH: &str =
    "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";

/// `params_hash` for a series scan.
///
/// - Aggregated / canonical (`[close.1d]`): SHA-256 of `{}` ([`EMPTY_PARAMS_HASH`]).
/// - Unaggregated (`[binance:close.1d]`): SHA-256 of compact JSON `{"source":"<source>"}`.
pub fn params_hash_for_source(source: Option<&str>) -> String {
    match source {
        None => EMPTY_PARAMS_HASH.to_string(),
        Some(source) => {
            let json = format!(r#"{{"source":"{}"}}"#, escape_json_string(source));
            hex_sha256(json.as_bytes())
        }
    }
}

fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\u{20}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[cfg(test)]
mod params_hash_tests {
    use super::*;

    #[test]
    fn empty_params_matches_constant() {
        assert_eq!(params_hash_for_source(None), EMPTY_PARAMS_HASH);
        assert_eq!(hex_sha256(b"{}"), EMPTY_PARAMS_HASH);
    }

    #[test]
    fn source_params_hash_binance() {
        assert_eq!(
            params_hash_for_source(Some("binance")),
            "691508c082c9c6b7be0aaed0f8a914bca6e8b2333ffadd9b297f367d4e83aa87"
        );
    }
}