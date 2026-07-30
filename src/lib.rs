//! Indicator expression grammar → joint analytics SQL.
//!
//! Parses the MCDX indicator grammar and compiles it to the shared CTE pipeline
//! that analytics runs against `core.data` or `core.obj`.
//!
//! Series literals carry the bar bucket and an optional emit domain:
//!
//! - `[close.1d; $from:$to]` — daily closes over an absolute ms range
//! - `[close.1h]` — full possible hourly series from available data
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

pub use ast::{
    AssetRef, BatchExpr, BinOp, CallOp, DomainBound, EmitCount, EmitEnd, Expr, IndexSelector,
    LookbackBound, Series, SeriesDomain,
};
pub use compile::{
    compile, compile_batch, compile_expr, BindValue, CompiledQuery, CompileRequest, Scaffolds,
    SourceTable,
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

/// Fingerprint used for bare close bars (`params_hash` of empty params).
/// SHA-256 of the JSON object `{}` — matches analytics empty-params convention.
pub const EMPTY_PARAMS_HASH: &str =
    "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
