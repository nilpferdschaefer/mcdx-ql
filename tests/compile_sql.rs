//! End-to-end compile tests for grammar → SQL.

use std::collections::BTreeMap;

use mcdx_ql::{compile, BindValue, CompileRequest, ParamValue};
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
        reporting_period: "1d".into(),
        assets: vec!["BTC".into(), "ETH".into()],
        params: base_params(),
        after_ts: -1,
        limit: 16,
        publish_from: None,
    }
}

#[test]
fn compiles_avg_trailing_sugar() {
    let q = compile(&req("AVG([close; $from:$to], $period)")).unwrap();
    assert_eq!(q.max_lookback, 14);
    assert_eq!(q.domain.from_ms, 1_700_000_000_000);
    assert_eq!(q.domain.to_ms, 1_700_086_400_000);
    assert_eq!(q.interval_ms, 86_400_000);
    assert_eq!(q.indicators, vec!["value".to_string()]);
    assert!(q.sql.contains("AVG(e.close) OVER w_close_1d_14"));
    assert!(q.sql.contains("MAX(e.version) OVER w_close_1d_14"));
    assert!(q.sql.contains("(COUNT(*) OVER w_close_1d_14) >= 14"));
    assert!(q.sql.contains("ROWS BETWEEN 13 PRECEDING AND CURRENT ROW"));
    assert!(q.sql.contains("dirty_from"));
    assert!(q.sql.contains("LATERAL (VALUES"));
    assert!(q.sql.contains("('value', v_0, ver_0, w_0)"));

    assert_eq!(
        q.binds[0],
        BindValue::TextArray(vec!["BTC".into(), "ETH".into()])
    );
    assert_eq!(q.binds[1], BindValue::BigInt(1_700_000_000_000));
    assert_eq!(q.binds[2], BindValue::BigInt(1_700_086_400_000));
    assert_eq!(q.binds[6], BindValue::Int(14));
}

#[test]
fn avg_explicit_lookback_matches_sugar() {
    let sugar = compile(&req("AVG([close; $from:$to], $period)")).unwrap();
    let explicit = compile(&req("AVG([close; $from:$to], t-($period-1), t)")).unwrap();
    assert!(explicit.sql.contains("AVG(e.close) OVER w_close_1d_14"));
    assert_eq!(sugar.max_lookback, explicit.max_lookback);
}

#[test]
fn compiles_batch_shared_envelope() {
    let q = compile(&req(
        "{ sma_14: AVG([close; $from:$to], 14), ema_14: EMA([close; $from:$to], 14) }",
    ))
    .unwrap();
    assert_eq!(q.indicators, vec!["ema_14".to_string(), "sma_14".to_string()]);
    assert!(q.sql.contains("('ema_14', v_0, ver_0, w_0)"));
    assert!(q.sql.contains("('sma_14', v_1, ver_1, w_1)"));
    assert!(q.scaffolds.closes_to_date);
    assert!(q.sql.contains("closes_to_date"));
}

#[test]
fn compiles_rsi_warmup_period_plus_one() {
    let q = compile(&req("RSI([close; $from:$to], $period)")).unwrap();
    assert_eq!(q.max_lookback, 15);
    assert!(q.sql.contains("cardinality(e.closes_to_date) >= 15"));
}

#[test]
fn compiles_market_qualifier() {
    let q = compile(&req(
        "REGR_SLOPE(RET([close; $from:$to]), RET([close@TOTALCRYPTOMARKETCAP; $from:$to]), $period)",
    ))
    .unwrap();
    assert!(q.scaffolds.bar_ret);
    assert!(q
        .scaffolds
        .market_tickers
        .contains(&"TOTALCRYPTOMARKETCAP".to_string()));
    assert!(q.sql.contains("market_totalcryptomarketcap"));
    assert!(q.sql.contains("m_totalcryptomarketcap.bar_ret"));
    assert!(q.sql.contains("REGR_SLOPE(e.bar_ret, m_totalcryptomarketcap.bar_ret)"));
}

#[test]
fn compiles_benchmark_param() {
    let mut r = req("RET([close@$benchmark; $from:$to])");
    r.params
        .insert("benchmark".into(), ParamValue::Text("TOTALCRYPTOMARKETCAP".into()));
    let q = compile(&r).unwrap();
    assert!(q
        .scaffolds
        .market_tickers
        .iter()
        .any(|t| t == "TOTALCRYPTOMARKETCAP"));
}

#[test]
fn rejects_unknown_series() {
    let err = compile(&req("AVG([cloze; $from:$to], $period)")).unwrap_err();
    assert_eq!(err.code.as_str(), "sem_error");
    assert!(err.message.contains("unknown series [cloze]"));
    let json = err.to_error_json();
    assert_eq!(json["code"], "sem_error");
}

#[test]
fn rejects_missing_param() {
    let mut r = req("AVG([close; $from:$to], $period)");
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
fn var_and_std_fragments() {
    let q = compile(&req("STD([close; $from:$to], $period)")).unwrap();
    assert!(q.sql.contains("SQRT(GREATEST(0,"));
    assert!(q.sql.contains("AVG(e.close * e.close) OVER w_close_1d_14"));
}
