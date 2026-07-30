//! Recursive-descent parser for the indicator grammar.

use std::collections::BTreeMap;

use crate::ast::{
    AssetRef, BatchExpr, BinOp, CallOp, DomainBound, EmitCount, EmitEnd, Expr, IndexSelector,
    LookbackBound, Series, SeriesDomain, TrailingPeriod, WindowSpec,
};
use crate::interval::interval_ms;
use crate::error::Error;
use crate::lex::{tokenize, Token, TokenKind};

pub fn parse_expr(input: &str) -> Result<Expr, Error> {
    let tokens = tokenize(input)?;
    let mut p = Parser::new(input, tokens);
    let expr = p.parse_expr()?;
    p.expect_eof()?;
    Ok(expr)
}

pub fn parse_batch(input: &str) -> Result<BatchExpr, Error> {
    let tokens = tokenize(input)?;
    let mut p = Parser::new(input, tokens);
    let batch = p.parse_batch_or_expr()?;
    p.expect_eof()?;
    Ok(batch)
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    i: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, tokens: Vec<Token>) -> Self {
        Self {
            src,
            tokens,
            i: 0,
        }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.i]
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.i].clone();
        if !matches!(t.kind, TokenKind::Eof) {
            self.i += 1;
        }
        t
    }

    fn err(&self, message: impl Into<String>, pos: Option<usize>) -> Error {
        Error::parse(message, self.src, pos)
    }

    fn expect_eof(&self) -> Result<(), Error> {
        if !matches!(self.peek().kind, TokenKind::Eof) {
            return Err(self.err(
                format!("unexpected token after expression: {:?}", self.peek().kind),
                Some(self.peek().pos),
            ));
        }
        Ok(())
    }

    fn parse_batch_or_expr(&mut self) -> Result<BatchExpr, Error> {
        if matches!(self.peek().kind, TokenKind::LBrace) {
            let map = self.parse_batch_map()?;
            // Optional batch-level emit range: `{ … }[$from:$to]`. Desugars to
            // wrapping each member with the same postfix Range so every series
            // inherits it (same rules as a single-expr postfix).
            if matches!(self.peek().kind, TokenKind::LBracket) {
                let pos = self.peek().pos;
                self.advance(); // [
                let selector = self.parse_index_selector()?;
                if !matches!(self.peek().kind, TokenKind::RBracket) {
                    return Err(self.err("expected `]` after batch range", Some(self.peek().pos)));
                }
                self.advance();
                let IndexSelector::Range { from, to } = selector else {
                    return Err(self.err(
                        "only an emit-range `[$from:$to]` may follow a batch; result index/slice belongs on a single expression",
                        Some(pos),
                    ));
                };
                let mut wrapped = BTreeMap::new();
                for (name, expr) in map {
                    if let Some(inner) = find_range_spec(&expr) {
                        return Err(self.err(
                            format!(
                                "batch range `[$from:$to]` conflicts with a range already specified by member `{name}` (at byte {inner}); a range may be specified at only one level — descendants inherit it"
                            ),
                            Some(pos),
                        ));
                    }
                    wrapped.insert(
                        name,
                        Expr::Index {
                            base: Box::new(expr),
                            selector: IndexSelector::Range {
                                from: from.clone(),
                                to: to.clone(),
                            },
                            pos,
                        },
                    );
                }
                Ok(BatchExpr::Batch(wrapped))
            } else {
                Ok(BatchExpr::Batch(map))
            }
        } else {
            Ok(BatchExpr::Single(self.parse_expr()?))
        }
    }

    fn parse_batch_map(&mut self) -> Result<BTreeMap<String, Expr>, Error> {
        self.advance(); // {
        let mut map = BTreeMap::new();
        if matches!(self.peek().kind, TokenKind::RBrace) {
            self.advance();
            return Ok(map);
        }
        loop {
            let name_tok = self.advance();
            let name = match name_tok.kind {
                TokenKind::Ident(s) => s,
                _ => {
                    return Err(self.err(
                        "expected indicator name in batch",
                        Some(name_tok.pos),
                    ))
                }
            };
            if !matches!(self.peek().kind, TokenKind::Colon) {
                return Err(self.err("expected `:` after batch name", Some(self.peek().pos)));
            }
            self.advance();
            let expr = self.parse_expr()?;
            if map.insert(name.clone(), expr).is_some() {
                return Err(self.err(
                    format!("duplicate batch name `{name}`"),
                    Some(name_tok.pos),
                ));
            }
            match &self.peek().kind {
                TokenKind::Comma => {
                    self.advance();
                    if matches!(self.peek().kind, TokenKind::RBrace) {
                        self.advance();
                        break;
                    }
                }
                TokenKind::RBrace => {
                    self.advance();
                    break;
                }
                _ => {
                    return Err(self.err(
                        "expected `,` or `}` in batch",
                        Some(self.peek().pos),
                    ))
                }
            }
        }
        Ok(map)
    }

    fn parse_expr(&mut self) -> Result<Expr, Error> {
        self.parse_additive()
    }

    fn parse_additive(&mut self) -> Result<Expr, Error> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            left = Expr::BinOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, Error> {
        let mut left = self.parse_postfix()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let right = self.parse_postfix()?;
            left = Expr::BinOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// `primary` with optional postfix: result index/slice (`AVG(...)[-1]`,
    /// `(…)[4:10]`) or an emit-range override (`(…)[$from:$to]`).
    fn parse_postfix(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_primary()?;
        while matches!(self.peek().kind, TokenKind::LBracket) {
            let pos = self.peek().pos;
            self.advance(); // [
            let selector = self.parse_index_selector()?;
            if !matches!(self.peek().kind, TokenKind::RBracket) {
                return Err(self.err("expected `]` after index/slice", Some(self.peek().pos)));
            }
            self.advance();
            // A range applies to the whole subtree and is inherited by every
            // descendant series, so a descendant may not also specify a range.
            if matches!(selector, IndexSelector::Range { .. }) {
                if let Some(inner) = find_range_spec(&expr) {
                    return Err(self.err(
                        format!(
                            "range `[$from:$to]` here conflicts with a range already specified by an inner expression (at byte {inner}); a range may be specified at only one level — descendants inherit it"
                        ),
                        Some(pos),
                    ));
                }
            }
            expr = Expr::Index {
                base: Box::new(expr),
                selector,
                pos,
            };
        }
        Ok(expr)
    }

    fn parse_index_selector(&mut self) -> Result<IndexSelector, Error> {
        // Emit-range override `[$from:$to]` — distinguished from integer
        // result slices by the leading `$` (indices/slices are integer-based).
        if matches!(self.peek().kind, TokenKind::Dollar) {
            let from = self.parse_domain_bound()?;
            if !matches!(self.peek().kind, TokenKind::Colon) {
                return Err(self.err(
                    "expected `:` in range `[$from:$to]`",
                    Some(self.peek().pos),
                ));
            }
            self.advance();
            let to = self.parse_domain_bound()?;
            return Ok(IndexSelector::Range { from, to });
        }
        // slice if we see `:` before closing `]`, else single index
        let start = if matches!(self.peek().kind, TokenKind::Colon) {
            None
        } else {
            Some(self.parse_index_int()?)
        };
        if matches!(self.peek().kind, TokenKind::Colon) {
            self.advance();
            let end = if matches!(self.peek().kind, TokenKind::RBracket) {
                None
            } else {
                Some(self.parse_index_int()?)
            };
            Ok(IndexSelector::Slice { start, end })
        } else if let Some(i) = start {
            Ok(IndexSelector::Index(i))
        } else {
            Err(self.err("empty index `[]` is illegal", Some(self.peek().pos)))
        }
    }

    fn parse_index_int(&mut self) -> Result<i64, Error> {
        let neg = if matches!(self.peek().kind, TokenKind::Minus) {
            self.advance();
            true
        } else {
            false
        };
        match self.advance().kind {
            TokenKind::Int(v) => {
                if neg {
                    Ok(-v)
                } else {
                    Ok(v)
                }
            }
            _ => Err(self.err(
                "expected integer index (e.g. `-1`, `4`, `-10:-1`)",
                Some(self.peek().pos),
            )),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, Error> {
        match &self.peek().kind {
            TokenKind::LParen => {
                self.advance();
                let e = self.parse_expr()?;
                if !matches!(self.peek().kind, TokenKind::RParen) {
                    return Err(self.err("expected `)`", Some(self.peek().pos)));
                }
                self.advance();
                Ok(e)
            }
            TokenKind::LBracket => self.parse_series(),
            TokenKind::Dollar => self.parse_param_expr(),
            TokenKind::Int(v) => {
                let pos = self.peek().pos;
                let value = *v as f64;
                self.advance();
                Ok(Expr::Literal {
                    value,
                    is_int: true,
                    pos,
                })
            }
            TokenKind::Float(v) => {
                let pos = self.peek().pos;
                let value = *v;
                self.advance();
                Ok(Expr::Literal {
                    value,
                    is_int: false,
                    pos,
                })
            }
            TokenKind::Ident(name) => {
                if let Some(op) = CallOp::parse(name) {
                    let pos = self.peek().pos;
                    self.advance();
                    self.parse_call(op, pos)
                } else if name == "t" || name == "t0" {
                    Err(self.err(
                        format!("`{name}` is only legal as a lookback bound, not an expression"),
                        Some(self.peek().pos),
                    ))
                } else {
                    Err(self.err(
                        format!(
                            "bare identifier `{name}` is illegal — series need `[]`, params need `$`, ops are uppercase builtins"
                        ),
                        Some(self.peek().pos),
                    ))
                }
            }
            _ => Err(self.err("expected expression", Some(self.peek().pos))),
        }
    }

    fn parse_param_expr(&mut self) -> Result<Expr, Error> {
        let pos = self.peek().pos;
        self.advance(); // $
        let name_tok = self.advance();
        match name_tok.kind {
            TokenKind::Ident(name) => Ok(Expr::Param { name, pos }),
            _ => Err(self.err("expected parameter name after `$`", Some(name_tok.pos))),
        }
    }

    fn parse_series(&mut self) -> Result<Expr, Error> {
        let pos = self.peek().pos;
        self.advance(); // [
        let name_tok = self.advance();
        let name = match name_tok.kind {
            TokenKind::Ident(s) => s,
            TokenKind::Dollar => {
                return Err(self.err(
                    "a range `[$from:$to]` is not a standalone value — apply it as a postfix to an expression, e.g. `REGR(…, …, 31)[$from:$to]`",
                    Some(name_tok.pos),
                ));
            }
            _ => {
                return Err(self.err("expected series name", Some(name_tok.pos)));
            }
        };

        // Required bucket: `.1d` / `.1h` / `.5m` …
        if !matches!(self.peek().kind, TokenKind::Dot) {
            return Err(self.err(
                "series bucket required — expected `[close.1d]` / `[close.1h]` style",
                Some(self.peek().pos),
            ));
        }
        let bucket_pos = self.peek().pos;
        self.advance(); // .
        let bucket = self.parse_bucket(bucket_pos)?;

        let asset = if matches!(self.peek().kind, TokenKind::At) {
            self.advance();
            match &self.peek().kind {
                TokenKind::Ident(_) => {
                    let t = self.advance();
                    match t.kind {
                        // `@self` explicitly names the per-row request asset.
                        TokenKind::Ident(s) if s == "self" => AssetRef::SelfRow,
                        TokenKind::Ident(s) => AssetRef::Literal(s),
                        _ => unreachable!(),
                    }
                }
                TokenKind::Dollar => {
                    self.advance();
                    let t = self.advance();
                    match t.kind {
                        TokenKind::Ident(s) => AssetRef::Param(s),
                        _ => {
                            return Err(self.err(
                                "expected parameter name after `@$`",
                                Some(t.pos),
                            ))
                        }
                    }
                }
                _ => {
                    return Err(self.err(
                        "expected ticker or `$param` after `@`",
                        Some(self.peek().pos),
                    ))
                }
            }
        } else {
            AssetRef::Row
        };

        // Optional domain after `;`:
        //   `$from:$to`     absolute emit range
        //   `100@$end`      N bars ending at `$end`
        //   `$n@latest`     N bars ending at latest available
        // Omitted → full possible series (reduce with postfix `[-1]`, `[-10:-1]`, …).
        let domain = if matches!(self.peek().kind, TokenKind::Semi) {
            self.advance(); // ;
            Some(self.parse_series_domain()?)
        } else {
            None
        };

        if !matches!(self.peek().kind, TokenKind::RBracket) {
            return Err(self.err("expected `]`", Some(self.peek().pos)));
        }
        self.advance();

        Ok(Expr::Series(Series {
            name,
            bucket,
            asset,
            domain,
            pos,
        }))
    }

    /// Parse `1d` / `15m` / `1h` / `1w` after a `.`.
    fn parse_bucket(&mut self, pos: usize) -> Result<String, Error> {
        let n_tok = self.advance();
        let n = match n_tok.kind {
            TokenKind::Int(v) => v,
            _ => {
                return Err(self.err(
                    "expected bucket like `1d` / `1h` / `5m` after `.`",
                    Some(n_tok.pos),
                ))
            }
        };
        let unit_tok = self.advance();
        let unit = match unit_tok.kind {
            TokenKind::Ident(s) => s,
            _ => {
                return Err(self.err(
                    "expected bucket unit `m`/`h`/`d`/`w` after size",
                    Some(unit_tok.pos),
                ))
            }
        };
        if !matches!(unit.as_str(), "m" | "h" | "d" | "w") {
            return Err(self.err(
                format!("unknown bucket unit `{unit}` — use m/h/d/w"),
                Some(unit_tok.pos),
            ));
        }
        let bucket = format!("{n}{unit}");
        interval_ms(&bucket).map_err(|_| {
            self.err(
                format!("unsupported reporting_period / bucket `{bucket}`"),
                Some(pos),
            )
        })?;
        Ok(bucket)
    }

    fn parse_series_domain(&mut self) -> Result<SeriesDomain, Error> {
        let pos = self.peek().pos;
        match &self.peek().kind {
            TokenKind::Int(n) => {
                let value = *n;
                self.advance();
                if !matches!(self.peek().kind, TokenKind::At) {
                    return Err(self.err(
                        "expected `@` in trailing domain `N@$end` / `N@latest`",
                        Some(self.peek().pos),
                    ));
                }
                self.advance();
                let end = self.parse_emit_end()?;
                Ok(SeriesDomain::TrailingBars {
                    count: EmitCount::Int { value, pos },
                    end,
                    pos,
                })
            }
            TokenKind::Dollar => {
                self.advance();
                let name_tok = self.advance();
                let name = match name_tok.kind {
                    TokenKind::Ident(s) => s,
                    _ => {
                        return Err(self.err(
                            "expected name after `$` in domain",
                            Some(name_tok.pos),
                        ))
                    }
                };
                if name == "t" || name == "t0" {
                    return Err(self.err(
                        "`t`/`t0` are illegal inside the domain slot",
                        Some(name_tok.pos),
                    ));
                }
                match &self.peek().kind {
                    TokenKind::Colon => {
                        self.advance();
                        let to = self.parse_domain_bound()?;
                        Ok(SeriesDomain::Absolute {
                            from: DomainBound {
                                name,
                                pos: name_tok.pos,
                            },
                            to,
                        })
                    }
                    TokenKind::At => {
                        self.advance();
                        let end = self.parse_emit_end()?;
                        Ok(SeriesDomain::TrailingBars {
                            count: EmitCount::Param {
                                name,
                                pos: name_tok.pos,
                            },
                            end,
                            pos,
                        })
                    }
                    _ => Err(self.err(
                        "expected `:$to` (absolute) or `@$end`/`@latest` (trailing N bars)",
                        Some(self.peek().pos),
                    )),
                }
            }
            _ => Err(self.err(
                "expected domain `$from:$to` or trailing `N@$end` / `$n@latest`",
                Some(pos),
            )),
        }
    }

    fn parse_emit_end(&mut self) -> Result<EmitEnd, Error> {
        let pos = self.peek().pos;
        match &self.peek().kind {
            TokenKind::Dollar => {
                self.advance();
                let name_tok = self.advance();
                match name_tok.kind {
                    TokenKind::Ident(name) => {
                        if name == "t" || name == "t0" || name == "latest" {
                            return Err(self.err(
                                "emit end must be `$name` (epoch-ms) or the keyword `latest`",
                                Some(name_tok.pos),
                            ));
                        }
                        Ok(EmitEnd::Param { name, pos })
                    }
                    _ => Err(self.err("expected param name after `$`", Some(name_tok.pos))),
                }
            }
            TokenKind::Ident(s) if s == "latest" => {
                self.advance();
                Ok(EmitEnd::Latest { pos })
            }
            _ => Err(self.err(
                "expected `$end` or `latest` after `@` in trailing domain",
                Some(pos),
            )),
        }
    }

    fn parse_domain_bound(&mut self) -> Result<DomainBound, Error> {
        let pos = self.peek().pos;
        if !matches!(self.peek().kind, TokenKind::Dollar) {
            return Err(self.err(
                "domain bound must be `$name` (absolute ms); `t`/`t0` illegal in domain",
                Some(pos),
            ));
        }
        self.advance();
        let name_tok = self.advance();
        match name_tok.kind {
            TokenKind::Ident(name) => {
                if name == "t" || name == "t0" {
                    return Err(self.err(
                        "`t`/`t0` are illegal inside the domain slot",
                        Some(name_tok.pos),
                    ));
                }
                Ok(DomainBound { name, pos })
            }
            _ => Err(self.err("expected domain parameter name", Some(name_tok.pos))),
        }
    }

    fn parse_call(&mut self, op: CallOp, pos: usize) -> Result<Expr, Error> {
        if !matches!(self.peek().kind, TokenKind::LParen) {
            return Err(self.err("expected `(` after builtin op", Some(self.peek().pos)));
        }
        self.advance();

        if matches!(self.peek().kind, TokenKind::RParen) {
            return Err(self.err("empty argument list", Some(self.peek().pos)));
        }

        let first = self.parse_expr()?;
        let mut args = vec![first];
        let mut window = None;
        let windowed = op_takes_window(op);

        while matches!(self.peek().kind, TokenKind::Comma) {
            self.advance();

            // Trailing-window sugar / explicit lookback — only for windowed ops, and
            // only when the remainder is `$period)` / `N)` / `lookback, lookback)`.
            if windowed && window.is_none() {
                if let Some(w) = self.try_parse_trailing_or_lookback()? {
                    window = Some(w);
                    break;
                }
            }

            args.push(self.parse_expr()?);
        }

        if !matches!(self.peek().kind, TokenKind::RParen) {
            return Err(self.err("expected `)`", Some(self.peek().pos)));
        }
        self.advance();

        Ok(Expr::Call {
            op,
            args,
            window,
            pos,
        })
    }

    /// After a comma following the primary arg, try trailing sugar or lookback pair.
    fn try_parse_trailing_or_lookback(&mut self) -> Result<Option<WindowSpec>, Error> {
        // `$IDENT` trailing sugar (and nothing else before `)`)
        if matches!(self.peek().kind, TokenKind::Dollar) {
            let save = self.i;
            let pos = self.peek().pos;
            self.advance();
            let name_tok = self.advance();
            let name = match name_tok.kind {
                TokenKind::Ident(s) => s,
                _ => {
                    self.i = save;
                    return Ok(None);
                }
            };
            // If next is `)` → trailing sugar. If `,` → could be start of something else; treat as param expr.
            if matches!(self.peek().kind, TokenKind::RParen) {
                return Ok(Some(WindowSpec::Trailing {
                    period: TrailingPeriod::Param { name, pos },
                }));
            }
            // Not trailing sugar alone — rewind and let caller parse as expr.
            self.i = save;
            return Ok(None);
        }

        // Bare INT trailing sugar when followed by `)`
        if let TokenKind::Int(v) = self.peek().kind {
            let save = self.i;
            let pos = self.peek().pos;
            let value = v;
            self.advance();
            if matches!(self.peek().kind, TokenKind::RParen) {
                return Ok(Some(WindowSpec::Trailing {
                    period: TrailingPeriod::Int { value, pos },
                }));
            }
            self.i = save;
            return Ok(None);
        }

        // Lookback bound pair: lookback , lookback
        if self.is_lookback_start() {
            let start = self.parse_lookback_bound()?;
            if !matches!(self.peek().kind, TokenKind::Comma) {
                return Err(self.err(
                    "expected `,` between lookback bounds",
                    Some(self.peek().pos),
                ));
            }
            self.advance();
            let end = self.parse_lookback_bound()?;
            return Ok(Some(WindowSpec::Explicit { start, end }));
        }

        Ok(None)
    }

    fn is_lookback_start(&self) -> bool {
        matches!(&self.peek().kind, TokenKind::Ident(s) if s == "t" || s == "t0")
    }

    fn parse_lookback_bound(&mut self) -> Result<LookbackBound, Error> {
        let tok = self.advance();
        match tok.kind {
            TokenKind::Ident(s) if s == "t0" => Ok(LookbackBound::T0),
            TokenKind::Ident(s) if s == "t" => {
                if matches!(self.peek().kind, TokenKind::Minus) {
                    self.advance();
                    let additive = self.parse_additive_lookback()?;
                    Ok(LookbackBound::TMinus(Box::new(additive)))
                } else {
                    Ok(LookbackBound::T)
                }
            }
            _ => Err(self.err(
                "expected lookback bound `t`, `t0`, or `t-…`",
                Some(tok.pos),
            )),
        }
    }

    /// `additive = INT | "(" expr ")" | "$" IDENT`
    fn parse_additive_lookback(&mut self) -> Result<Expr, Error> {
        match &self.peek().kind {
            TokenKind::Int(v) => {
                let pos = self.peek().pos;
                let value = *v as f64;
                self.advance();
                Ok(Expr::Literal {
                    value,
                    is_int: true,
                    pos,
                })
            }
            TokenKind::LParen => {
                self.advance();
                let e = self.parse_expr()?;
                if !matches!(self.peek().kind, TokenKind::RParen) {
                    return Err(self.err("expected `)` in lookback additive", Some(self.peek().pos)));
                }
                self.advance();
                Ok(e)
            }
            TokenKind::Dollar => self.parse_param_expr(),
            _ => Err(self.err(
                "expected lookback additive: INT, `(expr)`, or `$name`",
                Some(self.peek().pos),
            )),
        }
    }
}

/// Find the byte offset of the first emit-range specification anywhere in
/// `expr` — a series with its own `; domain`, or a postfix `[$from:$to]`.
/// Used to reject a range applied over a subtree that already has one.
fn find_range_spec(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Series(s) => s.domain.as_ref().map(|_| s.pos),
        Expr::Index {
            base,
            selector,
            pos,
        } => {
            if matches!(selector, IndexSelector::Range { .. }) {
                return Some(*pos);
            }
            find_range_spec(base)
        }
        Expr::BinOp { left, right, .. } => find_range_spec(left).or_else(|| find_range_spec(right)),
        Expr::Call { args, window, .. } => {
            for a in args {
                if let Some(p) = find_range_spec(a) {
                    return Some(p);
                }
            }
            // Explicit lookback bounds may embed `(expr)` operands.
            if let Some(WindowSpec::Explicit { start, end }) = window {
                for b in [start, end] {
                    if let LookbackBound::TMinus(inner) = b {
                        if let Some(p) = find_range_spec(inner) {
                            return Some(p);
                        }
                    }
                }
            }
            None
        }
        Expr::Param { .. } | Expr::Literal { .. } => None,
    }
}

fn op_takes_window(op: CallOp) -> bool {
    matches!(
        op,
        CallOp::Avg
            | CallOp::Var
            | CallOp::Std
            | CallOp::Count
            | CallOp::Ema
            | CallOp::Rma
            | CallOp::Rsi
            | CallOp::RegrSlope
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_avg_trailing_sugar() {
        let e = parse_expr("AVG([close.1d; $from:$to], $period)").unwrap();
        match e {
            Expr::Call {
                op: CallOp::Avg,
                window: Some(WindowSpec::Trailing { .. }),
                args,
                ..
            } => match &args[0] {
                Expr::Series(s) => {
                    assert_eq!(s.bucket, "1d");
                    assert!(matches!(s.domain, Some(SeriesDomain::Absolute { .. })));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_latest_domain() {
        let e = parse_expr("AVG([close.1h], 14)").unwrap();
        match e {
            Expr::Call { args, .. } => match &args[0] {
                Expr::Series(s) => {
                    assert_eq!(s.bucket, "1h");
                    assert!(s.domain.is_none());
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_trailing_bars_ending_at_param() {
        let e = parse_expr("AVG([close.1d; 100@$end], 14)").unwrap();
        match e {
            Expr::Call { args, .. } => match &args[0] {
                Expr::Series(s) => match &s.domain {
                    Some(SeriesDomain::TrailingBars {
                        count: EmitCount::Int { value: 100, .. },
                        end: EmitEnd::Param { name, .. },
                        ..
                    }) => assert_eq!(name, "end"),
                    other => panic!("{other:?}"),
                },
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_trailing_bars_ending_latest() {
        let e = parse_expr("AVG([close.1d; $n@latest], $period)").unwrap();
        match e {
            Expr::Call { args, .. } => match &args[0] {
                Expr::Series(s) => {
                    assert!(matches!(
                        s.domain,
                        Some(SeriesDomain::TrailingBars {
                            count: EmitCount::Param { .. },
                            end: EmitEnd::Latest { .. },
                            ..
                        })
                    ));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_explicit_lookback() {
        let e = parse_expr("AVG([close.1d; $from:$to], t-($period-1), t)").unwrap();
        match e {
            Expr::Call {
                window: Some(WindowSpec::Explicit { .. }),
                ..
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_missing_bucket() {
        let err = parse_expr("AVG([close; $from:$to], $period)").unwrap_err();
        assert!(err.message.contains("bucket required"));
    }

    #[test]
    fn parses_asset_qualifier() {
        let e = parse_expr("RET([close.1d@TOTALCRYPTOMARKETCAP; $from:$to])").unwrap();
        match e {
            Expr::Call { args, .. } => match &args[0] {
                Expr::Series(s) => {
                    assert_eq!(s.asset, AssetRef::Literal("TOTALCRYPTOMARKETCAP".into()));
                    assert_eq!(s.bucket, "1d");
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_regr_alias() {
        let e = parse_expr(
            "REGR(RET([close.1h@self; $from:$to]), RET([close.1h@$benchmark; $from:$to]), 31)",
        )
        .unwrap();
        match e {
            Expr::Call {
                op: CallOp::RegrSlope,
                args,
                window: Some(WindowSpec::Trailing {
                    period: TrailingPeriod::Int { value: 31, .. },
                }),
                ..
            } => assert_eq!(args.len(), 2),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_postfix_range() {
        let e = parse_expr("AVG([close.1d], 14)[$from:$to]").unwrap();
        match e {
            Expr::Index {
                selector: IndexSelector::Range { from, to },
                ..
            } => {
                assert_eq!(from.name, "from");
                assert_eq!(to.name, "to");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_nested_range_at_parse() {
        // Inner series range + outer postfix range is a syntax error.
        let err = parse_expr(
            "REGR(RET([close.1d@self; $from:$to]), RET([close.1d@$b]), 31)[$from:$to]",
        )
        .unwrap_err();
        assert_eq!(err.code.as_str(), "parse_error");
        assert!(err.message.contains("only one level"), "{}", err.message);
    }

    #[test]
    fn parses_self_qualifier() {
        let e = parse_expr("RET([close.1h@self; $from:$to])").unwrap();
        match e {
            Expr::Call { args, .. } => match &args[0] {
                Expr::Series(s) => assert_eq!(s.asset, AssetRef::SelfRow),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_batch() {
        let b = parse_batch(
            "{ sma_14: AVG([close.1d; $from:$to], 14), ema_14: EMA([close.1d; $from:$to], 14) }",
        )
        .unwrap();
        match b {
            BatchExpr::Batch(m) => {
                assert!(m.contains_key("sma_14"));
                assert!(m.contains_key("ema_14"));
            }
            _ => panic!("expected batch"),
        }
    }

    #[test]
    fn parses_batch_postfix_range() {
        let b = parse_batch(
            "{ close: [close.1h], ema: EMA([close.1h], $ema_n) }[$from:$to]",
        )
        .unwrap();
        match b {
            BatchExpr::Batch(m) => {
                for (name, expr) in &m {
                    match expr {
                        Expr::Index {
                            selector: IndexSelector::Range { from, to },
                            ..
                        } => {
                            assert_eq!(from.name, "from", "{name}");
                            assert_eq!(to.name, "to", "{name}");
                        }
                        other => panic!("{name}: {other:?}"),
                    }
                }
            }
            _ => panic!("expected batch"),
        }
    }

    #[test]
    fn rejects_batch_result_slice() {
        let err = parse_batch("{ close: [close.1h] }[-1]").unwrap_err();
        assert_eq!(err.code.as_str(), "parse_error");
        assert!(
            err.message.contains("only an emit-range"),
            "{}",
            err.message
        );
    }

    #[test]
    fn rejects_batch_range_when_member_has_range() {
        let err = parse_batch(
            "{ close: [close.1h; $from:$to], ema: EMA([close.1h], 14) }[$from:$to]",
        )
        .unwrap_err();
        assert_eq!(err.code.as_str(), "parse_error");
        assert!(err.message.contains("only one level"), "{}", err.message);
        assert!(err.message.contains("close"), "{}", err.message);
    }

    #[test]
    fn parses_result_index_and_slice() {
        let e = parse_expr("AVG([close.1d], 14)[-1]").unwrap();
        match e {
            Expr::Index {
                selector: IndexSelector::Index(-1),
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        let e = parse_expr("AVG([close.1d], 14)[-10:-1]").unwrap();
        match e {
            Expr::Index {
                selector: IndexSelector::Slice {
                    start: Some(-10),
                    end: Some(-1),
                },
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        let e = parse_expr("AVG([close.1d], 14)[4:10]").unwrap();
        match e {
            Expr::Index {
                selector: IndexSelector::Slice {
                    start: Some(4),
                    end: Some(10),
                },
                ..
            } => {}
            other => panic!("{other:?}"),
        }
    }
}
