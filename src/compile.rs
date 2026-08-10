//! Compile analyzed expressions into the joint-analytics CTE SQL envelope.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::ast::{
    AssetRef, BatchExpr, BinOp, CallOp, Expr, LookbackBound, Series, TrailingPeriod, WindowSpec,
};
use crate::error::Error;
use crate::interval::interval_ms;
use crate::parse::parse_batch;
use crate::sem::{analyze_obj, Analysis, Domain, ParamValue};

/// Request inputs for compilation (mirrors the read-api request minus HTTP concerns).
#[derive(Debug, Clone)]
pub struct CompileRequest {
    /// Grammar source: a single expr or a `{ name: expr, ... }` batch.
    pub expr: String,
    /// Optional echo/check of bucket. Grammar owns the period via `[close.1d]`;
    /// if set here it must match every series bucket.
    pub reporting_period: Option<String>,
    pub assets: Vec<String>,
    pub params: BTreeMap<String, ParamValue>,
    /// Exclusive pagination cursor (`-1` = first page).
    pub after_ts: i64,
    /// Max distinct `timestamp_start` ranks.
    pub limit: i32,
    pub publish_from: Option<i64>,
    /// Series stems whose fact table is `obj` (object/candle values), as
    /// resolved from `series_slot` by the caller. A referenced series whose
    /// stem is in this set compiles against `obj` (raw object fetch, or a
    /// scalar `->field` projection) instead of the default scalar `data`
    /// path. Empty = every series is scalar (backwards-compatible default).
    pub obj_data_types: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BindValue {
    TextArray(Vec<String>),
    BigInt(i64),
    /// SQL NULL for optional domain bounds (full / trailing-latest modes).
    Null,
    Int(i32),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scaffolds {
    pub bar_ret: bool,
    pub closes_to_date: bool,
    pub highs_to_date: bool,
    pub lows_to_date: bool,
    pub market_tickers: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CompiledQuery {
    pub sql: String,
    /// Positional binds matching `?` placeholders in order.
    pub binds: Vec<BindValue>,
    pub reporting_period: String,
    /// Unaggregated source label (`binance` in `[binance:close.1d]`), or `None`
    /// for aggregated / canonical series.
    pub source: Option<String>,
    /// Datastore `params_hash` used in the SQL scan filters.
    pub params_hash: String,
    pub expr: String,
    pub domain: Domain,
    pub indicators: Vec<String>,
    pub max_lookback: i32,
    pub scaffolds: Scaffolds,
    pub interval_ms: i64,
    pub analysis: Analysis,
}

pub fn compile(req: &CompileRequest) -> Result<CompiledQuery, Error> {
    let batch = parse_batch(&req.expr)?;
    compile_batch(req, &batch)
}

pub fn compile_expr(req: &CompileRequest, expr: &Expr) -> Result<CompiledQuery, Error> {
    compile_batch(req, &BatchExpr::Single(expr.clone()))
}

pub fn compile_batch(req: &CompileRequest, batch: &BatchExpr) -> Result<CompiledQuery, Error> {
    if req.assets.is_empty() {
        return Err(Error::compile("assets must be non-empty", &req.expr));
    }
    if req.limit < 1 {
        return Err(Error::compile("limit must be >= 1", &req.expr));
    }

    let analysis = analyze_obj(batch, &req.params, &req.expr, &req.obj_data_types)?;

    if let Some(req_period) = &req.reporting_period {
        if req_period != &analysis.reporting_period {
            return Err(Error::compile(
                format!(
                    "request reporting_period `{req_period}` conflicts with grammar bucket `{}`",
                    analysis.reporting_period
                ),
                &req.expr,
            ));
        }
    }

    let reporting_period = analysis.reporting_period.clone();
    let interval = interval_ms(&reporting_period).map_err(|e| {
        Error::compile(format!("{e}"), &req.expr)
    })?;

    // Object (`obj`) series path. A referenced stem is an object series when
    // the caller resolved it as `obj` in `series_slot`. When every series
    // is an object series, compile the raw-fetch / `->field` envelope against
    // `obj`; mixing object and scalar series in one query is not yet allowed.
    let obj_stem_count = analysis
        .series_names
        .iter()
        .filter(|n| req.obj_data_types.contains(*n))
        .count();
    if obj_stem_count > 0 {
        if obj_stem_count != analysis.series_names.len() {
            return Err(Error::compile(
                "cannot mix object and scalar series in one query",
                &req.expr,
            ));
        }
        return compile_obj_batch(req, batch, analysis, reporting_period, interval);
    }

    // `->field` projects a JSON key and is only valid on an object series.
    let mut series_refs: Vec<&Series> = Vec::new();
    match batch {
        BatchExpr::Single(e) => collect_series(e, &mut series_refs),
        BatchExpr::Batch(m) => m.values().for_each(|e| collect_series(e, &mut series_refs)),
    }
    if let Some(s) = series_refs.iter().find(|s| s.field.is_some()) {
        return Err(Error::compile(
            format!(
                "`->{}` field accessor requires an object series; `{}` is a scalar series",
                s.field.as_deref().unwrap_or(""),
                s.name
            ),
            &req.expr,
        ));
    }

    let indicators: Vec<String> = match batch {
        BatchExpr::Single(_) => vec!["value".to_string()],
        BatchExpr::Batch(m) => m.keys().cloned().collect(),
    };

    let mut named_windows: BTreeMap<String, i32> = BTreeMap::new();
    let mut value_cols: Vec<(String, String, String, String)> = Vec::new();
    // (stem, value_sql, version_sql, warmup_sql)

    let mut cg = Codegen {
        params: &req.params,
        analysis: &analysis,
        reporting_period: &reporting_period,
        named_windows: &mut named_windows,
        expr_src: &req.expr,
    };

    match batch {
        BatchExpr::Single(e) => {
            let frag = cg.gen_expr(e)?;
            value_cols.push((
                "value".into(),
                frag.value_sql,
                frag.version_sql,
                frag.warmup_sql,
            ));
        }
        BatchExpr::Batch(map) => {
            for (name, e) in map {
                let frag = cg.gen_expr(e)?;
                value_cols.push((
                    name.clone(),
                    frag.value_sql,
                    frag.version_sql,
                    frag.warmup_sql,
                ));
            }
        }
    }

    let scaffolds = Scaffolds {
        bar_ret: analysis.needs_bar_ret,
        closes_to_date: analysis.needs_closes_to_date,
        highs_to_date: analysis.needs_highs_to_date,
        lows_to_date: analysis.needs_lows_to_date,
        market_tickers: analysis.market_tickers.iter().cloned().collect(),
    };

    let sql = render_envelope(
        &reporting_period,
        &analysis.params_hash,
        interval,
        &scaffolds,
        &named_windows,
        &value_cols,
        &analysis.domain,
    );

    let (dirty_from, dirty_to) = match &analysis.domain {
        Domain::Absolute { from_ms, to_ms, .. } => {
            (BindValue::BigInt(*from_ms), BindValue::BigInt(*to_ms))
        }
        Domain::Full | Domain::TrailingLatest { .. } | Domain::FromStart { .. } => {
            (BindValue::Null, BindValue::Null)
        }
    };

    let binds = vec![
        BindValue::TextArray(req.assets.clone()),
        dirty_from,
        dirty_to,
        BindValue::BigInt(req.after_ts),
        BindValue::Int(req.limit),
        BindValue::BigInt(req.publish_from.unwrap_or(i64::MIN)),
        BindValue::Int(analysis.max_lookback),
        BindValue::TextArray(indicators.clone()),
    ];

    Ok(CompiledQuery {
        sql,
        binds,
        reporting_period,
        source: analysis.source.clone(),
        params_hash: analysis.params_hash.clone(),
        expr: req.expr.clone(),
        domain: analysis.domain.clone(),
        indicators,
        max_lookback: analysis.max_lookback,
        scaffolds,
        interval_ms: interval,
        analysis,
    })
}

#[derive(Clone)]
struct Frag {
    value_sql: String,
    version_sql: String,
    warmup_sql: String,
    /// Optional trailing window period (bars) used by windowed aggregates.
    period: Option<i32>,
    /// Series data_type for window naming, when applicable.
    series_key: Option<String>,
}

struct Codegen<'a> {
    params: &'a BTreeMap<String, ParamValue>,
    analysis: &'a Analysis,
    reporting_period: &'a str,
    named_windows: &'a mut BTreeMap<String, i32>,
    expr_src: &'a str,
}

impl Codegen<'_> {
    fn gen_expr(&mut self, expr: &Expr) -> Result<Frag, Error> {
        match expr {
            // Result index/slice only restricts emit domain (handled in analyze).
            Expr::Index { base, .. } => self.gen_expr(base),
            Expr::Series(s) => self.gen_series(s),
            Expr::Literal { value, is_int, .. } => {
                let sql = if *is_int && value.fract() == 0.0 {
                    format!("{}", *value as i64)
                } else {
                    format!("{value}")
                };
                Ok(Frag {
                    value_sql: sql,
                    version_sql: "NULL::bigint".into(),
                    warmup_sql: "TRUE".into(),
                    period: None,
                    series_key: None,
                })
            }
            Expr::Param { name, pos } => {
                let v = self.params.get(name).ok_or_else(|| {
                    Error::compile(format!("missing param `${name}`"), self.expr_src)
                })?;
                let sql = match v {
                    ParamValue::Int(i) => i.to_string(),
                    ParamValue::Float(f) => f.to_string(),
                    ParamValue::Text(_) => {
                        return Err(Error::sem(
                            format!("text param `${name}` cannot be used as a numeric expression"),
                            self.expr_src,
                            Some(*pos),
                        ));
                    }
                };
                Ok(Frag {
                    value_sql: sql,
                    version_sql: "NULL::bigint".into(),
                    warmup_sql: "TRUE".into(),
                    period: None,
                    series_key: None,
                })
            }
            Expr::BinOp { op, left, right } => {
                let l = self.gen_expr(left)?;
                let r = self.gen_expr(right)?;
                Ok(Frag {
                    value_sql: format!(
                        "({} {} {})",
                        l.value_sql,
                        op.as_sql(),
                        r.value_sql
                    ),
                    version_sql: combine_versions(&[l.version_sql.clone(), r.version_sql.clone()]),
                    warmup_sql: format!("({} AND {})", l.warmup_sql, r.warmup_sql),
                    period: None,
                    series_key: None,
                })
            }
            Expr::Call {
                op,
                args,
                window,
                pos,
            } => self.gen_call(*op, args, window.as_ref(), *pos),
        }
    }

    fn gen_series(&mut self, s: &Series) -> Result<Frag, Error> {
        let col = match s.asset {
            AssetRef::Row | AssetRef::SelfRow => match s.name.as_str() {
                "close" => "e.close".to_string(),
                "open" => "e.open".to_string(),
                "high" => "e.high".to_string(),
                "low" => "e.low".to_string(),
                "volume" => "e.volume".to_string(),
                other => {
                    return Err(Error::compile(
                        format!("unsupported series [{other}]"),
                        self.expr_src,
                    ))
                }
            },
            AssetRef::Literal(_) | AssetRef::Param(_) => {
                let ticker = match &s.asset {
                    AssetRef::Literal(t) => t.clone(),
                    AssetRef::Param(p) => self
                        .params
                        .get(p)
                        .and_then(|v| v.as_text().map(str::to_string))
                        .ok_or_else(|| {
                            Error::compile(format!("missing ticker param `${p}`"), self.expr_src)
                        })?,
                    AssetRef::Row | AssetRef::SelfRow => unreachable!(),
                };
                let join = market_join_alias(&ticker, self.analysis.market_tickers.len());
                match s.name.as_str() {
                    "close" => format!("{join}.close"),
                    other => {
                        return Err(Error::compile(
                            format!("@{ticker} currently only supports [close], not [{other}]"),
                            self.expr_src,
                        ))
                    }
                }
            }
        };

        Ok(Frag {
            value_sql: col,
            version_sql: "e.version".into(),
            warmup_sql: "TRUE".into(),
            period: None,
            series_key: Some(s.name.clone()),
        })
    }

    fn gen_call(
        &mut self,
        op: CallOp,
        args: &[Expr],
        window: Option<&WindowSpec>,
        pos: usize,
    ) -> Result<Frag, Error> {
        match op {
            CallOp::Ret => {
                let value_sql = if is_row_close(&args[0]) {
                    "e.bar_ret".to_string()
                } else if let Some(ticker) = market_close_ticker(&args[0], self.params) {
                    let join = market_join_alias(&ticker, self.analysis.market_tickers.len());
                    format!("{join}.market_ret")
                } else {
                    let series = self.gen_expr(&args[0])?;
                    format!(
                        "(CASE WHEN LAG({v}) OVER (PARTITION BY e.coin, e.seg_key ORDER BY e.timestamp_start) IS NULL \
                         OR LAG({v}) OVER (PARTITION BY e.coin, e.seg_key ORDER BY e.timestamp_start) <= 0 \
                         OR {v} <= 0 THEN NULL \
                         WHEN ABS({v} / LAG({v}) OVER (PARTITION BY e.coin, e.seg_key ORDER BY e.timestamp_start) - 1.0) > 5 THEN NULL \
                         ELSE {v} / LAG({v}) OVER (PARTITION BY e.coin, e.seg_key ORDER BY e.timestamp_start) - 1.0 END)",
                        v = series.value_sql
                    )
                };
                Ok(Frag {
                    value_sql,
                    version_sql: "e.version".into(),
                    warmup_sql: "TRUE".into(),
                    period: None,
                    series_key: Some("ret".into()),
                })
            }
            CallOp::Tr => {
                // Close-to-close true range via to-date array (Wilder path).
                Ok(Frag {
                    value_sql: "ABS(e.close - LAG(e.close) OVER (PARTITION BY e.coin, e.seg_key ORDER BY e.timestamp_start))".into(),
                    version_sql: "e.version".into(),
                    warmup_sql: "(LAG(e.close) OVER (PARTITION BY e.coin, e.seg_key ORDER BY e.timestamp_start) IS NOT NULL)".into(),
                    period: None,
                    series_key: Some("close".into()),
                })
            }
            CallOp::Avg | CallOp::Count | CallOp::Var | CallOp::Std => {
                let inner = self.gen_expr(&args[0])?;
                let period = self.resolve_period(window, pos)?;
                let series_key = inner.series_key.as_deref().unwrap_or("expr");
                let win = self.ensure_trailing_window(series_key, period);
                let (value_sql, warmup_sql) = match op {
                    CallOp::Avg => (
                        format!("AVG({}) OVER {}", inner.value_sql, win),
                        format!("(COUNT(*) OVER {win}) >= {period}"),
                    ),
                    CallOp::Count => (
                        format!("COUNT(*) OVER {win}"),
                        format!("(COUNT(*) OVER {win}) >= {period}"),
                    ),
                    CallOp::Var => (
                        format!(
                            "(AVG({v} * {v}) OVER {win} - POWER(AVG({v}) OVER {win}, 2))",
                            v = inner.value_sql,
                            win = win
                        ),
                        format!("(COUNT(*) OVER {win}) >= {period}"),
                    ),
                    CallOp::Std => (
                        format!(
                            "SQRT(GREATEST(0, (AVG({v} * {v}) OVER {win} - POWER(AVG({v}) OVER {win}, 2))))",
                            v = inner.value_sql,
                            win = win
                        ),
                        format!("(COUNT(*) OVER {win}) >= {period}"),
                    ),
                    _ => unreachable!(),
                };
                Ok(Frag {
                    value_sql,
                    version_sql: format!("MAX(e.version) OVER {win}"),
                    warmup_sql,
                    period: Some(period),
                    series_key: inner.series_key,
                })
            }
            CallOp::Ema => {
                let _inner = self.gen_expr(&args[0])?;
                let period = self.resolve_period(window, pos)?;
                // Value uses inception closes_to_date; version matches SMA's finite period frame.
                let win = self.ensure_trailing_window("close", period);
                let value_sql = ema_sql("e.closes_to_date", period);
                Ok(Frag {
                    value_sql,
                    version_sql: format!("MAX(e.version) OVER {win}"),
                    warmup_sql: format!("(array_length(e.closes_to_date, 1) >= {period})"),
                    period: Some(period),
                    series_key: Some("close".into()),
                })
            }
            CallOp::Rma => {
                let period = self.resolve_period(window, pos)?;
                // RMA(TR(close)) → Wilder ATR over closes_to_date (analytics atrWilderSql).
                if !is_tr_of_close(&args[0]) {
                    let _ = self.gen_expr(&args[0])?;
                    return Err(Error::compile(
                        "RMA currently supports RMA(TR([close; …]), $period) only",
                        self.expr_src,
                    ));
                }
                let _ = self.gen_expr(&args[0])?;
                // Value uses inception closes_to_date; version matches SMA's finite period frame.
                let win = self.ensure_trailing_window("close", period);
                Ok(Frag {
                    value_sql: rma_tr_sql("e.closes_to_date", period),
                    version_sql: format!("MAX(e.version) OVER {win}"),
                    warmup_sql: format!("(array_length(e.closes_to_date, 1) >= {})", period + 1),
                    period: Some(period),
                    series_key: Some("close".into()),
                })
            }
            CallOp::Rsi => {
                let _inner = self.gen_expr(&args[0])?;
                let period = self.resolve_period(window, pos)?;
                // Value uses inception closes_to_date; version matches SMA's finite period frame.
                let win = self.ensure_trailing_window("close", period);
                Ok(Frag {
                    value_sql: rsi_sql("e.closes_to_date", period),
                    version_sql: format!("MAX(e.version) OVER {win}"),
                    warmup_sql: format!("(array_length(e.closes_to_date, 1) >= {})", period + 1),
                    period: Some(period),
                    series_key: Some("close".into()),
                })
            }
            CallOp::RegrSlope => {
                let y = self.gen_expr(&args[0])?;
                let x = self.gen_expr(&args[1])?;
                let period = self.resolve_period(window, pos)?;
                let win = self.ensure_trailing_window("regr", period);
                Ok(Frag {
                    value_sql: format!(
                        "REGR_SLOPE({}, {}) OVER {}",
                        y.value_sql, x.value_sql, win
                    ),
                    version_sql: format!("MAX(e.version) OVER {win}"),
                    warmup_sql: format!("(COUNT(*) OVER {win}) >= {period}"),
                    period: Some(period),
                    series_key: None,
                })
            }
            CallOp::Sqrt => {
                let a = self.gen_expr(&args[0])?;
                Ok(Frag {
                    value_sql: format!("SQRT({})", a.value_sql),
                    version_sql: a.version_sql,
                    warmup_sql: a.warmup_sql,
                    period: a.period,
                    series_key: a.series_key,
                })
            }
            CallOp::Abs => {
                let a = self.gen_expr(&args[0])?;
                Ok(Frag {
                    value_sql: format!("ABS({})", a.value_sql),
                    version_sql: a.version_sql,
                    warmup_sql: a.warmup_sql,
                    period: a.period,
                    series_key: a.series_key,
                })
            }
            // Unary trigonometric / hyperbolic transforms. SQL function name
            // matches the builtin spelling (`op.as_str()`); each is a pure
            // scalar map that inherits its argument's version / warmup / period.
            CallOp::Sin
            | CallOp::Cos
            | CallOp::Tan
            | CallOp::Asin
            | CallOp::Acos
            | CallOp::Atan
            | CallOp::Sinh
            | CallOp::Cosh
            | CallOp::Tanh
            | CallOp::Asinh
            | CallOp::Acosh
            | CallOp::Atanh => {
                let a = self.gen_expr(&args[0])?;
                Ok(Frag {
                    value_sql: format!("{}({})", op.as_str(), a.value_sql),
                    version_sql: a.version_sql,
                    warmup_sql: a.warmup_sql,
                    period: a.period,
                    series_key: a.series_key,
                })
            }
            CallOp::Power => {
                let a = self.gen_expr(&args[0])?;
                let b = self.gen_expr(&args[1])?;
                Ok(Frag {
                    value_sql: format!("POWER({}, {})", a.value_sql, b.value_sql),
                    version_sql: combine_versions(&[a.version_sql.clone(), b.version_sql.clone()]),
                    warmup_sql: format!("({} AND {})", a.warmup_sql, b.warmup_sql),
                    period: None,
                    series_key: None,
                })
            }
            CallOp::Greatest => {
                let frags: Vec<Frag> = args
                    .iter()
                    .map(|a| self.gen_expr(a))
                    .collect::<Result<_, _>>()?;
                let vals: Vec<_> = frags.iter().map(|f| f.value_sql.clone()).collect();
                let vers: Vec<_> = frags.iter().map(|f| f.version_sql.clone()).collect();
                let warms: Vec<_> = frags.iter().map(|f| format!("({})", f.warmup_sql)).collect();
                Ok(Frag {
                    value_sql: format!("GREATEST({})", vals.join(", ")),
                    version_sql: combine_versions(&vers),
                    warmup_sql: warms.join(" AND "),
                    period: None,
                    series_key: None,
                })
            }
        }
    }

    fn resolve_period(&self, window: Option<&WindowSpec>, pos: usize) -> Result<i32, Error> {
        let window = window.ok_or_else(|| {
            Error::sem("missing window", self.expr_src, Some(pos))
        })?;
        match window {
            WindowSpec::Trailing {
                period: TrailingPeriod::Param { name, .. },
            } => Ok(self.params.get(name).and_then(|v| v.as_i64()).unwrap() as i32),
            WindowSpec::Trailing {
                period: TrailingPeriod::Int { value, .. },
            } => Ok(*value as i32),
            WindowSpec::Explicit {
                start: LookbackBound::TMinus(inner),
                end: LookbackBound::T,
            } => Ok(eval_const(inner, self.params)? as i32 + 1),
            WindowSpec::Explicit {
                start: LookbackBound::T0,
                end: LookbackBound::T,
            } => Ok(self.analysis.max_lookback),
            _ => Err(Error::sem(
                "unsupported lookback pair",
                self.expr_src,
                Some(pos),
            )),
        }
    }

    fn ensure_trailing_window(&mut self, series_key: &str, period: i32) -> String {
        let name = format!(
            "w_{}_{}_{}",
            sanitize_ident(series_key),
            sanitize_ident(self.reporting_period),
            period
        );
        self.named_windows.insert(name.clone(), period);
        name
    }
}

/// Merge version expressions for multi-operand forms.
///
/// Compile-time `NULL::bigint` (literals / numeric params) is dropped so
/// `STD(...) * SQRT($bars_per_year)` emits a plain `MAX(version) OVER w_…`
/// like v1, instead of `GREATEST(COALESCE(…, Long.MIN_VALUE::bigint), …)`.
/// Bare `-9223372036854775808::bigint` is rejected by Postgres at parse time.
fn combine_versions(versions: &[String]) -> String {
    let non_null: Vec<&str> = versions
        .iter()
        .map(String::as_str)
        .filter(|v| *v != "NULL::bigint")
        .collect();
    match non_null.as_slice() {
        [] => "NULL::bigint".into(),
        [only] => (*only).to_string(),
        many => format!("GREATEST({})", many.join(", ")),
    }
}

fn sanitize_ident(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn is_row_close(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Series(Series {
            name,
            asset: AssetRef::Row | AssetRef::SelfRow,
            ..
        }) if name == "close"
    )
}

fn market_close_ticker(expr: &Expr, params: &BTreeMap<String, ParamValue>) -> Option<String> {
    match expr {
        Expr::Series(Series {
            name,
            asset: AssetRef::Literal(t),
            ..
        }) if name == "close" => Some(t.clone()),
        Expr::Series(Series {
            name,
            asset: AssetRef::Param(p),
            ..
        }) if name == "close" => params.get(p).and_then(|v| v.as_text().map(str::to_string)),
        _ => None,
    }
}

fn is_tr_of_close(expr: &Expr) -> bool {
    match expr {
        Expr::Call {
            op: CallOp::Tr,
            args,
            ..
        } => args.len() == 1 && is_row_close(&args[0]),
        _ => false,
    }
}

fn eval_const(expr: &Expr, params: &BTreeMap<String, ParamValue>) -> Result<i64, Error> {
    match expr {
        Expr::Literal { value, .. } => Ok(*value as i64),
        Expr::Param { name, .. } => Ok(params.get(name).and_then(|v| v.as_i64()).unwrap_or(0)),
        Expr::BinOp { op, left, right } => {
            let l = eval_const(left, params)?;
            let r = eval_const(right, params)?;
            Ok(match op {
                BinOp::Add => l + r,
                BinOp::Sub => l - r,
                BinOp::Mul => l * r,
                BinOp::Div => l / r,
            })
        }
        _ => Err(Error::compile("non-constant lookback", "")),
    }
}

/// Join alias for a market ticker. Single-ticker exprs use `m` (spec `market_ret`).
fn market_join_alias(ticker: &str, market_count: usize) -> String {
    if market_count <= 1 {
        "m".to_string()
    } else {
        format!("m_{}", sanitize_ident(ticker))
    }
}

fn market_cte_name(ticker: &str, market_count: usize) -> String {
    if market_count <= 1 {
        "market_ret".to_string()
    } else {
        format!("market_ret_{}", sanitize_ident(ticker))
    }
}

/// Inception-SMA-seeded EMA over `C` (`emaCloseSql`).
fn ema_sql(closes_arr: &str, period: i32) -> String {
    format!(
        "(SELECT CASE WHEN array_length({arr}, 1) < {period} THEN NULL ELSE (\n\
           WITH RECURSIVE vals AS (\n\
             SELECT u.ord, u.c FROM unnest({arr}) WITH ORDINALITY AS u(c, ord)\n\
           ),\n\
           seed AS (SELECT AVG(v.c) AS ema FROM vals v WHERE v.ord <= {period}),\n\
           rec AS (\n\
             SELECT {period}::bigint AS ord, s.ema FROM seed s\n\
             UNION ALL\n\
             SELECT v.ord, v.c * (2.0/({period}+1.0)) + r.ema * (1.0 - (2.0/({period}+1.0)))\n\
             FROM rec r JOIN vals v ON v.ord = r.ord + 1\n\
           )\n\
           SELECT r.ema FROM rec r ORDER BY r.ord DESC LIMIT 1\n\
         ) END)",
        arr = closes_arr,
        period = period
    )
}

/// Wilder ATR: close-to-close TR over `C`, seed AVG(tr) for bars `2..period+1`.
fn rma_tr_sql(closes_arr: &str, period: i32) -> String {
    let seed_ord = period + 1;
    format!(
        "(SELECT CASE WHEN array_length({arr}, 1) < {need} THEN NULL ELSE (\n\
           WITH RECURSIVE vals AS (\n\
             SELECT u.ord, u.c::double precision AS c\n\
             FROM unnest({arr}) WITH ORDINALITY AS u(c, ord)\n\
           ),\n\
           tr AS (\n\
             SELECT v.ord, ABS(v.c - p.c) AS tr\n\
             FROM vals v JOIN vals p ON p.ord = v.ord - 1\n\
             WHERE v.ord >= 2\n\
           ),\n\
           seed AS (\n\
             SELECT AVG(t.tr) AS atr\n\
             FROM tr t WHERE t.ord BETWEEN 2 AND {seed_ord}\n\
           ),\n\
           rec AS (\n\
             SELECT {seed_ord}::bigint AS ord, s.atr FROM seed s\n\
             UNION ALL\n\
             SELECT t.ord, (r.atr * ({period} - 1) + t.tr) / {period}\n\
             FROM rec r JOIN tr t ON t.ord = r.ord + 1\n\
           )\n\
           SELECT r.atr FROM rec r ORDER BY r.ord DESC LIMIT 1\n\
         ) END)",
        arr = closes_arr,
        period = period,
        need = period + 1,
        seed_ord = seed_ord
    )
}

/// Wilder RSI over `C` (`rsiWilderSql`): `100 - 100/(1 + avg_gain/avg_loss)`.
fn rsi_sql(closes_arr: &str, period: i32) -> String {
    let seed_ord = period + 1;
    format!(
        "(SELECT CASE WHEN array_length({arr}, 1) < {need} THEN NULL ELSE (\n\
           WITH RECURSIVE vals AS (\n\
             SELECT u.ord, u.c::double precision AS c\n\
             FROM unnest({arr}) WITH ORDINALITY AS u(c, ord)\n\
           ),\n\
           ch AS (\n\
             SELECT v.ord,\n\
                    GREATEST(v.c - p.c, 0) AS gain,\n\
                    GREATEST(p.c - v.c, 0) AS loss\n\
             FROM vals v JOIN vals p ON p.ord = v.ord - 1\n\
             WHERE v.ord >= 2\n\
           ),\n\
           seed AS (\n\
             SELECT AVG(c.gain) AS avg_gain, AVG(c.loss) AS avg_loss\n\
             FROM ch c WHERE c.ord BETWEEN 2 AND {seed_ord}\n\
           ),\n\
           rec AS (\n\
             SELECT {seed_ord}::bigint AS ord, s.avg_gain, s.avg_loss FROM seed s\n\
             UNION ALL\n\
             SELECT c.ord,\n\
                    (r.avg_gain * ({period} - 1) + c.gain) / {period},\n\
                    (r.avg_loss * ({period} - 1) + c.loss) / {period}\n\
             FROM rec r JOIN ch c ON c.ord = r.ord + 1\n\
           )\n\
           SELECT CASE\n\
                    WHEN r.avg_loss = 0 THEN 100.0\n\
                    ELSE 100.0 - (100.0 / (1.0 + (r.avg_gain / r.avg_loss)))\n\
                  END\n\
           FROM rec r ORDER BY r.ord DESC LIMIT 1\n\
         ) END)",
        arr = closes_arr,
        period = period,
        need = period + 1,
        seed_ord = seed_ord
    )
}

/// Bounds CTE: absolute uses dirty_* binds; other domains resolve from available data.
fn render_bounds_cte(
    domain: &Domain,
    reporting_period: &str,
    params_hash: &str,
    interval_ms: i64,
) -> String {
    let scan = format!(
        "CROSS JOIN LATERAL (\n\
         \x20   SELECT MIN(c.timestamp_start) AS min_ts,\n\
         \x20          MAX(c.timestamp_start) AS max_ts\n\
         \x20   FROM data c\n\
         \x20   JOIN mcdx_asset a ON a.id = c.asset\n\
         \x20   WHERE c.data_type = 'close'\n\
         \x20     AND c.reporting_period = '{reporting_period}'\n\
         \x20     AND c.params_hash = '{params_hash}'\n\
         \x20     AND a.canonical_ticker = ANY(p.coins)\n\
         \x20 ) l"
    );

    match domain {
        Domain::Absolute { .. } => "bounds AS (\n\
             \x20 SELECT\n\
             \x20   p.dirty_from AS emit_from,\n\
             \x20   p.dirty_to AS emit_to\n\
             \x20 FROM params p\n\
             ),"
            .to_string(),
        Domain::Full => format!(
            "bounds AS (\n\
             \x20 SELECT\n\
             \x20   l.min_ts AS emit_from,\n\
             \x20   l.max_ts AS emit_to\n\
             \x20 FROM params p\n\
             \x20 {scan}\n\
             ),"
        ),
        Domain::TrailingLatest { bars, end_offset } => {
            if *bars == i32::MAX {
                // `[:-k]` on full series: from min through max - end_offset
                let end_shift = *end_offset as i64 * interval_ms;
                format!(
                    "bounds AS (\n\
                     \x20 SELECT\n\
                     \x20   l.min_ts AS emit_from,\n\
                     \x20   l.max_ts - {end_shift} AS emit_to\n\
                     \x20 FROM params p\n\
                     \x20 {scan}\n\
                     ),"
                )
            } else {
                let end_shift = *end_offset as i64 * interval_ms;
                let span = (*bars as i64 - 1) * interval_ms;
                format!(
                    "bounds AS (\n\
                     \x20 SELECT\n\
                     \x20   (l.max_ts - {end_shift}) - {span} AS emit_from,\n\
                     \x20   l.max_ts - {end_shift} AS emit_to\n\
                     \x20 FROM params p\n\
                     \x20 {scan}\n\
                     ),"
                )
            }
        }
        Domain::FromStart { start, count } => {
            // First warmup-complete result ≈ min_ts + (max_lookback-1)*interval.
            // Result index `start` is `(start-1)` bars after that.
            let start_shift = format!(
                "l.min_ts + ((p.max_lookback::bigint + {}::bigint - 2) * {interval_ms})",
                start
            );
            if *count == i32::MAX {
                format!(
                    "bounds AS (\n\
                     \x20 SELECT\n\
                     \x20   {start_shift} AS emit_from,\n\
                     \x20   l.max_ts AS emit_to\n\
                     \x20 FROM params p\n\
                     \x20 {scan}\n\
                     ),"
                )
            } else {
                let span = (*count as i64 - 1) * interval_ms;
                format!(
                    "bounds AS (\n\
                     \x20 SELECT\n\
                     \x20   {start_shift} AS emit_from,\n\
                     \x20   LEAST(l.max_ts, ({start_shift}) + {span}) AS emit_to\n\
                     \x20 FROM params p\n\
                     \x20 {scan}\n\
                     ),"
                )
            }
        }
    }
}

fn render_envelope(
    reporting_period: &str,
    params_hash: &str,
    interval_ms: i64,
    scaffolds: &Scaffolds,
    named_windows: &BTreeMap<String, i32>,
    value_cols: &[(String, String, String, String)],
    domain: &Domain,
) -> String {
    let mut sql = String::new();

    writeln!(
        sql,
        "WITH params AS (\n\
         \x20 SELECT\n\
         \x20   ?::text[]  AS coins,\n\
         \x20   ?::bigint  AS dirty_from,\n\
         \x20   ?::bigint  AS dirty_to,\n\
         \x20   ?::bigint  AS after_ts,\n\
         \x20   ?::int     AS lim,\n\
         \x20   ?::bigint  AS publish_from,\n\
         \x20   ?::int     AS max_lookback,\n\
         \x20   ?::text[]  AS indicators\n\
         ),"
    )
    .unwrap();

    writeln!(
        sql,
        "{}",
        render_bounds_cte(domain, reporting_period, params_hash, interval_ms)
    )
    .unwrap();

    // ordered — scan pads lookback before emit_from
    writeln!(
        sql,
        "ordered AS (\n\
         \x20 SELECT a.canonical_ticker AS coin,\n\
         \x20        c.timestamp_start,\n\
         \x20        c.timestamp_start + {interval_ms} AS timestamp_end,\n\
         \x20        c.value AS close,\n\
         \x20        c.version,\n\
         \x20        c.timestamp_start\n\
         \x20          - ((ROW_NUMBER() OVER (\n\
         \x20                 PARTITION BY c.asset ORDER BY c.timestamp_start) - 1)\n\
         \x20             * {interval_ms}) AS seg_key\n\
         \x20 FROM data c\n\
         \x20 JOIN mcdx_asset a ON a.id = c.asset\n\
         \x20 CROSS JOIN params p\n\
         \x20 CROSS JOIN bounds b\n\
         \x20 WHERE c.data_type = 'close'\n\
         \x20   AND c.reporting_period = '{reporting_period}'\n\
         \x20   AND c.params_hash = '{params_hash}'\n\
         \x20   AND a.canonical_ticker = ANY(p.coins)\n\
         \x20   AND c.timestamp_start >= b.emit_from - (p.max_lookback::bigint * {interval_ms})\n\
         \x20   AND c.timestamp_start <= b.emit_to\n\
         ),"
    )
    .unwrap();

    // enriched
    write!(sql, "enriched AS (\n  SELECT o.coin, o.timestamp_start, o.timestamp_end, o.close, o.version, o.seg_key").unwrap();
    if scaffolds.bar_ret {
        write!(
            sql,
            ",\n         CASE WHEN LAG(o.close) OVER (PARTITION BY o.coin, o.seg_key ORDER BY o.timestamp_start) IS NULL\n\
             \x20               OR LAG(o.close) OVER (PARTITION BY o.coin, o.seg_key ORDER BY o.timestamp_start) <= 0\n\
             \x20               OR o.close <= 0 THEN NULL\n\
             \x20               WHEN ABS(o.close / LAG(o.close) OVER (PARTITION BY o.coin, o.seg_key ORDER BY o.timestamp_start) - 1.0) > 5 THEN NULL\n\
             \x20               ELSE o.close / LAG(o.close) OVER (PARTITION BY o.coin, o.seg_key ORDER BY o.timestamp_start) - 1.0\n\
             \x20          END AS bar_ret"
        )
        .unwrap();
    }
    if scaffolds.closes_to_date {
        write!(
            sql,
            ",\n         array_agg(o.close) OVER (\n\
             \x20          PARTITION BY o.coin, o.seg_key ORDER BY o.timestamp_start\n\
             \x20          ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW\n\
             \x20        ) AS closes_to_date"
        )
        .unwrap();
    }
    if scaffolds.highs_to_date {
        write!(
            sql,
            ",\n         array_agg(o.high) OVER (\n\
             \x20          PARTITION BY o.coin, o.seg_key ORDER BY o.timestamp_start\n\
             \x20          ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW\n\
             \x20        ) AS highs_to_date"
        )
        .unwrap();
    }
    if scaffolds.lows_to_date {
        write!(
            sql,
            ",\n         array_agg(o.low) OVER (\n\
             \x20          PARTITION BY o.coin, o.seg_key ORDER BY o.timestamp_start\n\
             \x20          ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW\n\
             \x20        ) AS lows_to_date"
        )
        .unwrap();
    }
    writeln!(sql, "\n  FROM ordered o\n),").unwrap();

    // optional market CTEs (§4.3) — single ticker uses `market_ret` / `m.market_ret`
    let market_count = scaffolds.market_tickers.len();
    for ticker in &scaffolds.market_tickers {
        let cte = market_cte_name(ticker, market_count);
        writeln!(
            sql,
            "{cte} AS (\n\
             \x20 SELECT r.timestamp_start, r.close, r.bar_ret AS market_ret\n\
             \x20 FROM (\n\
             \x20   SELECT c.timestamp_start,\n\
             \x20          c.value AS close,\n\
             \x20          CASE WHEN LAG(c.value) OVER (\n\
             \x20                     PARTITION BY c.asset ORDER BY c.timestamp_start) IS NULL\n\
             \x20                 OR LAG(c.value) OVER (\n\
             \x20                     PARTITION BY c.asset ORDER BY c.timestamp_start) <= 0\n\
             \x20                 OR c.value <= 0\n\
             \x20               THEN NULL\n\
             \x20               ELSE c.value / LAG(c.value) OVER (\n\
             \x20                     PARTITION BY c.asset ORDER BY c.timestamp_start) - 1.0\n\
             \x20          END AS bar_ret\n\
             \x20   FROM data c\n\
             \x20   JOIN mcdx_asset a ON a.id = c.asset\n\
             \x20   CROSS JOIN params p\n\
             \x20   CROSS JOIN bounds b\n\
             \x20   WHERE c.data_type = 'close'\n\
             \x20     AND c.reporting_period = '{reporting_period}'\n\
             \x20     AND c.params_hash = '{params_hash}'\n\
             \x20     AND a.canonical_ticker = '{ticker}'\n\
             \x20     AND c.timestamp_start >= b.emit_from - (p.max_lookback::bigint * {interval_ms})\n\
             \x20     AND c.timestamp_start <= b.emit_to\n\
             \x20 ) r\n\
             \x20 WHERE r.bar_ret IS NOT NULL\n\
             ),"
        )
        .unwrap();
    }

    // windowed
    write!(sql, "windowed AS (\n  SELECT e.coin, e.timestamp_start, e.timestamp_end, e.version, e.seg_key").unwrap();
    for (i, (_stem, v, ver, w)) in value_cols.iter().enumerate() {
        write!(sql, ",\n         {v} AS v_{i},\n         {ver} AS ver_{i},\n         {w} AS w_{i}").unwrap();
    }
    write!(sql, "\n  FROM enriched e").unwrap();
    for ticker in &scaffolds.market_tickers {
        let cte = market_cte_name(ticker, market_count);
        let join = market_join_alias(ticker, market_count);
        write!(
            sql,
            "\n  LEFT JOIN {cte} {join} ON {join}.timestamp_start = e.timestamp_start"
        )
        .unwrap();
    }
    if !named_windows.is_empty() {
        writeln!(sql, "\n  WINDOW").unwrap();
        let mut first = true;
        for (name, period) in named_windows {
            if !first {
                writeln!(sql, ",").unwrap();
            }
            first = false;
            let preceding = period - 1;
            write!(
                sql,
                "    {name} AS (\n\
                 \x20     PARTITION BY e.coin, e.seg_key ORDER BY e.timestamp_start\n\
                 \x20     ROWS BETWEEN {preceding} PRECEDING AND CURRENT ROW\n\
                 \x20   )"
            )
            .unwrap();
        }
    }
    writeln!(sql, "\n),").unwrap();

    // unpivoted
    writeln!(sql, "unpivoted AS (").unwrap();
    writeln!(
        sql,
        "  SELECT w.coin, u.indicator, w.timestamp_start, w.timestamp_end,\n\
         \x20        u.value, u.version, u.warmup_complete\n\
         \x20 FROM windowed w\n\
         \x20 CROSS JOIN params p\n\
         \x20 CROSS JOIN LATERAL (VALUES"
    )
    .unwrap();
    for (i, (stem, _, _, _)) in value_cols.iter().enumerate() {
        let comma = if i + 1 == value_cols.len() { "" } else { "," };
        // Escape single quotes in stem
        let esc = stem.replace('\'', "''");
        writeln!(
            sql,
            "    ('{esc}', v_{i}, ver_{i}, w_{i}){comma}"
        )
        .unwrap();
    }
    writeln!(
        sql,
        "  ) AS u(indicator, value, version, warmup_complete)\n\
         \x20 CROSS JOIN bounds b\n\
         \x20 WHERE u.value IS NOT NULL\n\
         \x20   AND w.timestamp_start >= COALESCE(NULLIF(p.publish_from, -9223372036854775808), -9223372036854775808)\n\
         \x20   AND w.timestamp_start > COALESCE(p.after_ts, -9223372036854775808)\n\
         \x20   AND w.timestamp_start >= b.emit_from\n\
         \x20   AND w.timestamp_start <= b.emit_to\n\
         \x20   AND u.indicator = ANY(p.indicators)\n\
         ),"
    )
    .unwrap();

    writeln!(
        sql,
        "ranked AS (\n\
         \x20 SELECT u.*,\n\
         \x20        DENSE_RANK() OVER (ORDER BY u.timestamp_start) AS ts_rank\n\
         \x20 FROM unpivoted u\n\
         )\n\
         SELECT r.coin, r.indicator, r.timestamp_start, r.timestamp_end,\n\
         \x20      r.value, r.version, r.warmup_complete\n\
         FROM ranked r\n\
         CROSS JOIN params p\n\
         WHERE r.ts_rank <= p.lim\n\
         ORDER BY r.timestamp_start, r.coin, r.indicator;"
    )
    .unwrap();

    sql
}

/// Collect all `Series` references in an expression tree (for validation).
fn collect_series<'a>(expr: &'a Expr, out: &mut Vec<&'a Series>) {
    match expr {
        Expr::Series(s) => out.push(s),
        Expr::Call { args, .. } => args.iter().for_each(|a| collect_series(a, out)),
        Expr::BinOp { left, right, .. } => {
            collect_series(left, out);
            collect_series(right, out);
        }
        Expr::Index { base, .. } => collect_series(base, out),
        Expr::Param { .. } | Expr::Literal { .. } => {}
    }
}

/// Compile a homogeneous object-series request against `obj`.
///
/// Each output must be a bare object series (raw object fetch → `value` is the
/// JSON object as text) or a `->field` projection (→ `value` is one JSON key as
/// numeric text). Object series cannot yet be wrapped in operators. All members
/// must share the same stem. Uses the same 8 positional binds as the scalar path.
fn compile_obj_batch(
    req: &CompileRequest,
    batch: &BatchExpr,
    analysis: Analysis,
    reporting_period: String,
    interval_ms: i64,
) -> Result<CompiledQuery, Error> {
    let indicators: Vec<String> = match batch {
        BatchExpr::Single(_) => vec!["value".to_string()],
        BatchExpr::Batch(m) => m.keys().cloned().collect(),
    };

    let members: Vec<(String, &Expr)> = match batch {
        BatchExpr::Single(e) => vec![("value".to_string(), e)],
        BatchExpr::Batch(m) => m.iter().map(|(k, v)| (k.clone(), v)).collect(),
    };

    let mut cols: Vec<(String, String)> = Vec::new(); // (indicator, value_text_sql)
    let mut stems: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (name, e) in &members {
        match e {
            Expr::Series(s) => {
                if !req.obj_data_types.contains(&s.name) {
                    return Err(Error::compile(
                        format!("`{}` is not an object series", s.name),
                        &req.expr,
                    ));
                }
                let val = match &s.field {
                    None => "o.value::text".to_string(),
                    Some(f) => format!("(o.value->>'{}')", f.replace('\'', "''")),
                };
                stems.insert(s.name.clone());
                cols.push((name.clone(), val));
            }
            _ => {
                return Err(Error::compile(
                    "an object series cannot be wrapped in an operator yet; \
                     fetch the raw object or a `->field` scalar and compute downstream",
                    &req.expr,
                ))
            }
        }
    }
    if stems.len() != 1 {
        return Err(Error::compile(
            "all object series in one query must share the same stem",
            &req.expr,
        ));
    }
    let stem = stems.iter().next().unwrap();

    let sql = render_obj_envelope(
        &reporting_period,
        &analysis.params_hash,
        interval_ms,
        &analysis.domain,
        &cols,
        stem,
    );

    let (dirty_from, dirty_to) = match &analysis.domain {
        Domain::Absolute { from_ms, to_ms, .. } => {
            (BindValue::BigInt(*from_ms), BindValue::BigInt(*to_ms))
        }
        _ => (BindValue::Null, BindValue::Null),
    };
    let binds = vec![
        BindValue::TextArray(req.assets.clone()),
        dirty_from,
        dirty_to,
        BindValue::BigInt(req.after_ts),
        BindValue::Int(req.limit),
        BindValue::BigInt(req.publish_from.unwrap_or(i64::MIN)),
        BindValue::Int(analysis.max_lookback),
        BindValue::TextArray(indicators.clone()),
    ];

    Ok(CompiledQuery {
        sql,
        binds,
        reporting_period,
        source: analysis.source.clone(),
        params_hash: analysis.params_hash.clone(),
        expr: req.expr.clone(),
        domain: analysis.domain.clone(),
        indicators,
        max_lookback: analysis.max_lookback,
        scaffolds: Scaffolds::default(),
        interval_ms,
        analysis,
    })
}

/// Render the `obj` fetch envelope (same output columns + 8 binds as the
/// scalar envelope). `cols` are `(indicator_name, value_text_sql)`.
fn render_obj_envelope(
    reporting_period: &str,
    params_hash: &str,
    interval_ms: i64,
    domain: &Domain,
    cols: &[(String, String)],
    stem: &str,
) -> String {
    use std::fmt::Write;
    let stem_esc = stem.replace('\'', "''");
    let mut sql = String::new();

    write!(
        sql,
        "WITH params AS (\n\
         \x20 SELECT ?::text[] AS coins, ?::bigint AS dirty_from, ?::bigint AS dirty_to,\n\
         \x20        ?::bigint AS after_ts, ?::int AS lim, ?::bigint AS publish_from,\n\
         \x20        ?::int AS max_lookback, ?::text[] AS indicators\n\
         ),\n"
    )
    .unwrap();

    let scan = format!(
        "CROSS JOIN LATERAL (\n\
         \x20   SELECT MIN(o.timestamp_start) AS min_ts, MAX(o.timestamp_start) AS max_ts\n\
         \x20   FROM obj o JOIN mcdx_asset a ON a.id = o.asset\n\
         \x20   WHERE o.data_type = '{stem_esc}' AND o.reporting_period = '{reporting_period}'\n\
         \x20     AND o.params_hash = '{params_hash}' AND a.canonical_ticker = ANY(p.coins)\n\
         \x20 ) l"
    );
    match domain {
        Domain::Absolute { .. } => writeln!(
            sql,
            "bounds AS (SELECT p.dirty_from AS emit_from, p.dirty_to AS emit_to FROM params p),"
        )
        .unwrap(),
        Domain::Full => writeln!(
            sql,
            "bounds AS (SELECT l.min_ts AS emit_from, l.max_ts AS emit_to FROM params p {scan}),"
        )
        .unwrap(),
        Domain::TrailingLatest { bars, end_offset } => {
            let end_shift = *end_offset as i64 * interval_ms;
            if *bars == i32::MAX {
                writeln!(sql, "bounds AS (SELECT l.min_ts AS emit_from, l.max_ts - {end_shift} AS emit_to FROM params p {scan}),").unwrap();
            } else {
                let span = (*bars as i64 - 1) * interval_ms;
                writeln!(sql, "bounds AS (SELECT (l.max_ts - {end_shift}) - {span} AS emit_from, l.max_ts - {end_shift} AS emit_to FROM params p {scan}),").unwrap();
            }
        }
        Domain::FromStart { start, count } => {
            let start_shift = format!("l.min_ts + (({start} - 1)::bigint * {interval_ms})");
            if *count == i32::MAX {
                writeln!(sql, "bounds AS (SELECT {start_shift} AS emit_from, l.max_ts AS emit_to FROM params p {scan}),").unwrap();
            } else {
                let span = (*count as i64 - 1) * interval_ms;
                writeln!(sql, "bounds AS (SELECT {start_shift} AS emit_from, LEAST(l.max_ts, ({start_shift}) + {span}) AS emit_to FROM params p {scan}),").unwrap();
            }
        }
    }

    write!(
        sql,
        "src AS (\n\
         \x20 SELECT a.canonical_ticker AS coin, o.timestamp_start,\n\
         \x20        o.timestamp_start + {interval_ms} AS timestamp_end, o.version"
    )
    .unwrap();
    for (i, (_name, val)) in cols.iter().enumerate() {
        write!(sql, ",\n         {val} AS v_{i}").unwrap();
    }
    write!(
        sql,
        "\n  FROM obj o JOIN mcdx_asset a ON a.id = o.asset\n\
         \x20 CROSS JOIN params p CROSS JOIN bounds b\n\
         \x20 WHERE o.data_type = '{stem_esc}' AND o.reporting_period = '{reporting_period}'\n\
         \x20   AND o.params_hash = '{params_hash}' AND a.canonical_ticker = ANY(p.coins)\n\
         \x20   AND o.timestamp_start >= b.emit_from AND o.timestamp_start <= b.emit_to\n\
         ),\n"
    )
    .unwrap();

    write!(
        sql,
        "unpivoted AS (\n\
         \x20 SELECT s.coin, u.indicator, s.timestamp_start, s.timestamp_end, u.value, s.version, TRUE AS warmup_complete\n\
         \x20 FROM src s CROSS JOIN params p\n\
         \x20 CROSS JOIN LATERAL (VALUES"
    )
    .unwrap();
    for (i, (name, _val)) in cols.iter().enumerate() {
        let comma = if i + 1 == cols.len() { "" } else { "," };
        let esc = name.replace('\'', "''");
        write!(sql, "\n    ('{esc}', v_{i}){comma}").unwrap();
    }
    write!(
        sql,
        "\n  ) AS u(indicator, value)\n\
         \x20 WHERE u.value IS NOT NULL\n\
         \x20   AND s.timestamp_start >= COALESCE(NULLIF(p.publish_from, -9223372036854775808), -9223372036854775808)\n\
         \x20   AND s.timestamp_start > COALESCE(p.after_ts, -9223372036854775808)\n\
         \x20   AND u.indicator = ANY(p.indicators)\n\
         ),\n"
    )
    .unwrap();

    write!(
        sql,
        "ranked AS (\n\
         \x20 SELECT u.*, DENSE_RANK() OVER (ORDER BY u.timestamp_start) AS ts_rank FROM unpivoted u\n\
         )\n\
         SELECT r.coin, r.indicator, r.timestamp_start, r.timestamp_end, r.value, r.version, r.warmup_complete\n\
         FROM ranked r CROSS JOIN params p\n\
         WHERE r.ts_rank <= p.lim\n\
         ORDER BY r.timestamp_start, r.coin, r.indicator;"
    )
    .unwrap();

    sql
}
