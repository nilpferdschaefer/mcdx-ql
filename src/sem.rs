//! Semantic analysis: bind params, unify domain, derive lookback / scaffolds.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{
    AssetRef, BatchExpr, CallOp, DomainBound, EmitCount, EmitEnd, Expr, IndexSelector,
    LookbackBound, Series, SeriesDomain, TrailingPeriod, WindowSpec,
};
use crate::error::Error;
use crate::interval::interval_ms;

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

/// Emit domain resolved from grammar (series domain ∩ result slice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Domain {
    /// Explicit `$from:$to` (inclusive `timestamp_start` ms).
    Absolute {
        from_param: String,
        to_param: String,
        from_ms: i64,
        to_ms: i64,
    },
    /// Largest possible result series from available source data (default when domain omitted).
    Full,
    /// Emit `bars` result bars ending `end_offset` bars before the latest available bar
    /// (`end_offset = 0` → ending at latest). Used for `N@latest` and negative slices like `[-1]`.
    TrailingLatest { bars: i32, end_offset: i32 },
    /// Emit `count` result bars starting at 1-based result index `start` from the first
    /// warmup-complete bar (`expr[4]` / `expr[4:10]` / `expr[:5]`). `count == i32::MAX` = through end.
    FromStart { start: i32, count: i32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub domain: Domain,
    /// Unified bar bucket from series literals (`1d`, `1h`, …).
    pub reporting_period: String,
    /// Unified unaggregated source (`binance` in `[binance:close.1d]`), or `None`
    /// for aggregated / canonical series (`[close.1d]`).
    pub source: Option<String>,
    /// Datastore `params_hash` for the unified source (empty-params when aggregated).
    pub params_hash: String,
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
    analyze_obj(batch, params, expr_src, &BTreeSet::new())
}

pub fn analyze_obj(
    batch: &BatchExpr,
    params: &BTreeMap<String, ParamValue>,
    expr_src: &str,
    obj_data_types: &BTreeSet<String>,
) -> Result<Analysis, Error> {
    let mut ctx = AnalyzeCtx {
        params,
        expr_src,
        obj_data_types,
        domain: None,
        reporting_period: None,
        source: None,
        saw_source: false,
        saw_series: false,
        max_lookback: 0,
        needs_bar_ret: false,
        needs_closes_to_date: false,
        needs_highs_to_date: false,
        needs_lows_to_date: false,
        market_tickers: BTreeSet::new(),
        required_params: BTreeSet::new(),
        series_names: BTreeSet::new(),
        implicit_row_positions: Vec::new(),
        has_qualified_series: false,
        range_scope: None,
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

    // Once an expression references another asset via `@`, every series must be
    // qualified explicitly so the comparison is unambiguous — the implicit row
    // asset must be written as `@self`.
    if ctx.has_qualified_series {
        if let Some(&pos) = ctx.implicit_row_positions.first() {
            return Err(Error::sem(
                "this expression compares more than one asset, so every series must be qualified after `@` — write the row asset as `@self` (e.g. `[close.1d@self; …]`)",
                expr_src,
                Some(pos),
            ));
        }
    }

    if !ctx.saw_series {
        return Err(Error::sem(
            "expression must contain at least one series literal",
            expr_src,
            None,
        ));
    }

    let reporting_period = ctx.reporting_period.ok_or_else(|| {
        Error::sem("expression must declare a series bucket (e.g. `[close.1d]`)", expr_src, None)
    })?;

    let domain = ctx.domain.unwrap_or(Domain::Full);
    match &domain {
        Domain::Absolute {
            from_param,
            to_param,
            from_ms,
            to_ms,
        } => {
            if from_ms > to_ms {
                return Err(Error::sem(
                    format!(
                        "domain requires ${from_param} <= ${to_param} (got {from_ms} > {to_ms})"
                    ),
                    expr_src,
                    None,
                ));
            }
        }
        Domain::TrailingLatest { bars, end_offset } => {
            if *bars < 1 {
                return Err(Error::sem(
                    format!("trailing emit bar count must be >= 1, got {bars}"),
                    expr_src,
                    None,
                ));
            }
            if *end_offset < 0 {
                return Err(Error::sem(
                    format!("trailing end_offset must be >= 0, got {end_offset}"),
                    expr_src,
                    None,
                ));
            }
        }
        Domain::FromStart { start, count } => {
            if *start < 1 {
                return Err(Error::sem(
                    format!("result index must be >= 1, got {start}"),
                    expr_src,
                    None,
                ));
            }
            if *count < 1 {
                return Err(Error::sem(
                    format!("result slice length must be >= 1, got {count}"),
                    expr_src,
                    None,
                ));
            }
        }
        Domain::Full => {}
    }

    // Reject request-level dirty_from/dirty_to if present as params names used wrongly —
    // callers should not pass them; domain comes from grammar.
    if params.contains_key("dirty_from") || params.contains_key("dirty_to") {
        return Err(Error::sem(
            "request-level dirty_from/dirty_to are rejected; use `$from:$to`, `N@$end`, `N@latest`, or result slices in the grammar",
            expr_src,
            None,
        ));
    }

    let source = ctx.source;
    let params_hash = crate::params_hash_for_source(source.as_deref());

    Ok(Analysis {
        domain,
        reporting_period,
        source,
        params_hash,
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
    /// Series stems stored in `obj` (accepted without the scalar allowlist).
    obj_data_types: &'a BTreeSet<String>,
    domain: Option<Domain>,
    reporting_period: Option<String>,
    /// Unified source once any series has been seen (`None` = aggregated).
    source: Option<String>,
    /// Whether at least one series has set [`Self::source`].
    saw_source: bool,
    saw_series: bool,
    max_lookback: i32,
    needs_bar_ret: bool,
    needs_closes_to_date: bool,
    needs_highs_to_date: bool,
    needs_lows_to_date: bool,
    market_tickers: BTreeSet<String>,
    required_params: BTreeSet<String>,
    series_names: BTreeSet<String>,
    /// Byte offsets of series that use the implicit row asset (no `@`).
    implicit_row_positions: Vec<usize>,
    /// Whether any series is `@`-qualified (literal ticker or `$param`).
    has_qualified_series: bool,
    /// Set to the byte offset of an enclosing `expr[$from:$to]` while walking its
    /// subtree; descendants inherit that range and may not declare their own.
    range_scope: Option<usize>,
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

    fn interval(&self, pos: usize) -> Result<i64, Error> {
        let bucket = self.reporting_period.as_deref().ok_or_else(|| {
            Error::sem(
                "result index/slice requires a series bucket",
                self.expr_src,
                Some(pos),
            )
        })?;
        interval_ms(bucket).map_err(|e| Error::sem(format!("{e}"), self.expr_src, Some(pos)))
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
            Expr::Index {
                base,
                selector: IndexSelector::Range { from, to },
                pos,
            } => {
                // Emit-range override: fix the range up front so descendant
                // series inherit it, then walk the subtree under that scope.
                let resolved = self.resolve_absolute_domain(from, to)?;
                self.unify_domain(resolved, *pos)?;
                let saved = self.range_scope.replace(*pos);
                self.walk_expr(base)?;
                self.range_scope = saved;
                Ok(())
            }
            Expr::Index {
                base,
                selector,
                pos,
            } => {
                self.walk_expr(base)?;
                let current = self.domain.clone().ok_or_else(|| {
                    Error::sem(
                        "result index/slice requires a timeseries expression",
                        self.expr_src,
                        Some(*pos),
                    )
                })?;
                let interval = self.interval(*pos)?;
                let sliced = apply_selector(current, selector, interval, self.expr_src, *pos)?;
                self.domain = Some(sliced);
                Ok(())
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
        // Object (`obj`) series bypass the scalar allowlist; they compile to a
        // raw object fetch or a `->field` scalar projection in the obj envelope.
        if !self.obj_data_types.contains(&s.name) {
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
        }
        self.saw_series = true;
        self.series_names.insert(s.name.clone());

        if !self.saw_source {
            self.source = s.source.clone();
            self.saw_source = true;
        } else if self.source != s.source {
            let left = self
                .source
                .as_deref()
                .map(|src| format!("[{src}:…]"))
                .unwrap_or_else(|| "[…] (aggregated)".into());
            let right = s
                .source
                .as_deref()
                .map(|src| format!("[{src}:…]"))
                .unwrap_or_else(|| "[…] (aggregated)".into());
            return Err(Error::sem(
                format!(
                    "all series sources must match; got {left} vs {right}"
                ),
                self.expr_src,
                Some(s.pos),
            ));
        }

        match &self.reporting_period {
            None => self.reporting_period = Some(s.bucket.clone()),
            Some(existing) if existing == &s.bucket => {}
            Some(existing) => {
                return Err(Error::sem(
                    format!(
                        "all series buckets must match; got `{existing}` vs `{}`",
                        s.bucket
                    ),
                    self.expr_src,
                    Some(s.pos),
                ));
            }
        }

        match &s.asset {
            AssetRef::Row => self.implicit_row_positions.push(s.pos),
            AssetRef::SelfRow => {}
            AssetRef::Literal(t) => {
                self.has_qualified_series = true;
                self.market_tickers.insert(t.clone());
            }
            AssetRef::Param(p) => {
                self.has_qualified_series = true;
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

        // A series carrying its own range while an enclosing `[$from:$to]` is in
        // effect is rejected (also caught at parse time; kept as a safety net).
        if s.domain.is_some() {
            if let Some(scope) = self.range_scope {
                return Err(Error::sem(
                    format!(
                        "this series specifies a range, but an enclosing `[$from:$to]` (at byte {scope}) already set one; specify the range at only one level"
                    ),
                    self.expr_src,
                    Some(s.pos),
                ));
            }
        }

        let resolved = match &s.domain {
            None => {
                // With an enclosing range in scope, inherit it (contribute
                // nothing). Otherwise this series is "full" and must unify with
                // any sibling-declared range.
                if self.range_scope.is_some() {
                    return Ok(());
                }
                Domain::Full
            }
            Some(SeriesDomain::Absolute { from, to }) => self.resolve_absolute_domain(from, to)?,
            Some(SeriesDomain::TrailingBars { count, end, pos }) => {
                let bars = self.resolve_emit_count(count)?;
                if bars < 1 {
                    return Err(Error::sem(
                        format!("trailing emit bar count must be >= 1, got {bars}"),
                        self.expr_src,
                        Some(*pos),
                    ));
                }
                let interval = interval_ms(&s.bucket).map_err(|e| {
                    Error::sem(format!("{e}"), self.expr_src, Some(s.pos))
                })?;
                match end {
                    EmitEnd::Latest { .. } => Domain::TrailingLatest {
                        bars,
                        end_offset: 0,
                    },
                    EmitEnd::Param { name, pos: epos } => {
                        let end_v = self.require_param(name, Some(*epos))?;
                        let to_ms = end_v.as_i64().ok_or_else(|| {
                            Error::sem(
                                format!("emit end param `${name}` must be integer epoch-ms"),
                                self.expr_src,
                                Some(*epos),
                            )
                        })?;
                        let from_ms = to_ms
                            .checked_sub((bars as i64 - 1).checked_mul(interval).ok_or_else(
                                || Error::sem("emit range overflow", self.expr_src, Some(*pos)),
                            )?)
                            .ok_or_else(|| {
                                Error::sem("emit range overflow", self.expr_src, Some(*pos))
                            })?;
                        Domain::Absolute {
                            from_param: format!("{name}-({bars}-1)*interval"),
                            to_param: name.clone(),
                            from_ms,
                            to_ms,
                        }
                    }
                }
            }
        };

        self.unify_domain(resolved, s.pos)?;
        Ok(())
    }

    /// Resolve an absolute `$from:$to` range (from a series domain or a postfix
    /// `[$from:$to]`) into a `Domain::Absolute`, binding the epoch-ms params.
    fn resolve_absolute_domain(
        &mut self,
        from: &DomainBound,
        to: &DomainBound,
    ) -> Result<Domain, Error> {
        let from_v = self.require_param(&from.name, Some(from.pos))?;
        let to_v = self.require_param(&to.name, Some(to.pos))?;
        let from_ms = from_v.as_i64().ok_or_else(|| {
            Error::sem(
                format!("domain param `${}` must be integer epoch-ms", from.name),
                self.expr_src,
                Some(from.pos),
            )
        })?;
        let to_ms = to_v.as_i64().ok_or_else(|| {
            Error::sem(
                format!("domain param `${}` must be integer epoch-ms", to.name),
                self.expr_src,
                Some(to.pos),
            )
        })?;
        Ok(Domain::Absolute {
            from_param: from.name.clone(),
            to_param: to.name.clone(),
            from_ms,
            to_ms,
        })
    }

    fn resolve_emit_count(&mut self, count: &EmitCount) -> Result<i32, Error> {
        match count {
            EmitCount::Int { value, pos } => {
                if *value < 1 || *value > i32::MAX as i64 {
                    return Err(Error::sem(
                        format!("trailing emit bar count must be in 1..={}, got {value}", i32::MAX),
                        self.expr_src,
                        Some(*pos),
                    ));
                }
                Ok(*value as i32)
            }
            EmitCount::Param { name, pos } => {
                let v = self.require_param(name, Some(*pos))?;
                let n = v.as_i64().ok_or_else(|| {
                    Error::sem(
                        format!("emit count param `${name}` must be integer"),
                        self.expr_src,
                        Some(*pos),
                    )
                })?;
                if n < 1 || n > i32::MAX as i64 {
                    return Err(Error::sem(
                        format!("trailing emit bar count must be in 1..={}, got {n}", i32::MAX),
                        self.expr_src,
                        Some(*pos),
                    ));
                }
                Ok(n as i32)
            }
        }
    }

    fn unify_domain(&mut self, domain: Domain, pos: usize) -> Result<(), Error> {
        match (&self.domain, &domain) {
            (None, d) => {
                self.domain = Some(d.clone());
                Ok(())
            }
            (Some(a), b) if a == b => Ok(()),
            (Some(existing), new) => Err(Error::sem(
                format!("conflicting series domains in one expr: {existing:?} vs {new:?}"),
                self.expr_src,
                Some(pos),
            )),
        }
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
                if period < 2 {
                    return Err(Error::sem(
                        format!("RMA requires $period >= 2, got {period}"),
                        self.expr_src,
                        Some(pos),
                    ));
                }
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
                if period < 2 {
                    return Err(Error::sem(
                        format!("RSI requires $period >= 2, got {period}"),
                        self.expr_src,
                        Some(pos),
                    ));
                }
                self.max_lookback = self.max_lookback.max(period + 1);
                self.walk_expr(&args[0])?;
            }
            CallOp::RegrSlope => {
                self.needs_bar_ret = true;
                if args.len() != 2 {
                    return Err(Error::sem(
                        "REGR expects (y, x) plus a period window, e.g. REGR(RET([close.1h; …]), RET([close.1h@$benchmark; …]), 31)",
                        self.expr_src,
                        Some(pos),
                    ));
                }
                let period = self.window_period(window, pos)?;
                self.max_lookback = self.max_lookback.max(period);
                self.walk_expr(&args[0])?;
                self.walk_expr(&args[1])?;
            }
            CallOp::Sqrt
            | CallOp::Abs
            | CallOp::Sin
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

/// Apply a result index/slice to an already-resolved emit domain.
fn apply_selector(
    domain: Domain,
    selector: &IndexSelector,
    interval: i64,
    expr_src: &str,
    pos: usize,
) -> Result<Domain, Error> {
    match selector {
        // Range overrides are resolved in `walk_expr` before any result slice,
        // never routed through `apply_selector`.
        IndexSelector::Range { .. } => Ok(domain),
        IndexSelector::Index(i) => {
            if *i == 0 {
                return Err(Error::sem(
                    "result index `0` is illegal — positives are 1-based, negatives count from the end (`-1` = last)",
                    expr_src,
                    Some(pos),
                ));
            }
            if *i > 0 {
                if *i > i32::MAX as i64 {
                    return Err(Error::sem("result index out of range", expr_src, Some(pos)));
                }
                apply_from_start(domain, *i as i32, 1, interval, expr_src, pos)
            } else {
                let from_end = -*i;
                if from_end > i32::MAX as i64 {
                    return Err(Error::sem("result index out of range", expr_src, Some(pos)));
                }
                // Single element: the `from_end`-th bar from the end.
                apply_trailing(domain, 1, (from_end as i32) - 1, interval, expr_src, pos)
            }
        }
        IndexSelector::Slice { start, end } => match (start, end) {
            (None, None) => Ok(domain), // `[:]` — no further restriction
            (Some(s), None) => {
                if *s == 0 {
                    return Err(Error::sem(
                        "result index `0` is illegal — positives are 1-based, negatives count from the end",
                        expr_src,
                        Some(pos),
                    ));
                }
                if *s > 0 {
                    // `[4:]` — from 4 through end
                    if *s > i32::MAX as i64 {
                        return Err(Error::sem("result index out of range", expr_src, Some(pos)));
                    }
                    apply_from_start(domain, *s as i32, i32::MAX, interval, expr_src, pos)
                } else {
                    // `[-10:]` — last 10
                    let n = -*s;
                    if n > i32::MAX as i64 {
                        return Err(Error::sem("result index out of range", expr_src, Some(pos)));
                    }
                    apply_trailing(domain, n as i32, 0, interval, expr_src, pos)
                }
            }
            (None, Some(e)) => {
                if *e == 0 {
                    return Err(Error::sem(
                        "result index `0` is illegal — positives are 1-based, negatives count from the end",
                        expr_src,
                        Some(pos),
                    ));
                }
                if *e > 0 {
                    // `[:5]` — first 5
                    if *e > i32::MAX as i64 {
                        return Err(Error::sem("result index out of range", expr_src, Some(pos)));
                    }
                    apply_from_start(domain, 1, *e as i32, interval, expr_src, pos)
                } else {
                    // `[:-k]` — through the k-th from end inclusive
                    let end_from_end = -*e;
                    if end_from_end > i32::MAX as i64 {
                        return Err(Error::sem("result index out of range", expr_src, Some(pos)));
                    }
                    apply_through_end_offset(
                        domain,
                        (end_from_end as i32) - 1,
                        interval,
                        expr_src,
                        pos,
                    )
                }
            }
            (Some(s), Some(e)) => resolve_closed_slice(domain, *s, *e, interval, expr_src, pos),
        },
    }
}

fn resolve_closed_slice(
    domain: Domain,
    start: i64,
    end: i64,
    interval: i64,
    expr_src: &str,
    pos: usize,
) -> Result<Domain, Error> {
    if start == 0 || end == 0 {
        return Err(Error::sem(
            "result index `0` is illegal — positives are 1-based, negatives count from the end",
            expr_src,
            Some(pos),
        ));
    }
    if start > 0 && end > 0 {
        if start > end {
            return Err(Error::sem(
                format!("result slice start ({start}) must be <= end ({end})"),
                expr_src,
                Some(pos),
            ));
        }
        if end > i32::MAX as i64 {
            return Err(Error::sem("result index out of range", expr_src, Some(pos)));
        }
        let count = (end - start + 1) as i32;
        return apply_from_start(domain, start as i32, count, interval, expr_src, pos);
    }
    if start < 0 && end < 0 {
        if start > end {
            // e.g. -1:-10 — invalid order
            return Err(Error::sem(
                format!("result slice start ({start}) must be <= end ({end}) when both are negative"),
                expr_src,
                Some(pos),
            ));
        }
        // [-10:-1]: start_from_end=10, end_from_end=1
        let start_from_end = -start;
        let end_from_end = -end;
        let bars = start_from_end - end_from_end + 1;
        if bars > i32::MAX as i64 {
            return Err(Error::sem("result slice too large", expr_src, Some(pos)));
        }
        let end_offset = (end_from_end as i32) - 1;
        return apply_trailing(domain, bars as i32, end_offset, interval, expr_src, pos);
    }
    if start > 0 && end < 0 {
        // `[4:-1]` — from 4 through last
        if start > i32::MAX as i64 {
            return Err(Error::sem("result index out of range", expr_src, Some(pos)));
        }
        if end != -1 {
            return Err(Error::sem(
                "mixed slices must end at `-1` (through last), e.g. `[4:-1]`",
                expr_src,
                Some(pos),
            ));
        }
        return apply_from_start(domain, start as i32, i32::MAX, interval, expr_src, pos);
    }
    Err(Error::sem(
        "unsupported mixed result slice — use both-positive, both-negative, or `[n:-1]`",
        expr_src,
        Some(pos),
    ))
}

fn apply_from_start(
    domain: Domain,
    start: i32,
    count: i32,
    interval: i64,
    expr_src: &str,
    pos: usize,
) -> Result<Domain, Error> {
    match domain {
        Domain::Full => Ok(Domain::FromStart { start, count }),
        Domain::Absolute {
            from_param,
            to_param,
            from_ms,
            to_ms,
        } => {
            let new_from = from_ms
                .checked_add((start as i64 - 1).checked_mul(interval).ok_or_else(|| {
                    Error::sem("result slice overflow", expr_src, Some(pos))
                })?)
                .ok_or_else(|| Error::sem("result slice overflow", expr_src, Some(pos)))?;
            let new_to = if count == i32::MAX {
                to_ms
            } else {
                let cand = new_from
                    .checked_add((count as i64 - 1).checked_mul(interval).ok_or_else(|| {
                        Error::sem("result slice overflow", expr_src, Some(pos))
                    })?)
                    .ok_or_else(|| Error::sem("result slice overflow", expr_src, Some(pos)))?;
                cand.min(to_ms)
            };
            if new_from > to_ms {
                return Err(Error::sem(
                    format!("result index {start} is past the end of the emit domain"),
                    expr_src,
                    Some(pos),
                ));
            }
            Ok(Domain::Absolute {
                from_param,
                to_param,
                from_ms: new_from,
                to_ms: new_to,
            })
        }
        Domain::TrailingLatest { bars, end_offset } => {
            // Restrict within the trailing window by converting to a smaller trailing window.
            let last_index = start.saturating_add(count.saturating_sub(1));
            if count != i32::MAX && last_index > bars {
                return Err(Error::sem(
                    format!(
                        "result slice [{start}:{last_index}] exceeds trailing emit window of {bars} bars"
                    ),
                    expr_src,
                    Some(pos),
                ));
            }
            let take = if count == i32::MAX {
                bars - start + 1
            } else {
                count
            };
            if take < 1 {
                return Err(Error::sem(
                    format!("result index {start} is past the end of the emit domain"),
                    expr_src,
                    Some(pos),
                ));
            }
            // Window ends at original end; start moves forward by (start-1).
            // New end_offset stays; new bars = from start to end of window.
            let new_bars = take;
            let new_end_offset = end_offset + (bars - (start + take - 1));
            Ok(Domain::TrailingLatest {
                bars: new_bars,
                end_offset: new_end_offset,
            })
        }
        Domain::FromStart {
            start: base_start,
            count: base_count,
        } => {
            let abs_start = base_start + start - 1;
            let available = if base_count == i32::MAX {
                i32::MAX
            } else {
                base_count - start + 1
            };
            if available < 1 {
                return Err(Error::sem(
                    format!("result index {start} is past the end of the emit domain"),
                    expr_src,
                    Some(pos),
                ));
            }
            let new_count = if count == i32::MAX {
                available
            } else {
                count.min(available)
            };
            Ok(Domain::FromStart {
                start: abs_start,
                count: new_count,
            })
        }
    }
}

fn apply_trailing(
    domain: Domain,
    bars: i32,
    end_offset: i32,
    interval: i64,
    expr_src: &str,
    pos: usize,
) -> Result<Domain, Error> {
    match domain {
        Domain::Full => Ok(Domain::TrailingLatest { bars, end_offset }),
        Domain::Absolute {
            from_param,
            to_param,
            from_ms,
            to_ms,
        } => {
            let new_to = to_ms
                .checked_sub((end_offset as i64).checked_mul(interval).ok_or_else(|| {
                    Error::sem("result slice overflow", expr_src, Some(pos))
                })?)
                .ok_or_else(|| Error::sem("result slice overflow", expr_src, Some(pos)))?;
            let new_from = new_to
                .checked_sub((bars as i64 - 1).checked_mul(interval).ok_or_else(|| {
                    Error::sem("result slice overflow", expr_src, Some(pos))
                })?)
                .ok_or_else(|| Error::sem("result slice overflow", expr_src, Some(pos)))?;
            Ok(Domain::Absolute {
                from_param,
                to_param,
                from_ms: new_from.max(from_ms),
                to_ms: new_to.min(to_ms).max(from_ms),
            })
        }
        Domain::TrailingLatest {
            bars: base_bars,
            end_offset: base_end,
        } => {
            // Compose: take `bars` ending `end_offset` before the current window's end.
            let new_end = base_end + end_offset;
            let available_before_end = base_bars;
            let take = bars.min(available_before_end.saturating_sub(end_offset).max(0));
            if take < 1 {
                return Err(Error::sem(
                    "result slice is empty for this trailing emit window",
                    expr_src,
                    Some(pos),
                ));
            }
            Ok(Domain::TrailingLatest {
                bars: take,
                end_offset: new_end,
            })
        }
        Domain::FromStart { start, count } => {
            // Trailing slice of a from-start window → shrink from the end.
            if count == i32::MAX {
                // Unknown length — keep FromStart and let SQL/runtime pagination handle it
                // by converting to trailing relative to full series is ambiguous; reject.
                return Err(Error::sem(
                    "negative result slices on open-ended `[n:]` domains are unsupported; use an explicit end",
                    expr_src,
                    Some(pos),
                ));
            }
            let take = bars.min(count.saturating_sub(end_offset).max(0));
            if take < 1 {
                return Err(Error::sem(
                    "result slice is empty for this emit domain",
                    expr_src,
                    Some(pos),
                ));
            }
            let new_start = start + count - end_offset - take;
            Ok(Domain::FromStart {
                start: new_start,
                count: take,
            })
        }
    }
}

fn apply_through_end_offset(
    domain: Domain,
    end_offset: i32,
    interval: i64,
    expr_src: &str,
    pos: usize,
) -> Result<Domain, Error> {
    // `[:-k]` → everything through the k-th from end (inclusive).
    match domain {
        Domain::Full => {
            // Can't bound the start without knowing N; emit through max-end_offset by using
            // FromStart{1, MAX} clipped via Trailing… — use Absolute-style runtime:
            // emit_to = max - end_offset*iv, emit_from = min.
            // Represent as TrailingLatest with bars=i32::MAX? Better add handling in compile
            // for Full with end clamp. Encode as Absolute-impossible; use FromStart open + note.
            // Practical: TrailingLatest can't express "all but last offset".
            // Use a synthetic Absolute at runtime via Full + end_offset bake in compile —
            // store as TrailingLatest { bars: i32::MAX, end_offset } and teach compile:
            // if bars == MAX: emit_from = min_ts, emit_to = max - end_offset*iv
            Ok(Domain::TrailingLatest {
                bars: i32::MAX,
                end_offset,
            })
        }
        Domain::Absolute {
            from_param,
            to_param,
            from_ms,
            to_ms,
        } => {
            let new_to = to_ms
                .checked_sub((end_offset as i64).checked_mul(interval).ok_or_else(|| {
                    Error::sem("result slice overflow", expr_src, Some(pos))
                })?)
                .ok_or_else(|| Error::sem("result slice overflow", expr_src, Some(pos)))?;
            Ok(Domain::Absolute {
                from_param,
                to_param,
                from_ms,
                to_ms: new_to.max(from_ms),
            })
        }
        Domain::TrailingLatest { bars, end_offset: base } => {
            let new_end = base + end_offset;
            let take = bars.saturating_sub(end_offset);
            if take < 1 {
                return Err(Error::sem(
                    "result slice is empty for this trailing emit window",
                    expr_src,
                    Some(pos),
                ));
            }
            Ok(Domain::TrailingLatest {
                bars: take,
                end_offset: new_end,
            })
        }
        Domain::FromStart { start, count } => {
            if count == i32::MAX {
                return Err(Error::sem(
                    "negative result slices on open-ended `[n:]` domains are unsupported",
                    expr_src,
                    Some(pos),
                ));
            }
            let take = count.saturating_sub(end_offset);
            if take < 1 {
                return Err(Error::sem(
                    "result slice is empty for this emit domain",
                    expr_src,
                    Some(pos),
                ));
            }
            Ok(Domain::FromStart {
                start,
                count: take,
            })
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
        let src = "AVG([close.1d; $from:$to], $period)";
        let batch = parse_batch(src).unwrap();
        let a = analyze(&batch, &params(14, 100, 200), src).unwrap();
        assert_eq!(a.max_lookback, 14);
        assert_eq!(a.reporting_period, "1d");
        assert_eq!(a.source, None);
        assert_eq!(a.params_hash, crate::EMPTY_PARAMS_HASH);
        match a.domain {
            Domain::Absolute {
                from_ms, to_ms, ..
            } => {
                assert_eq!(from_ms, 100);
                assert_eq!(to_ms, 200);
            }
            other => panic!("expected absolute, got {other:?}"),
        }
    }

    #[test]
    fn analyzes_unaggregated_source() {
        let src = "AVG([binance:close.1d; $from:$to], $period)";
        let batch = parse_batch(src).unwrap();
        let a = analyze(&batch, &params(14, 100, 200), src).unwrap();
        assert_eq!(a.source.as_deref(), Some("binance"));
        assert_eq!(
            a.params_hash,
            crate::params_hash_for_source(Some("binance"))
        );
    }

    #[test]
    fn rejects_mixed_aggregated_and_source() {
        let src = "AVG([binance:close.1d], 14) + AVG([close.1d], 14)";
        let batch = parse_batch(src).unwrap();
        let err = analyze(&batch, &BTreeMap::new(), src).unwrap_err();
        assert!(err.message.contains("sources must match"), "{}", err.message);
    }

    #[test]
    fn analyzes_full_default() {
        let src = "AVG([close.1d], $period)";
        let batch = parse_batch(src).unwrap();
        let p = BTreeMap::from([("period".into(), ParamValue::Int(14))]);
        let a = analyze(&batch, &p, src).unwrap();
        assert_eq!(a.domain, Domain::Full);
    }

    #[test]
    fn analyzes_last_index() {
        let src = "AVG([close.1d], $period)[-1]";
        let batch = parse_batch(src).unwrap();
        let p = BTreeMap::from([("period".into(), ParamValue::Int(14))]);
        let a = analyze(&batch, &p, src).unwrap();
        assert_eq!(
            a.domain,
            Domain::TrailingLatest {
                bars: 1,
                end_offset: 0
            }
        );
    }

    #[test]
    fn analyzes_trailing_slice() {
        let src = "AVG([close.1d], 14)[-10:-1]";
        let batch = parse_batch(src).unwrap();
        let a = analyze(&batch, &BTreeMap::new(), src).unwrap();
        assert_eq!(
            a.domain,
            Domain::TrailingLatest {
                bars: 10,
                end_offset: 0
            }
        );
    }

    #[test]
    fn analyzes_positive_index() {
        let src = "AVG([close.1d], 14)[4]";
        let batch = parse_batch(src).unwrap();
        let a = analyze(&batch, &BTreeMap::new(), src).unwrap();
        assert_eq!(a.domain, Domain::FromStart { start: 4, count: 1 });
    }

    #[test]
    fn analyzes_absolute_last() {
        let src = "AVG([close.1d; $from:$to], $period)[-1]";
        let batch = parse_batch(src).unwrap();
        let a = analyze(&batch, &params(14, 100, 200), src).unwrap();
        // interval ignored for absolute shrink using ms — to stays 200 when interval applied
        // from_ms/to were 100/200 which are not aligned; shrink uses interval of 1d
        match a.domain {
            Domain::Absolute { from_ms, to_ms, .. } => {
                assert_eq!(to_ms, 200);
                assert_eq!(from_ms, 200);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_domain_mismatch() {
        let mut p = params(14, 100, 200);
        p.insert("from2".into(), ParamValue::Int(50));
        let src =
            "AVG([close.1d; $from:$to], $period) + AVG([close.1d; $from2:$to], $period)";
        let batch = parse_batch(src).unwrap();
        let err = analyze(&batch, &p, src).unwrap_err();
        assert!(
            err.message.contains("conflicting") || err.message.contains("same"),
            "{}",
            err.message
        );
    }

    #[test]
    fn rejects_bucket_mismatch() {
        let src = "AVG([close.1d; $from:$to], $period) + AVG([close.1h; $from:$to], $period)";
        let batch = parse_batch(src).unwrap();
        let err = analyze(&batch, &params(14, 100, 200), src).unwrap_err();
        assert!(err.message.contains("buckets must match"));
    }

    #[test]
    fn rejects_dirty_params() {
        let mut p = params(14, 100, 200);
        p.insert("dirty_from".into(), ParamValue::Int(1));
        let src = "AVG([close.1d; $from:$to], $period)";
        let batch = parse_batch(src).unwrap();
        assert!(analyze(&batch, &p, src).is_err());
    }
}
