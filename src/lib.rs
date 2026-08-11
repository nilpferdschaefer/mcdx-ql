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
    LookbackBound, ParamLit, Series, SeriesDomain,
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

/// SHA-256 hex of the canonical compact JSON of `params`.
///
/// Mirrors the datastore / analytics contract (`mcdx-datastore` `stream_params.rs`
/// `canonical_params_value` + `params_hash`) **byte-for-byte** so a QL scan filter
/// matches the rows the loader wrote: object keys sorted lexicographically
/// (recursively), arrays keep order, integers stay integers (`96`, not `96.0`),
/// compact JSON with no spaces. An empty object hashes to [`EMPTY_PARAMS_HASH`].
pub fn params_hash(params: &serde_json::Value) -> String {
    let canonical = canonical_params_value(params);
    let bytes = serde_json::to_vec(&canonical).expect("canonical Value always serializes");
    hex_sha256(&bytes)
}

/// Recursively sort object keys so serialization is deterministic. Numbers keep
/// their `serde_json` representation (int stays int); scalars pass through.
fn canonical_params_value(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_params_value).collect()),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonical_params_value(&map[k]));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// `params_hash` for a series identity: an optional unaggregated `source` plus
/// zero or more inline indicator params (`[sma.1h{period:31}]`). Composes both
/// into one canonical params object, so the result matches whatever the loader
/// stored. No source and no params → [`EMPTY_PARAMS_HASH`].
pub fn params_hash_for_identity(
    source: Option<&str>,
    params: &[(String, serde_json::Value)],
) -> String {
    if source.is_none() && params.is_empty() {
        return EMPTY_PARAMS_HASH.to_string();
    }
    let mut map = serde_json::Map::new();
    if let Some(s) = source {
        map.insert("source".to_string(), serde_json::Value::String(s.to_string()));
    }
    for (k, v) in params {
        map.insert(k.clone(), v.clone());
    }
    params_hash(&serde_json::Value::Object(map))
}

/// `params_hash` for a series scan.
///
/// - Aggregated / canonical (`[close.1d]`): SHA-256 of `{}` ([`EMPTY_PARAMS_HASH`]).
/// - Unaggregated (`[binance:close.1d]`): SHA-256 of compact JSON `{"source":"<source>"}`.
pub fn params_hash_for_source(source: Option<&str>) -> String {
    params_hash_for_identity(source, &[])
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

    // Indicator params must hash identically to what the analytics loader wrote
    // (mcdx-datastore stream_params.rs). These constants are the sha256 of the
    // canonical compact JSON — drift here silently unmatches stored rows.
    #[test]
    fn indicator_period_hashes_match_loader() {
        let p = |n: i64| params_hash_for_identity(None, &[("period".to_string(), n.into())]);
        assert_eq!(
            p(96),
            "5f544ae02055d958bbbb70f1018b5758e8025bc998c5f60d5c0d9118eb9b7e26"
        );
        assert_eq!(
            p(31),
            "c5269d3c45d8d87c159d3f57ff76b94ca5c53bfc4387b31e8ed680fd2f829e38"
        );
        assert_eq!(
            p(7),
            "b811f7c4ac4ca079a2d6c5f858cbdd51bbf2a24e20a7934013ab1741e4c7bd71"
        );
    }

    #[test]
    fn params_hash_is_key_order_independent() {
        let a = serde_json::json!({"a": 1, "b": 2});
        let b = serde_json::json!({"b": 2, "a": 1});
        assert_eq!(params_hash(&a), params_hash(&b));
    }

    #[test]
    fn empty_identity_is_empty_hash() {
        assert_eq!(params_hash_for_identity(None, &[]), EMPTY_PARAMS_HASH);
        assert_eq!(params_hash(&serde_json::json!({})), EMPTY_PARAMS_HASH);
    }

    #[test]
    fn integers_stay_integers() {
        // {"period":96} not {"period":96.0}
        let h = params_hash(&serde_json::json!({"period": 96}));
        assert_eq!(
            h,
            "5f544ae02055d958bbbb70f1018b5758e8025bc998c5f60d5c0d9118eb9b7e26"
        );
    }
}