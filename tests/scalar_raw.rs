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

// SHA-256 of the canonical compact JSON — must match what the analytics loader
// wrote for the stored variant (mcdx-datastore stream_params.rs).
const HASH_PERIOD_31: &str = "c5269d3c45d8d87c159d3f57ff76b94ca5c53bfc4387b31e8ed680fd2f829e38";
const HASH_EMPTY: &str = "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";

#[test]
fn inline_params_drive_the_scan_hash() {
    let q = compile(&raw_req("[sma.1h{period:31}; $from:$to]")).unwrap();
    // Both the bounds scan and the src scan must filter the period-31 identity.
    assert!(
        q.sql
            .contains(&format!("c.params_hash = '{HASH_PERIOD_31}'")),
        "sql should filter the period:31 hash:\n{}",
        q.sql
    );
    assert!(
        !q.sql.contains(HASH_EMPTY),
        "must not fall back to the empty-params hash:\n{}",
        q.sql
    );
    assert_eq!(q.params_hash, HASH_PERIOD_31);
    assert!(q.sql.contains("c.data_type = 'sma'"), "sql:\n{}", q.sql);
}

#[test]
fn bare_stored_scalar_uses_empty_hash() {
    let q = compile(&raw_req("[sma.1h; $from:$to]")).unwrap();
    assert_eq!(q.params_hash, HASH_EMPTY);
    assert!(
        q.sql.contains(&format!("c.params_hash = '{HASH_EMPTY}'")),
        "sql:\n{}",
        q.sql
    );
}

#[test]
fn mixing_stored_variants_in_one_query_is_rejected() {
    // period:31 vs period:7 resolve to different identities — not yet supported
    // in a single query (Phase 2). Must be a clear error, not a silent wrong scan.
    let err = compile(&raw_req(
        "{ s31: [sma.1h{period:31}; $from:$to], s7: [sma.1h{period:7}; $from:$to] }",
    ))
    .unwrap_err();
    assert!(
        err.message.contains("share the same identity"),
        "got: {}",
        err.message
    );
}
