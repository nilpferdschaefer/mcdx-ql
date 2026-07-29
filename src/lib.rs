//! Indicator expression grammar → joint analytics SQL.
//!
//! Parses the MCDX indicator grammar and compiles it to the shared CTE pipeline
//! that analytics runs against `core.data`.

mod ast;
mod compile;
mod error;
mod interval;
mod lex;
mod parse;
mod sem;

pub use ast::{AssetRef, BatchExpr, BinOp, CallOp, DomainBound, Expr, LookbackBound, Series};
pub use compile::{
    compile, compile_batch, compile_expr, BindValue, CompiledQuery, CompileRequest, Scaffolds,
};
pub use error::{Error, ErrorCode};
pub use interval::{interval_ms, IntervalError};
pub use lex::tokenize;
pub use parse::{parse_batch, parse_expr};
pub use sem::{analyze, Analysis, Domain, ParamValue};

/// Fingerprint used for bare close bars (`params_hash` of empty params).
/// SHA-256 of the empty string — matches analytics empty-params convention.
pub const EMPTY_PARAMS_HASH: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
