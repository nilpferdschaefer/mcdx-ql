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
    assert!(q.sql.contains("p.dirty_from AS emit_from"));
    assert!(q.sql.contains("p.dirty_to AS emit_to"));
    assert_eq!(q.binds[1], BindValue::BigInt(1_700_000_000_000));
    assert_eq!(q.binds[2], BindValue::BigInt(1_700_086_400_000));
}

#[test]
fn full_domain_omitted() {
    let q = compile(&req("AVG([close.1d], $period)")).unwrap();
    assert_eq!(q.domain, Domain::Full);
    assert_eq!(q.binds[1], BindValue::Null);
    assert_eq!(q.binds[2], BindValue::Null);
    assert!(q.sql.contains("l.min_ts AS emit_from"));
    assert!(q.sql.contains("l.max_ts AS emit_to"));
    assert!(q.sql.contains("b.emit_from"));
    assert!(q.sql.contains("b.emit_to"));
}

#[test]
fn last_result_slice() {
    let q = compile(&req("AVG([close.1d], $period)[-1]")).unwrap();
    assert_eq!(
        q.domain,
        Domain::TrailingLatest {
            bars: 1,
            end_offset: 0
        }
    );
    assert!(q.sql.contains("l.max_ts - 0 AS emit_to") || q.sql.contains("l.max_ts AS emit_to"));
    // emit_from = max - 0 for 1 bar
    assert!(q.sql.contains("(l.max_ts - 0) - 0 AS emit_from") || q.sql.contains("AS emit_from"));
}

#[test]
fn trailing_result_slice() {
    let q = compile(&req("AVG([close.1d], 14)[-10:-1]")).unwrap();
    assert_eq!(
        q.domain,
        Domain::TrailingLatest {
            bars: 10,
            end_offset: 0
        }
    );
    assert!(q.sql.contains(&format!("(l.max_ts - 0) - {}", 9_i64 * 86_400_000)));
}

#[test]
fn positive_result_index() {
    let q = compile(&req("AVG([close.1d], 14)[4]")).unwrap();
    assert_eq!(q.domain, Domain::FromStart { start: 4, count: 1 });
    assert!(q.sql.contains("p.max_lookback::bigint + 4::bigint - 2"));
}

#[test]
fn trailing_bars_ending_at_date() {
    // 100 daily bars ending 2026-05-15 UTC (bar open ms)
    let end = 1_778_803_200_000_i64;
    let mut r = req(
        "REGR_SLOPE(RET([close.1d@self; 100@$end]), RET([close.1d@$b; 100@$end]), $period)",
    );
    r.params.insert("end".into(), ParamValue::Int(end));
    r.params.insert("b".into(), ParamValue::Text("ETH".into()));
    r.params.insert("period".into(), ParamValue::Int(31));
    let q = compile(&r).unwrap();
    assert_eq!(
        q.domain,
        Domain::Absolute {
            from_param: "end-(100-1)*interval".into(),
            to_param: "end".into(),
            from_ms: end - 99 * 86_400_000,
            to_ms: end,
        }
    );
    assert_eq!(q.binds[1], BindValue::BigInt(end - 99 * 86_400_000));
    assert_eq!(q.binds[2], BindValue::BigInt(end));
    assert!(q.sql.contains("REGR_SLOPE(e.bar_ret, m.market_ret)"));
    // One joint query emits the whole 100-bar range (paginated by limit).
    assert_eq!(q.indicators, vec!["value".to_string()]);
}

#[test]
fn trailing_bars_ending_latest() {
    let q = compile(&req("AVG([close.1d; 100@latest], $period)")).unwrap();
    assert_eq!(
        q.domain,
        Domain::TrailingLatest {
            bars: 100,
            end_offset: 0
        }
    );
    assert_eq!(q.binds[1], BindValue::Null);
    assert_eq!(q.binds[2], BindValue::Null);
    assert!(q.sql.contains(&format!("(l.max_ts - 0) - {}", 99_i64 * 86_400_000)));
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
        "REGR_SLOPE(RET([close.1d@self; $from:$to]), RET([close.1d@TOTALCRYPTOMARKETCAP; $from:$to]), $period)",
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
        "REGR_SLOPE(RET([close.1d@self; $from:$to]), RET([close.1d@$benchmark; $from:$to]), $period)",
    );
    r.params.insert("period".into(), ParamValue::Int(31));
    r.params
        .insert("benchmark".into(), ParamValue::Text("TOTALCRYPTOMARKETCAP".into()));
    let q = compile(&r).unwrap();
    assert!(q.sql.contains("REGR_SLOPE(e.bar_ret, m.market_ret) OVER w_regr_1d_31"));
}

#[test]
fn compiles_regr_alias_1h_31() {
    // User-facing REGR spelling: 31-period 1h beta vs a benchmark param.
    let mut r = req(
        "REGR(RET([close.1h@self; $from:$to]), RET([close.1h@$benchmark; $from:$to]), 31)",
    );
    r.params
        .insert("benchmark".into(), ParamValue::Text("TOTALCRYPTOMARKETCAP".into()));
    let q = compile(&r).unwrap();
    assert_eq!(q.reporting_period, "1h");
    assert_eq!(q.max_lookback, 31);
    assert!(q.scaffolds.bar_ret);
    assert!(q.sql.contains("REGR_SLOPE(e.bar_ret, m.market_ret) OVER w_regr_1h_31"));
    assert!(q.sql.contains("market_ret AS ("));
}

#[test]
fn regr_matches_regr_slope() {
    // REGR is a pure alias — identical SQL to REGR_SLOPE for the same expression.
    let regr = compile(&req(
        "REGR(RET([close.1d@self; $from:$to]), RET([close.1d@TOTALCRYPTOMARKETCAP; $from:$to]), $period)",
    ))
    .unwrap();
    let regr_slope = compile(&req(
        "REGR_SLOPE(RET([close.1d@self; $from:$to]), RET([close.1d@TOTALCRYPTOMARKETCAP; $from:$to]), $period)",
    ))
    .unwrap();
    assert_eq!(regr.sql, regr_slope.sql);
    assert_eq!(regr.binds, regr_slope.binds);
}

#[test]
fn self_row_matches_implicit_row_single_series() {
    // `@self` is codegen-identical to the implicit row asset when no other
    // asset is referenced.
    let explicit = compile(&req("AVG([close.1d@self; $from:$to], $period)")).unwrap();
    let implicit = compile(&req("AVG([close.1d; $from:$to], $period)")).unwrap();
    assert_eq!(explicit.sql, implicit.sql);
    assert_eq!(explicit.binds, implicit.binds);
}

#[test]
fn rejects_mixed_implicit_and_qualified_series() {
    // Implicit row series mixed with an `@`-qualified benchmark must be rejected.
    let mut r = req(
        "REGR(RET([close.1h; $from:$to]), RET([close.1h@$benchmark; $from:$to]), 31)",
    );
    r.params
        .insert("benchmark".into(), ParamValue::Text("TOTALCRYPTOMARKETCAP".into()));
    let err = compile(&r).unwrap_err();
    assert!(
        err.message.contains("@self") && err.message.contains("more than one asset"),
        "{}",
        err.message
    );
}

#[test]
fn compiles_regr_two_qualified_series() {
    // Both series qualified after `@` → each ticker gets its own market_ret CTE.
    let q = compile(&req(
        "REGR(RET([close.1h@BTC; $from:$to]), RET([close.1h@ETH; $from:$to]), 31)",
    ))
    .unwrap();
    assert!(q.scaffolds.bar_ret);
    assert_eq!(q.scaffolds.market_tickers.len(), 2);
    assert!(q.sql.contains("market_ret_btc AS ("));
    assert!(q.sql.contains("market_ret_eth AS ("));
    assert!(q
        .sql
        .contains("REGR_SLOPE(m_btc.market_ret, m_eth.market_ret) OVER w_regr_1h_31"));
    assert!(q.sql.contains("LEFT JOIN market_ret_btc m_btc"));
    assert!(q.sql.contains("LEFT JOIN market_ret_eth m_eth"));
}

#[test]
fn postfix_range_matches_inline_domains() {
    // Applying `[$from:$to]` on the whole REGR is equivalent to repeating the
    // range on every child series — descendants inherit it.
    let mut inline = req(
        "REGR(RET([close.1d@self; $from:$to]), RET([close.1d@$b; $from:$to]), $period)",
    );
    inline.params.insert("b".into(), ParamValue::Text("ETH".into()));
    inline.params.insert("period".into(), ParamValue::Int(31));

    let mut postfix = req(
        "REGR(RET([close.1d@self]), RET([close.1d@$b]), $period)[$from:$to]",
    );
    postfix.params.insert("b".into(), ParamValue::Text("ETH".into()));
    postfix.params.insert("period".into(), ParamValue::Int(31));

    let qi = compile(&inline).unwrap();
    let qp = compile(&postfix).unwrap();
    assert_eq!(qp.sql, qi.sql);
    assert_eq!(qp.binds, qi.binds);
    assert_eq!(
        qp.domain,
        Domain::Absolute {
            from_param: "from".into(),
            to_param: "to".into(),
            from_ms: 1_700_000_000_000,
            to_ms: 1_700_086_400_000,
        }
    );
}

#[test]
fn compiles_postfix_range_on_avg() {
    // Range applied to a single-series expression sets the absolute emit domain.
    let q = compile(&req("AVG([close.1d], $period)[$from:$to]")).unwrap();
    assert_eq!(
        q.domain,
        Domain::Absolute {
            from_param: "from".into(),
            to_param: "to".into(),
            from_ms: 1_700_000_000_000,
            to_ms: 1_700_086_400_000,
        }
    );
    assert_eq!(q.binds[1], BindValue::BigInt(1_700_000_000_000));
    assert_eq!(q.binds[2], BindValue::BigInt(1_700_086_400_000));
    assert!(q.sql.contains("AVG(e.close) OVER w_close_1d_14"));
    assert!(q.sql.contains("p.dirty_from AS emit_from"));
}

#[test]
fn rejects_nested_range_series_and_postfix() {
    // Inner series range + outer postfix range → conflict, rejected at parse.
    let mut r = req(
        "REGR(RET([close.1d@self; $from:$to]), RET([close.1d@$b]), $period)[$from:$to]",
    );
    r.params.insert("b".into(), ParamValue::Text("ETH".into()));
    r.params.insert("period".into(), ParamValue::Int(31));
    let err = compile(&r).unwrap_err();
    assert_eq!(err.code.as_str(), "parse_error");
    assert!(
        err.message.contains("only one level"),
        "{}",
        err.message
    );
}

#[test]
fn rejects_double_postfix_range() {
    let err = compile(&req("AVG([close.1d], 14)[$from:$to][$from:$to]")).unwrap_err();
    assert_eq!(err.code.as_str(), "parse_error");
    assert!(err.message.contains("only one level"), "{}", err.message);
}

#[test]
fn batch_postfix_range_matches_inline_domains() {
    // `{ … }[$from:$to]` is equivalent to repeating `; $from:$to` / per-member postfix.
    // Beta uses @$benchmark, so every series in the batch must be @-qualified (@self).
    let mut inline = req(
        "{ close: [close.1h@self; $from:$to], \
           vol: STD(RET([close.1h@self; $from:$to]), $vol_n) * SQRT($bars_per_year), \
           beta: REGR(RET([close.1h@self; $from:$to]), RET([close.1h@$benchmark; $from:$to]), $beta_n), \
           ema: EMA([close.1h@self; $from:$to], $ema_n) }",
    );
    inline.params.insert("vol_n".into(), ParamValue::Int(14));
    inline.params.insert("beta_n".into(), ParamValue::Int(31));
    inline.params.insert("ema_n".into(), ParamValue::Int(14));
    inline
        .params
        .insert("bars_per_year".into(), ParamValue::Float(8760.0));
    inline
        .params
        .insert("benchmark".into(), ParamValue::Text("BTC".into()));

    let mut postfix = req(
        "{ close: [close.1h@self], \
           vol: STD(RET([close.1h@self]), $vol_n) * SQRT($bars_per_year), \
           beta: REGR(RET([close.1h@self]), RET([close.1h@$benchmark]), $beta_n), \
           ema: EMA([close.1h@self], $ema_n) }[$from:$to]",
    );
    postfix.params.insert("vol_n".into(), ParamValue::Int(14));
    postfix.params.insert("beta_n".into(), ParamValue::Int(31));
    postfix.params.insert("ema_n".into(), ParamValue::Int(14));
    postfix
        .params
        .insert("bars_per_year".into(), ParamValue::Float(8760.0));
    postfix
        .params
        .insert("benchmark".into(), ParamValue::Text("BTC".into()));

    let qi = compile(&inline).unwrap();
    let qp = compile(&postfix).unwrap();
    assert_eq!(qp.sql, qi.sql);
    assert_eq!(qp.binds, qi.binds);
    assert_eq!(
        qp.indicators,
        vec![
            "beta".to_string(),
            "close".to_string(),
            "ema".to_string(),
            "vol".to_string(),
        ]
    );
    assert_eq!(
        qp.domain,
        Domain::Absolute {
            from_param: "from".into(),
            to_param: "to".into(),
            from_ms: 1_700_000_000_000,
            to_ms: 1_700_086_400_000,
        }
    );
}

#[test]
fn rejects_batch_postfix_range_with_member_domain() {
    let err = compile(&req(
        "{ close: [close.1h; $from:$to], ema: EMA([close.1h], 14) }[$from:$to]",
    ))
    .unwrap_err();
    assert_eq!(err.code.as_str(), "parse_error");
    assert!(err.message.contains("only one level"), "{}", err.message);
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

// ---------------------------------------------------------------------------
// Consumer regression QL inputs (close.1h; publish windows 14 & 31)
// Assert generated version / warmup / window / value SQL shapes.
// ---------------------------------------------------------------------------

fn assert_contains(sql: &str, fragment: &str) {
    assert!(
        sql.contains(fragment),
        "missing fragment:\n{fragment}\n--- in sql ---\n{sql}"
    );
}

fn assert_named_window(sql: &str, name: &str, preceding: i32) {
    assert_contains(
        sql,
        &format!(
            "{name} AS (\n\
             \x20     PARTITION BY e.coin, e.seg_key ORDER BY e.timestamp_start\n\
             \x20     ROWS BETWEEN {preceding} PRECEDING AND CURRENT ROW\n\
             \x20   )"
        ),
    );
}

fn assert_version_sql_hygiene(sql: &str) {
    // Bare Long.MIN_VALUE::bigint fails Postgres parse; only numeric form is OK
    // in publish_from / after_ts filters.
    assert!(
        !sql.contains("-9223372036854775808::bigint"),
        "must not emit Long.MIN_VALUE::bigint cast"
    );
    assert!(
        !sql.contains(
            "MAX(e.version) OVER (PARTITION BY e.coin, e.seg_key ORDER BY e.timestamp_start ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)"
        ),
        "period-N version must not use UNBOUNDED PRECEDING"
    );
    assert!(
        !sql.contains("COALESCE(MAX(e.version)"),
        "version must not wrap MAX in COALESCE(..., Long.MIN_VALUE)"
    );
}

fn assert_unpivot(sql: &str, stem: &str, idx: usize) {
    assert_contains(sql, &format!("('{stem}', v_{idx}, ver_{idx}, w_{idx})"));
}

#[test]
fn consumer_sql_sma_14_31() {
    // Golden reference: finite-window MAX(version).
    let q = compile(&req(
        "{ sma_14: AVG([close.1h; $from:$to], 14), sma_31: AVG([close.1h; $from:$to], 31) }",
    ))
    .unwrap();
    assert_eq!(q.indicators, vec!["sma_14".to_string(), "sma_31".to_string()]);
    assert_eq!(q.max_lookback, 31);
    assert_eq!(q.reporting_period, "1h");

    assert_contains(&q.sql, "AVG(e.close) OVER w_close_1h_14 AS v_0");
    assert_contains(&q.sql, "MAX(e.version) OVER w_close_1h_14 AS ver_0");
    assert_contains(&q.sql, "(COUNT(*) OVER w_close_1h_14) >= 14 AS w_0");
    assert_contains(&q.sql, "AVG(e.close) OVER w_close_1h_31 AS v_1");
    assert_contains(&q.sql, "MAX(e.version) OVER w_close_1h_31 AS ver_1");
    assert_contains(&q.sql, "(COUNT(*) OVER w_close_1h_31) >= 31 AS w_1");
    assert_named_window(&q.sql, "w_close_1h_14", 13);
    assert_named_window(&q.sql, "w_close_1h_31", 30);
    assert_unpivot(&q.sql, "sma_14", 0);
    assert_unpivot(&q.sql, "sma_31", 1);
    assert_version_sql_hygiene(&q.sql);
}

#[test]
fn consumer_sql_atr_14_31() {
    let q = compile(&req(
        "{ atr_14: RMA(TR([close.1h; $from:$to]), 14), atr_31: RMA(TR([close.1h; $from:$to]), 31) }",
    ))
    .unwrap();
    assert_eq!(q.indicators, vec!["atr_14".to_string(), "atr_31".to_string()]);
    assert_eq!(q.max_lookback, 32); // period+1 warmup
    assert!(q.scaffolds.closes_to_date);

    // Version matches SMA finite frames (not unbounded).
    assert_contains(&q.sql, "MAX(e.version) OVER w_close_1h_14 AS ver_0");
    assert_contains(&q.sql, "MAX(e.version) OVER w_close_1h_31 AS ver_1");
    assert_contains(&q.sql, "(array_length(e.closes_to_date, 1) >= 15) AS w_0");
    assert_contains(&q.sql, "(array_length(e.closes_to_date, 1) >= 32) AS w_1");
    // Wilder ATR value shape for both periods.
    assert_contains(&q.sql, "WHERE t.ord BETWEEN 2 AND 15");
    assert_contains(&q.sql, "WHERE t.ord BETWEEN 2 AND 32");
    assert_contains(&q.sql, "(r.atr * (14 - 1) + t.tr) / 14");
    assert_contains(&q.sql, "(r.atr * (31 - 1) + t.tr) / 31");
    assert_named_window(&q.sql, "w_close_1h_14", 13);
    assert_named_window(&q.sql, "w_close_1h_31", 30);
    assert_unpivot(&q.sql, "atr_14", 0);
    assert_unpivot(&q.sql, "atr_31", 1);
    assert_version_sql_hygiene(&q.sql);
}

#[test]
fn consumer_sql_rsi_14_31() {
    let q = compile(&req(
        "{ rsi_14: RSI([close.1h; $from:$to], 14), rsi_31: RSI([close.1h; $from:$to], 31) }",
    ))
    .unwrap();
    assert_eq!(q.indicators, vec!["rsi_14".to_string(), "rsi_31".to_string()]);
    assert_eq!(q.max_lookback, 32);
    assert!(q.scaffolds.closes_to_date);

    assert_contains(&q.sql, "MAX(e.version) OVER w_close_1h_14 AS ver_0");
    assert_contains(&q.sql, "MAX(e.version) OVER w_close_1h_31 AS ver_1");
    assert_contains(&q.sql, "(array_length(e.closes_to_date, 1) >= 15) AS w_0");
    assert_contains(&q.sql, "(array_length(e.closes_to_date, 1) >= 32) AS w_1");
    assert_contains(&q.sql, "WHERE c.ord BETWEEN 2 AND 15");
    assert_contains(&q.sql, "WHERE c.ord BETWEEN 2 AND 32");
    assert_contains(
        &q.sql,
        "ELSE 100.0 - (100.0 / (1.0 + (r.avg_gain / r.avg_loss)))",
    );
    assert_named_window(&q.sql, "w_close_1h_14", 13);
    assert_named_window(&q.sql, "w_close_1h_31", 30);
    assert_unpivot(&q.sql, "rsi_14", 0);
    assert_unpivot(&q.sql, "rsi_31", 1);
    assert_version_sql_hygiene(&q.sql);
}

#[test]
fn consumer_sql_ema_14_31() {
    let q = compile(&req(
        "{ ema_14: EMA([close.1h; $from:$to], 14), ema_31: EMA([close.1h; $from:$to], 31) }",
    ))
    .unwrap();
    assert_eq!(q.indicators, vec!["ema_14".to_string(), "ema_31".to_string()]);
    assert_eq!(q.max_lookback, 31);
    assert!(q.scaffolds.closes_to_date);

    assert_contains(&q.sql, "MAX(e.version) OVER w_close_1h_14 AS ver_0");
    assert_contains(&q.sql, "MAX(e.version) OVER w_close_1h_31 AS ver_1");
    assert_contains(&q.sql, "(array_length(e.closes_to_date, 1) >= 14) AS w_0");
    assert_contains(&q.sql, "(array_length(e.closes_to_date, 1) >= 31) AS w_1");
    assert_contains(&q.sql, "array_length(e.closes_to_date, 1) < 14");
    assert_contains(&q.sql, "array_length(e.closes_to_date, 1) < 31");
    assert_contains(&q.sql, "v.c * (2.0/(14+1.0)) + r.ema * (1.0 - (2.0/(14+1.0)))");
    assert_contains(&q.sql, "v.c * (2.0/(31+1.0)) + r.ema * (1.0 - (2.0/(31+1.0)))");
    assert_named_window(&q.sql, "w_close_1h_14", 13);
    assert_named_window(&q.sql, "w_close_1h_31", 30);
    assert_unpivot(&q.sql, "ema_14", 0);
    assert_unpivot(&q.sql, "ema_31", 1);
    assert_version_sql_hygiene(&q.sql);
}

#[test]
fn consumer_sql_vol_14_31() {
    let mut r = req(
        "{ vol_14: STD(RET([close.1h; $from:$to]), 14) * SQRT($bars_per_year), \
           vol_31: STD(RET([close.1h; $from:$to]), 31) * SQRT($bars_per_year) }",
    );
    r.params
        .insert("bars_per_year".into(), ParamValue::Int(8760));
    let q = compile(&r).unwrap();
    assert_eq!(q.indicators, vec!["vol_14".to_string(), "vol_31".to_string()]);
    assert_eq!(q.max_lookback, 31);
    assert!(q.scaffolds.bar_ret);

    // Plain MAX like v1 — constant SQRT scale must not introduce MIN_VALUE coalesce.
    assert_contains(
        &q.sql,
        "(SQRT(GREATEST(0, (AVG(e.bar_ret * e.bar_ret) OVER w_ret_1h_14 - POWER(AVG(e.bar_ret) OVER w_ret_1h_14, 2)))) * SQRT(8760)) AS v_0",
    );
    assert_contains(&q.sql, "MAX(e.version) OVER w_ret_1h_14 AS ver_0");
    assert_contains(&q.sql, "((COUNT(*) OVER w_ret_1h_14) >= 14 AND TRUE) AS w_0");
    assert_contains(
        &q.sql,
        "(SQRT(GREATEST(0, (AVG(e.bar_ret * e.bar_ret) OVER w_ret_1h_31 - POWER(AVG(e.bar_ret) OVER w_ret_1h_31, 2)))) * SQRT(8760)) AS v_1",
    );
    assert_contains(&q.sql, "MAX(e.version) OVER w_ret_1h_31 AS ver_1");
    assert_contains(&q.sql, "((COUNT(*) OVER w_ret_1h_31) >= 31 AND TRUE) AS w_1");
    assert_named_window(&q.sql, "w_ret_1h_14", 13);
    assert_named_window(&q.sql, "w_ret_1h_31", 30);
    assert_unpivot(&q.sql, "vol_14", 0);
    assert_unpivot(&q.sql, "vol_31", 1);
    assert_version_sql_hygiene(&q.sql);
}

#[test]
fn consumer_sql_sep_atr() {
    let mut r = req(
        "{ sep_atr: (AVG([close.1h; $from:$to], $fast) - AVG([close.1h; $from:$to], $slow)) \
           / RMA(TR([close.1h; $from:$to]), $atr) }",
    );
    r.params.insert("atr".into(), ParamValue::Int(14));
    r.params.insert("fast".into(), ParamValue::Int(48));
    r.params.insert("slow".into(), ParamValue::Int(96));
    let q = compile(&r).unwrap();
    assert_eq!(q.indicators, vec!["sep_atr".to_string()]);
    assert_eq!(q.max_lookback, 96);
    assert!(q.scaffolds.closes_to_date);

    assert_contains(
        &q.sql,
        "(AVG(e.close) OVER w_close_1h_48 - AVG(e.close) OVER w_close_1h_96)",
    );
    assert_contains(&q.sql, "WHERE t.ord BETWEEN 2 AND 15");
    // GREATEST of finite atr/fast/slow frames only (nested from binary ops).
    assert_contains(
        &q.sql,
        "GREATEST(GREATEST(MAX(e.version) OVER w_close_1h_48, MAX(e.version) OVER w_close_1h_96), MAX(e.version) OVER w_close_1h_14) AS ver_0",
    );
    assert_contains(
        &q.sql,
        "(((COUNT(*) OVER w_close_1h_48) >= 48 AND (COUNT(*) OVER w_close_1h_96) >= 96) AND (array_length(e.closes_to_date, 1) >= 15)) AS w_0",
    );
    assert_named_window(&q.sql, "w_close_1h_14", 13);
    assert_named_window(&q.sql, "w_close_1h_48", 47);
    assert_named_window(&q.sql, "w_close_1h_96", 95);
    assert_unpivot(&q.sql, "sep_atr", 0);
    assert_version_sql_hygiene(&q.sql);
}
