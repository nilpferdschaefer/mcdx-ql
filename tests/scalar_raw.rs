//! Compile tests for raw stored-scalar series: bare fetch from the `data` fact
//! table for caller-resolved `kind='data'` stems (e.g. `sma`, `rsi`). Mirrors
//! the obj raw-fetch path. `close` keeps the computed analytics engine and is
//! never routed here.

use std::collections::{BTreeMap, BTreeSet};

use mcdx_ql::{compile, CompileRequest, ParamValue};

fn base_params() -> BTreeMap<String, ParamValue> {
    BTreeMap::from([
        ("from".into(), ParamValue::Int(1_700_000_000_000)),
        ("to".into(), ParamValue::Int(1_700_086_400_000)),
        ("period".into(), ParamValue::Int(14)),
    ])
}

/// Request with `sma` resolved as a stored scalar (`data`) series, as the
/// datastore would from `series_slot` (kind='data') scoped to the assets.
fn raw_req(expr: &str) -> CompileRequest {
    CompileRequest {
        expr: expr.to_string(),
        reporting_period: None,
        assets: vec!["BTC".into()],
        params: base_params(),
        after_ts: -1,
        limit: 16,
        publish_from: None,
        obj_data_types: BTreeSet::new(),
        scalar_data_types: BTreeSet::from(["sma".to_string()]),
    }
}

#[test]
fn bare_stored_scalar_fetches_from_data() {
    let q = compile(&raw_req("[sma.1h; $from:$to]")).unwrap();
    assert!(q.sql.contains("FROM data c"), "sql:\n{}", q.sql);
    assert!(q.sql.contains("c.value::text AS v_0"), "sql:\n{}", q.sql);
    assert!(q.sql.contains("c.data_type = 'sma'"), "sql:\n{}", q.sql);
    assert!(
        !q.sql.contains("FROM obj o"),
        "must not touch obj:\n{}",
        q.sql
    );
    assert_eq!(q.indicators, vec!["value".to_string()]);
    // Same 8 positional binds as the scalar / obj envelopes.
    assert_eq!(q.binds.len(), 8);
}

#[test]
fn stored_scalar_in_operator_is_rejected() {
    let err = compile(&raw_req("AVG([sma.1h; $from:$to], $period)")).unwrap_err();
    assert!(
        err.message.contains("cannot be wrapped in an operator"),
        "got: {}",
        err.message
    );
}

#[test]
fn field_accessor_on_stored_scalar_is_rejected() {
    let err = compile(&raw_req("[sma.1h->foo; $from:$to]")).unwrap_err();
    assert!(
        err.message.contains("requires an object series"),
        "got: {}",
        err.message
    );
}

#[test]
fn data_and_obj_overlap_is_rejected() {
    // `series_slot` exclusivity is per-identity: the same stem can be `data`
    // for one identity and `obj` for another. When both resolve for the queried
    // assets, one query cannot target both fact tables.
    let mut req = raw_req("[sma.1h; $from:$to]");
    req.obj_data_types = BTreeSet::from(["sma".to_string()]);
    let err = compile(&req).unwrap_err();
    assert!(
        err.message.contains("both `data` and `obj`"),
        "got: {}",
        err.message
    );
}

#[test]
fn mixing_stored_scalar_with_close_is_rejected() {
    let err = compile(&raw_req(
        "{ px: [close.1h; $from:$to], s: [sma.1h; $from:$to] }",
    ))
    .unwrap_err();
    assert!(
        err.message.contains("cannot mix raw stored-scalar"),
        "got: {}",
        err.message
    );
}

#[test]
fn unknown_scalar_still_rejected() {
    // `ema` is not resolved in scalar_data_types here, and is not `close`.
    let err = compile(&raw_req("[ema.1h; $from:$to]")).unwrap_err();
    assert!(
        err.message.contains("unknown series [ema]"),
        "got: {}",
        err.message
    );
}

#[test]
fn close_keeps_analytics_engine_when_scalar_types_present() {
    // `close` is never routed to the raw path even when other scalar stems are
    // resolved; it stays on the computed engine and reads `data`.
    let q = compile(&raw_req("[close.1h; $from:$to]")).unwrap();
    assert!(q.sql.contains("FROM data c"), "sql:\n{}", q.sql);
    assert!(q.sql.contains("c.data_type = 'close'"), "sql:\n{}", q.sql);
}

#[test]
fn two_different_stored_scalar_stems_rejected() {
    let mut req = raw_req("{ a: [sma.1h; $from:$to], b: [rsi.1h; $from:$to] }");
    req.scalar_data_types = BTreeSet::from(["sma".to_string(), "rsi".to_string()]);
    let err = compile(&req).unwrap_err();
    assert!(
        err.message.contains("share the same stem"),
        "got: {}",
        err.message
    );
}
