//! Recursive-descent parser for the indicator grammar.

use std::collections::BTreeMap;

use crate::ast::{
    AssetRef, BatchExpr, BinOp, CallOp, DomainBound, Expr, LookbackBound, Series, SeriesDomain,
    TrailingPeriod, WindowSpec,
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
            Ok(BatchExpr::Batch(self.parse_batch_map()?))
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
        let mut left = self.parse_primary()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let right = self.parse_primary()?;
            left = Expr::BinOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
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

        // Optional absolute domain `; $from:$to`. Omitted → latest available bar.
        let domain = if matches!(self.peek().kind, TokenKind::Semi) {
            self.advance(); // ;
            let from = self.parse_domain_bound()?;
            if !matches!(self.peek().kind, TokenKind::Colon) {
                return Err(self.err("expected `:` between domain bounds", Some(self.peek().pos)));
            }
            self.advance();
            let to = self.parse_domain_bound()?;
            Some(SeriesDomain { from, to })
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
                    assert!(s.domain.is_some());
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
}
