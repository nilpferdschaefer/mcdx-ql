//! Semantic analysis: bind params, unify domain, derive lookback / scaffolds.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{
    AssetRef, BatchExpr, CallOp, Expr, LookbackBound, Series, TrailingPeriod, WindowSpec,
};
use crate::error::Error;

/// Bound parameter values from the request `params` object.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Int(i64),
    Float(f64),
    Text(String),
}

impl ParamValue {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Float(v) if v.fract() == 0.0 && *v >= i64::MIN as f64 && *v <= i64::MAX as f64 => {
                Some(*v as i64)
            }
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(*v as f64),
            Self::Float(v) => Some(*v),
            Self::Text(_) => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
    pub from_param: String,
    pub to_param: String,
    pub from_ms: i64,
    pub to_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub domain: Domain,
    pub max_lookback: i32,
    pub needs_bar_ret: bool,
    pub needs_closes_to_date: bool,
    pub needs_highs_to_date: bool,
    pub needs_lows_to_date: bool,
    /// Qualified market tickers referenced via `@` (resolved literals).
    pub market_tickers: BTreeSet<String>,
    pub required_params: BTreeSet<String>,
    pub series_names: BTreeSet<String>,
}

pub fn analyze(
    batch: &BatchExpr,
    params: &BTreeMap<String, ParamValue>,
    expr_src: &str,
) -> Result<Analysis, Error> {
    let mut ctx = AnalyzeCtx {
        params,
        expr_src,
        domain: None,
        max_lookback: 0,
        needs_bar_ret: false,
        needs_closes_to_date: false,
        needs_highs_to_date: false,
        needs_lows_to_date: false,
        market_tickers: BTreeSet::new(),
        required_params: BTreeSet::new(),
        series_names: BTreeSet::new(),
    };

    match batch {
        BatchExpr::Single(e) => ctx.walk_expr(e)?,
        BatchExpr::Batch(map) => {
            if map.is_empty() {
                return Err(Error::sem("batch must contain at least one binding", expr_src, None));
            }
            for e in map.values() {
                ctx.walk_expr(e)?;
            }
        }
    }

    let domain = ctx.domain.ok_or_else(|| {
        Error::sem(
            "expression must contain at least one series with a domain",
            expr_src,
            None,
        )
    })?;

    if domain.from_ms > domain.to_ms {
        return Err(Error::sem(
            format!(
                "domain requires ${} <= ${} (got {} > {})",
                domain.from_param, domain.to_param, domain.from_ms, domain.to_ms
            ),
            expr_src,
            None,
        ));
    }

    // Reject request-level dirty_from/dirty_to if present as params names used wrongly —
    // callers should not pass them; domain comes from grammar. Soft check: if both dirty_*
    // exist as params but are not referenced, still fail loud per spec.
    if params.contains_key("dirty_from") || params.contains_key("dirty_to") {
        return Err(Error::sem(
            "request-level dirty_from/dirty_to are rejected; express domain as series `$from:$to`",
            expr_src,
            None,
        ));
    }

    Ok(Analysis {
        domain,
        max_lookback: ctx.max_lookback.max(1),
        needs_bar_ret: ctx.needs_bar_ret,
        needs_closes_to_date: ctx.needs_closes_to_date,
        needs_highs_to_date: ctx.needs_highs_to_date,
        needs_lows_to_date: ctx.needs_lows_to_date,
        market_tickers: ctx.market_tickers,
        required_params: ctx.required_params,
        series_names: ctx.series_names,
    })
}

struct AnalyzeCtx<'a> {
    params: &'a BTreeMap<String, ParamValue>,
    expr_src: &'a str,
    domain: Option<Domain>,
    max_lookback: i32,
    needs_bar_ret: bool,
    needs_closes_to_date: bool,
    needs_highs_to_date: bool,
    needs_lows_to_date: bool,
    market_tickers: BTreeSet<String>,
    required_params: BTreeSet<String>,
    series_names: BTreeSet<String>,
}

impl<'a> AnalyzeCtx<'a> {
    fn require_param(&mut self, name: &str, pos: Option<usize>) -> Result<&'a ParamValue, Error> {
        self.required_params.insert(name.to_string());
        self.params.get(name).ok_or_else(|| {
            Error::sem(
                format!("missing param `${name}`"),
                self.expr_src,
                pos,
            )
        })
    }

    fn walk_expr(&mut self, expr: &Expr) -> Result<(), Error> {
        match expr {
            Expr::Series(s) => self.walk_series(s),
            Expr::Param { name, pos } => {
                self.require_param(name, Some(*pos))?;
                Ok(())
            }
            Expr::Literal { .. } => Ok(()),
            Expr::BinOp { left, right, .. } => {
                self.walk_expr(left)?;
                self.walk_expr(right)
            }
            Expr::Call {
                op,
                args,
                window,
                pos,
            } => self.walk_call(*op, args, window.as_ref(), *pos),
        }
    }

    fn walk_series(&mut self, s: &Series) -> Result<(), Error> {
        match s.name.as_str() {
            "close" => {}
            "high" | "low" => {
                // Scaffold hooks exist for ADX; full multi-type ordered scan is not yet emitted.
                return Err(Error::sem(
                    format!(
                        "series [{}] is reserved for ADX scaffolds and is not yet compilable",
                        s.name
                    ),
                    self.expr_src,
                    Some(s.pos),
                ));
            }
            "open" | "volume" => {
                return Err(Error::sem(
                    format!("series [{}] is not yet supported", s.name),
                    self.expr_src,
                    Some(s.pos),
                ));
            }
            other => {
                return Err(Error::sem(
                    format!("unknown series [{other}]"),
                    self.expr_src,
                    Some(s.pos),
                ));
            }
        }
        self.series_names.insert(s.name.clone());

        match &s.asset {
            AssetRef::Row => {}
            AssetRef::Literal(t) => {
                self.market_tickers.insert(t.clone());
            }
            AssetRef::Param(p) => {
                let v = self.require_param(p, Some(s.pos))?;
                let ticker = v.as_text().ok_or_else(|| {
                    Error::sem(
                        format!("param `${p}` used as asset ticker must be text"),
                        self.expr_src,
                        Some(s.pos),
                    )
                })?;
                self.market_tickers.insert(ticker.to_string());
            }
        }

        let from_v = self.require_param(&s.from.name, Some(s.from.pos))?;
        let to_v = self.require_param(&s.to.name, Some(s.to.pos))?;
        let from_ms = from_v.as_i64().ok_or_else(|| {
            Error::sem(
                format!("domain param `${}` must be integer epoch-ms", s.from.name),
                self.expr_src,
                Some(s.from.pos),
            )
        })?;
        let to_ms = to_v.as_i64().ok_or_else(|| {
            Error::sem(
                format!("domain param `${}` must be integer epoch-ms", s.to.name),
                self.expr_src,
                Some(s.to.pos),
            )
        })?;

        let domain = Domain {
            from_param: s.from.name.clone(),
            to_param: s.to.name.clone(),
            from_ms,
            to_ms,
        };

        if let Some(existing) = &self.domain {
            if existing.from_ms != domain.from_ms || existing.to_ms != domain.to_ms {
                return Err(Error::sem(
                    format!(
                        "all series domains must resolve to the same [$from,$to]; got [{},{}] vs [{},{}]",
                        existing.from_ms, existing.to_ms, domain.from_ms, domain.to_ms
                    ),
                    self.expr_src,
                    Some(s.pos),
                ));
            }
        } else {
            self.domain = Some(domain);
        }
        Ok(())
    }

    fn walk_call(
        &mut self,
        op: CallOp,
        args: &[Expr],
        window: Option<&WindowSpec>,
        pos: usize,
    ) -> Result<(), Error> {
        match op {
            CallOp::Ret => {
                self.needs_bar_ret = true;
                if args.len() != 1 {
                    return Err(Error::sem("RET expects one series argument", self.expr_src, Some(pos)));
                }
                if window.is_some() {
                    return Err(Error::sem("RET does not take a lookback window", self.expr_src, Some(pos)));
                }
                self.max_lookback = self.max_lookback.max(1);
                self.walk_expr(&args[0])?;
            }
            CallOp::Tr => {
                self.needs_closes_to_date = true;
                if args.len() != 1 {
                    return Err(Error::sem("TR expects one series argument", self.expr_src, Some(pos)));
                }
                if window.is_some() {
                    return Err(Error::sem("TR does not take a lookback window", self.expr_src, Some(pos)));
                }
                self.max_lookback = self.max_lookback.max(1);
                self.walk_expr(&args[0])?;
            }
            CallOp::Avg | CallOp::Var | CallOp::Std | CallOp::Count => {
                if args.len() != 1 {
                    return Err(Error::sem(
                        format!("{} expects one series/expr argument plus window", op.as_str()),
                        self.expr_src,
                        Some(pos),
                    ));
                }
                let period = self.window_period(window, pos)?;
                self.max_lookback = self.max_lookback.max(period);
                self.walk_expr(&args[0])?;
            }
            CallOp::Ema => {
                self.needs_closes_to_date = true;
                if args.len() != 1 {
                    return Err(Error::sem("EMA expects one series argument plus period", self.expr_src, Some(pos)));
                }
                let period = self.window_period(window, pos)?;
                self.max_lookback = self.max_lookback.max(period);
                self.walk_expr(&args[0])?;
            }
            CallOp::Rma => {
                self.needs_closes_to_date = true;
                if args.len() != 1 {
                    return Err(Error::sem("RMA expects one argument plus period", self.expr_src, Some(pos)));
                }
                let period = self.window_period(window, pos)?;
                // RMA(TR(...)) / ATR-style needs period+1 input bars
                self.max_lookback = self.max_lookback.max(period + 1);
                self.walk_expr(&args[0])?;
            }
            CallOp::Rsi => {
                self.needs_closes_to_date = true;
                if args.len() != 1 {
                    return Err(Error::sem("RSI expects one series argument plus period", self.expr_src, Some(pos)));
                }
                let period = self.window_period(window, pos)?;
                self.max_lookback = self.max_lookback.max(period + 1);
                self.walk_expr(&args[0])?;
            }
            CallOp::RegrSlope => {
                self.needs_bar_ret = true;
                if args.len() != 2 {
                    return Err(Error::sem(
                        "REGR_SLOPE expects (y, x) plus window",
                        self.expr_src,
                        Some(pos),
                    ));
                }
                let period = self.window_period(window, pos)?;
                self.max_lookback = self.max_lookback.max(period);
                self.walk_expr(&args[0])?;
                self.walk_expr(&args[1])?;
            }
            CallOp::Sqrt | CallOp::Abs => {
                if args.len() != 1 || window.is_some() {
                    return Err(Error::sem(
                        format!("{} expects one expression argument", op.as_str()),
                        self.expr_src,
                        Some(pos),
                    ));
                }
                self.walk_expr(&args[0])?;
            }
            CallOp::Power => {
                if args.len() != 2 || window.is_some() {
                    return Err(Error::sem("POWER expects two arguments", self.expr_src, Some(pos)));
                }
                self.walk_expr(&args[0])?;
                self.walk_expr(&args[1])?;
            }
            CallOp::Greatest => {
                if args.len() < 2 || window.is_some() {
                    return Err(Error::sem(
                        "GREATEST expects at least two arguments",
                        self.expr_src,
                        Some(pos),
                    ));
                }
                for a in args {
                    self.walk_expr(a)?;
                }
            }
        }
        Ok(())
    }

    fn window_period(&mut self, window: Option<&WindowSpec>, pos: usize) -> Result<i32, Error> {
        let window = window.ok_or_else(|| {
            Error::sem(
                "windowed op requires trailing `$period` or explicit lookback bounds",
                self.expr_src,
                Some(pos),
            )
        })?;
        match window {
            WindowSpec::Trailing {
                period: TrailingPeriod::Param { name, pos },
            } => {
                let v = self.require_param(name, Some(*pos))?;
                let n = v.as_i64().ok_or_else(|| {
                    Error::sem(
                        format!("period param `${name}` must be integer"),
                        self.expr_src,
                        Some(*pos),
                    )
                })?;
                if n < 1 {
                    return Err(Error::sem(
                        format!("period must be >= 1, got {n}"),
                        self.expr_src,
                        Some(*pos),
                    ));
                }
                Ok(n as i32)
            }
            WindowSpec::Trailing {
                period: TrailingPeriod::Int { value, pos },
            } => {
                if *value < 1 {
                    return Err(Error::sem(
                        format!("period must be >= 1, got {value}"),
                        self.expr_src,
                        Some(*pos),
                    ));
                }
                Ok(*value as i32)
            }
            WindowSpec::Explicit { start, end } => {
                self.walk_lookback(start)?;
                self.walk_lookback(end)?;
                // Derive bars from `t-(N-1), t` when possible; else require end=t and start=t-expr.
                match (start, end) {
                    (LookbackBound::TMinus(inner), LookbackBound::T) => {
                        let n = self.eval_lookback_bars(inner)? + 1;
                        Ok(n)
                    }
                    (LookbackBound::T0, LookbackBound::T) => {
                        // to-date window — lookback covered by unbounded scaffold; keep at least 1
                        Ok(1)
                    }
                    _ => Err(Error::sem(
                        "unsupported lookback pair; use `t-(N-1), t` or trailing `$period`",
                        self.expr_src,
                        Some(pos),
                    )),
                }
            }
        }
    }

    fn walk_lookback(&mut self, b: &LookbackBound) -> Result<(), Error> {
        match b {
            LookbackBound::T | LookbackBound::T0 => Ok(()),
            LookbackBound::TMinus(e) => self.walk_expr(e),
        }
    }

    /// Evaluate `N` in `t-N` / `t-(N-1)` to a non-negative bar offset.
    fn eval_lookback_bars(&mut self, expr: &Expr) -> Result<i32, Error> {
        let v = self.eval_const_i64(expr)?;
        if v < 0 {
            return Err(Error::sem(
                format!("lookback offset must be >= 0, got {v}"),
                self.expr_src,
                None,
            ));
        }
        Ok(v as i32)
    }

    fn eval_const_i64(&mut self, expr: &Expr) -> Result<i64, Error> {
        match expr {
            Expr::Literal { value, is_int, pos } => {
                if !*is_int && value.fract() != 0.0 {
                    return Err(Error::sem(
                        "lookback arithmetic must be integral",
                        self.expr_src,
                        Some(*pos),
                    ));
                }
                Ok(*value as i64)
            }
            Expr::Param { name, pos } => {
                let v = self.require_param(name, Some(*pos))?;
                v.as_i64().ok_or_else(|| {
                    Error::sem(
                        format!("param `${name}` must be integer in lookback"),
                        self.expr_src,
                        Some(*pos),
                    )
                })
            }
            Expr::BinOp { op, left, right } => {
                let l = self.eval_const_i64(left)?;
                let r = self.eval_const_i64(right)?;
                let v = match op {
                    crate::ast::BinOp::Add => l.checked_add(r),
                    crate::ast::BinOp::Sub => l.checked_sub(r),
                    crate::ast::BinOp::Mul => l.checked_mul(r),
                    crate::ast::BinOp::Div => {
                        if r == 0 {
                            return Err(Error::sem("division by zero in lookback", self.expr_src, None));
                        }
                        Some(l / r)
                    }
                };
                v.ok_or_else(|| Error::sem("integer overflow in lookback", self.expr_src, None))
            }
            _ => Err(Error::sem(
                "lookback additive must be a constant integer expression",
                self.expr_src,
                None,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_batch;

    fn params(period: i64, from: i64, to: i64) -> BTreeMap<String, ParamValue> {
        BTreeMap::from([
            ("period".into(), ParamValue::Int(period)),
            ("from".into(), ParamValue::Int(from)),
            ("to".into(), ParamValue::Int(to)),
        ])
    }

    #[test]
    fn analyzes_avg() {
        let src = "AVG([close; $from:$to], $period)";
        let batch = parse_batch(src).unwrap();
        let a = analyze(&batch, &params(14, 100, 200), src).unwrap();
        assert_eq!(a.max_lookback, 14);
        assert_eq!(a.domain.from_ms, 100);
        assert_eq!(a.domain.to_ms, 200);
    }

    #[test]
    fn rejects_domain_mismatch() {
        let mut p = params(14, 100, 200);
        p.insert("from2".into(), ParamValue::Int(50));
        let src = "AVG([close; $from:$to], $period) + AVG([close; $from2:$to], $period)";
        let batch = parse_batch(src).unwrap();
        let err = analyze(&batch, &p, src).unwrap_err();
        assert!(err.message.contains("same"));
    }

    #[test]
    fn rejects_dirty_params() {
        let mut p = params(14, 100, 200);
        p.insert("dirty_from".into(), ParamValue::Int(1));
        let src = "AVG([close; $from:$to], $period)";
        let batch = parse_batch(src).unwrap();
        assert!(analyze(&batch, &p, src).is_err());
    }
}
