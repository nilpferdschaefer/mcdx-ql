//! Abstract syntax tree for the indicator grammar.

use std::collections::BTreeMap;

/// Top-level request body: a single expression or a named batch.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchExpr {
    Single(Expr),
    Batch(BTreeMap<String, Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Series(Series),
    Call {
        op: CallOp,
        args: Vec<Expr>,
        /// Trailing-window sugar: `AVG(x, $period)` → equivalent lookback bounds.
        window: Option<WindowSpec>,
        /// Source byte offset of the op name (for errors).
        pos: usize,
    },
    BinOp {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// External `$name` parameter reference.
    Param {
        name: String,
        pos: usize,
    },
    /// Numeric literal.
    Literal {
        value: f64,
        /// True when the source token was an integer (no decimal point).
        is_int: bool,
        pos: usize,
    },
}

/// Window form attached to a call.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowSpec {
    /// Explicit `t-…, t` / `t0` lookback bounds.
    Explicit {
        start: LookbackBound,
        end: LookbackBound,
    },
    /// Sugar `OP(expr, $period)` or `OP(expr, 14)` ≡ `t-(N-1), t`.
    Trailing { period: TrailingPeriod },
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrailingPeriod {
    Param { name: String, pos: usize },
    Int { value: i64, pos: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    pub name: String,
    pub asset: AssetRef,
    pub from: DomainBound,
    pub to: DomainBound,
    pub pos: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetRef {
    /// Default: row asset from `params.coins`.
    Row,
    /// Literal ticker: `[close@TOTALCRYPTOMARKETCAP; …]`.
    Literal(String),
    /// Param ticker: `[close@$benchmark; …]`.
    Param(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainBound {
    pub name: String,
    pub pos: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LookbackBound {
    /// Current bar `t`.
    T,
    /// Segment start `t0`.
    T0,
    /// `t - additive`.
    TMinus(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl BinOp {
    pub fn as_sql(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallOp {
    Avg,
    Var,
    Std,
    Count,
    Ret,
    Tr,
    Ema,
    Rma,
    Rsi,
    RegrSlope,
    Sqrt,
    Greatest,
    Power,
    Abs,
}

impl CallOp {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "AVG" => Self::Avg,
            "VAR" => Self::Var,
            "STD" => Self::Std,
            "COUNT" => Self::Count,
            "RET" => Self::Ret,
            "TR" => Self::Tr,
            "EMA" => Self::Ema,
            "RMA" => Self::Rma,
            "RSI" => Self::Rsi,
            "REGR_SLOPE" => Self::RegrSlope,
            "SQRT" => Self::Sqrt,
            "GREATEST" => Self::Greatest,
            "POWER" => Self::Power,
            "ABS" => Self::Abs,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Avg => "AVG",
            Self::Var => "VAR",
            Self::Std => "STD",
            Self::Count => "COUNT",
            Self::Ret => "RET",
            Self::Tr => "TR",
            Self::Ema => "EMA",
            Self::Rma => "RMA",
            Self::Rsi => "RSI",
            Self::RegrSlope => "REGR_SLOPE",
            Self::Sqrt => "SQRT",
            Self::Greatest => "GREATEST",
            Self::Power => "POWER",
            Self::Abs => "ABS",
        }
    }
}
