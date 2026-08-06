//! Compile tests for object (`obj`) series: raw fetch + `->field` projection.
//! The `->` object accessor is additive — scalar `data` behaviour is unchanged
//! (covered by compile_sql.rs) and is only exercised here for object series.

use std::collections::{BTreeMap, BTreeSet};

use mcdx_ql::{compile, CompileRequest, ParamValue};

fn base_params() -> BTreeMap<String, ParamValue> {
    BTreeMap::from([
        ("from".into(), ParamValue::Int(1_700_000_000_000)),
        ("to".into(), ParamValue::Int(1_700_086_400_000)),
    ])
}

/// Request with `candles` resolved as an object series (as the datastore would
/// from `series_slot`).
fn obj_req(expr: &str) -> CompileRequest {
    CompileRequest {
        expr: expr.to_string(),
        reporting_period: None,
        assets: vec!["BTC".into()],
        params: base_params(),
        after_ts: -1,
        limit: 16,
        publish_from: None,
        obj_data_types: BTreeSet::from(["candles".to_string()]),
    }
}

#[test]
fn bare_object_series_fetches_from_obj() {
    let q = compile(&obj_req("[candles.1h; $from:$to]")).unwrap();
    assert!(q.sql.contains("FROM obj o"), "sql:\n{}", q.sql);
    // Raw object fetch: the whole jsonb value as text (discriminated as object).
    assert!(q.sql.contains("o.value::text AS v_0"), "sql:\n{}", q.sql);
    assert!(q.sql.contains("o.data_type = 'candles'"));
    assert!(!q.sql.contains("FROM data c"), "must not touch data table:\n{}", q.sql);
    assert_eq!(q.indicators, vec!["value".to_string()]);
    // Same 8 positional binds as the scalar path.
    assert_eq!(q.binds.len(), 8);
}

#[test]
fn object_field_projection_extracts_scalar() {
    let q = compile(&obj_req("[candles.1h->close; $from:$to]")).unwrap();
    assert!(
        q.sql.contains("(o.value->>'close') AS v_0"),
        "sql:\n{}",
        q.sql
    );
    assert!(q.sql.contains("FROM obj o"));
}

#[test]
fn batch_mixes_bare_object_and_field_projections() {
    let q =
        compile(&obj_req("{ bar: [candles.1h], hi: [candles.1h->high], lo: [candles.1h->low] }"))
            .unwrap();
    assert!(q.sql.contains("o.value::text AS v_"), "sql:\n{}", q.sql);
    assert!(q.sql.contains("(o.value->>'high')"), "sql:\n{}", q.sql);
    assert!(q.sql.contains("(o.value->>'low')"), "sql:\n{}", q.sql);
    // The unpivot carries the batch member names as indicators.
    assert!(q.sql.contains("'bar'") && q.sql.contains("'hi'") && q.sql.contains("'lo'"));
}

#[test]
fn field_accessor_on_scalar_series_is_rejected() {
    // `close` is a scalar (`data`) series here; `->close` is invalid on it.
    let err = compile(&obj_req("[close.1d->close]")).unwrap_err();
    assert!(
        err.message.contains("requires an object series"),
        "got: {}",
        err.message
    );
}

#[test]
fn object_series_in_operator_is_rejected() {
    let err = compile(&obj_req("AVG([candles.1h->close], 14)")).unwrap_err();
    assert!(
        err.message.contains("cannot be wrapped in an operator"),
        "got: {}",
        err.message
    );
}

#[test]
fn mixing_object_and_scalar_series_is_rejected() {
    let err = compile(&obj_req("{ px: [close.1h], candle: [candles.1h] }")).unwrap_err();
    assert!(
        err.message.contains("cannot mix object"),
        "got: {}",
        err.message
    );
}

#[test]
fn scalar_series_unaffected_when_obj_types_present() {
    // `close` is not in obj_data_types → scalar path, reads `data` as before.
    let q = compile(&obj_req("[close.1d; $from:$to]")).unwrap();
    assert!(q.sql.contains("FROM data c"), "sql:\n{}", q.sql);
    assert!(!q.sql.contains("FROM obj o"));
}
