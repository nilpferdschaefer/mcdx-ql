//! End-to-end compile tests for grammar → SQL.

use std::collections::BTreeMap;

use mcdx_ql::{compile, BindValue, CompileRequest, Domain, ParamValue};
use pretty_assertions::assert_eq;

fn base_params() -> BTreeMap<String, ParamValue> {
    BTreeMap::from([
        ("period".into(), ParamValue::Int(14)),
        ("from".into(), ParamValue::Int(1_700_000_000_000)),
        ("to".into(), ParamValue::Int(1_700_086_400_000)),
    ])
}

fn req(expr: &str) -> CompileRequest {
    CompileRequest {
        expr: expr.to_string(),
        reporting_period: None,
        assets: vec!["BTC".into(), "ETH".into()],
        params: base_params(),
        after_ts: -1,
        limit: 16,
        publish_from: None,
    }
}

#[test]
fn compiles_avg_trailing_sugar() {
    let q = compile(&req("AVG([close.1d; $from:$to], $period)")).unwrap();
    assert_eq!(q.max_lookback, 14);
    assert_eq!(q.reporting_period, "1d");
    assert_eq!(
        q.domain,
        Domain::Absolute {
            from_param: "from".into(),
            to_param: "to".into(),
            from_ms: 1_700_000_000_000,
            to_ms: 1_700_086_400_000,
        }
    );
    assert_eq!(q.interval_ms, 86_400_000);
    assert!(q.sql.contains("AVG(e.close) OVER w_close_1d_14"));
    assert!(q.sql.contains("bounds AS ("));
    assert!(q.sql.contains("COALESCE(p.dirty_from, l.ts) AS emit_from"));
    assert_eq!(q.binds[1], BindValue::BigInt(1_700_000_000_000));
    assert_eq!(q.binds[2], BindValue::BigInt(1_700_086_400_000));
}

#[test]
fn latest_domain_omitted() {
    let q = compile(&req("AVG([close.1d], $period)")).unwrap();
    assert_eq!(q.domain, Domain::Latest);
    assert_eq!(q.binds[1], BindValue::Null);
    assert_eq!(q.binds[2], BindValue::Null);
    assert!(q.sql.contains("COALESCE(p.dirty_from, l.ts)"));
    assert!(q.sql.contains("b.emit_from"));
    assert!(q.sql.contains("b.emit_to"));
}

#[test]
fn bucket_1h() {
    let q = compile(&req("AVG([close.1h; $from:$to], $period)")).unwrap();
    assert_eq!(q.reporting_period, "1h");
    assert_eq!(q.interval_ms, 3_600_000);
    assert!(q.sql.contains("w_close_1h_14"));
    assert!(q.sql.contains("reporting_period = '1h'"));
}

#[test]
fn rejects_missing_bucket() {
    let err = compile(&req("AVG([close; $from:$to], $period)")).unwrap_err();
    assert!(err.message.contains("bucket required"));
}

#[test]
fn rejects_request_period_mismatch() {
    let mut r = req("AVG([close.1d; $from:$to], $period)");
    r.reporting_period = Some("1h".into());
    let err = compile(&r).unwrap_err();
    assert!(err.message.contains("conflicts"));
}

#[test]
fn avg_explicit_lookback_matches_sugar() {
    let sugar = compile(&req("AVG([close.1d; $from:$to], $period)")).unwrap();
    let explicit = compile(&req("AVG([close.1d; $from:$to], t-($period-1), t)")).unwrap();
    assert!(explicit.sql.contains("AVG(e.close) OVER w_close_1d_14"));
    assert_eq!(sugar.max_lookback, explicit.max_lookback);
}

#[test]
fn compiles_batch_shared_envelope() {
    let q = compile(&req(
        "{ sma_14: AVG([close.1d; $from:$to], 14), ema_14: EMA([close.1d; $from:$to], 14) }",
    ))
    .unwrap();
    assert_eq!(q.indicators, vec!["ema_14".to_string(), "sma_14".to_string()]);
    assert!(q.sql.contains("('ema_14', v_0, ver_0, w_0)"));
    assert!(q.sql.contains("('sma_14', v_1, ver_1, w_1)"));
    assert!(q.scaffolds.closes_to_date);
}

#[test]
fn compiles_rsi_warmup_period_plus_one() {
    let q = compile(&req("RSI([close.1d; $from:$to], $period)")).unwrap();
    assert_eq!(q.max_lookback, 15);
    assert!(q.sql.contains("array_length(e.closes_to_date, 1) >= 15"));
}

#[test]
fn compiles_ema_close_sql_shape() {
    let q = compile(&req("EMA([close.1d; $from:$to], $period)")).unwrap();
    assert!(q.sql.contains("WITH RECURSIVE vals AS"));
    assert!(q.sql.contains("array_length(e.closes_to_date, 1) < 14"));
}

#[test]
fn compiles_atr_wilder_seed_bars() {
    let q = compile(&req("RMA(TR([close.1d; $from:$to]), $period)")).unwrap();
    assert!(q.sql.contains("WHERE t.ord BETWEEN 2 AND 15"));
}

#[test]
fn compiles_std_of_ret() {
    let q = compile(&req("STD(RET([close.1d; $from:$to]), $period)")).unwrap();
    assert!(q.scaffolds.bar_ret);
    assert!(q.sql.contains("AVG(e.bar_ret * e.bar_ret) OVER w_ret_1d_14"));
}

#[test]
fn compiles_market_qualifier() {
    let q = compile(&req(
        "REGR_SLOPE(RET([close.1d; $from:$to]), RET([close.1d@TOTALCRYPTOMARKETCAP; $from:$to]), $period)",
    ))
    .unwrap();
    assert!(q.sql.contains("market_ret AS ("));
    assert!(q.sql.contains("REGR_SLOPE(e.bar_ret, m.market_ret)"));
}

#[test]
fn compiles_benchmark_param() {
    let mut r = req("RET([close.1d@$benchmark; $from:$to])");
    r.params
        .insert("benchmark".into(), ParamValue::Text("TOTALCRYPTOMARKETCAP".into()));
    let q = compile(&r).unwrap();
    assert!(q.sql.contains("m.market_ret"));
}

#[test]
fn worked_mapping_vol_96() {
    let mut r = req("STD(RET([close.1d; $from:$to]), $period) * SQRT($bars_per_year)");
    r.params.insert("period".into(), ParamValue::Int(96));
    r.params
        .insert("bars_per_year".into(), ParamValue::Int(365));
    let q = compile(&r).unwrap();
    assert!(q.sql.contains("SQRT(365)"));
}

#[test]
fn worked_mapping_sep_atr() {
    let mut r = req(
        "(AVG([close.1d; $from:$to], $fast) - AVG([close.1d; $from:$to], $slow)) / RMA(TR([close.1d; $from:$to]), $atr)",
    );
    r.params.insert("fast".into(), ParamValue::Int(5));
    r.params.insert("slow".into(), ParamValue::Int(50));
    r.params.insert("atr".into(), ParamValue::Int(14));
    let q = compile(&r).unwrap();
    assert!(q.sql.contains("AVG(e.close) OVER w_close_1d_5"));
    assert_eq!(q.max_lookback, 50);
}

#[test]
fn worked_mapping_beta_31() {
    let mut r = req(
        "REGR_SLOPE(RET([close.1d; $from:$to]), RET([close.1d@$benchmark; $from:$to]), $period)",
    );
    r.params.insert("period".into(), ParamValue::Int(31));
    r.params
        .insert("benchmark".into(), ParamValue::Text("TOTALCRYPTOMARKETCAP".into()));
    let q = compile(&r).unwrap();
    assert!(q.sql.contains("REGR_SLOPE(e.bar_ret, m.market_ret) OVER w_regr_1d_31"));
}

#[test]
fn rejects_unknown_series() {
    let err = compile(&req("AVG([cloze.1d; $from:$to], $period)")).unwrap_err();
    assert!(err.message.contains("unknown series [cloze]"));
}

#[test]
fn rejects_missing_param() {
    let mut r = req("AVG([close.1d; $from:$to], $period)");
    r.params.remove("period");
    let err = compile(&r).unwrap_err();
    assert!(err.message.contains("missing param `$period`"));
}

#[test]
fn rejects_bare_identifier() {
    let err = compile(&req("AVG(close, $period)")).unwrap_err();
    assert_eq!(err.code.as_str(), "parse_error");
}

#[test]
fn rejects_rsi_period_lt_2() {
    let mut r = req("RSI([close.1d; $from:$to], $period)");
    r.params.insert("period".into(), ParamValue::Int(1));
    let err = compile(&r).unwrap_err();
    assert!(err.message.contains("RSI requires $period >= 2"));
}

#[test]
fn var_and_std_fragments() {
    let q = compile(&req("STD([close.1d; $from:$to], $period)")).unwrap();
    assert!(q.sql.contains("AVG(e.close * e.close) OVER w_close_1d_14"));
}
