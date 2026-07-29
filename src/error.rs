//! Error types for lex / parse / compile failures.

use thiserror::Error;

/// Machine-readable error codes (aligned with read-api error envelope).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ParseError,
    SemError,
    CompileError,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "parse_error",
            Self::SemError => "sem_error",
            Self::CompileError => "compile_error",
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub expr: String,
    pub pos: Option<usize>,
}

impl Error {
    pub fn parse(message: impl Into<String>, expr: impl Into<String>, pos: Option<usize>) -> Self {
        Self {
            code: ErrorCode::ParseError,
            message: message.into(),
            expr: expr.into(),
            pos,
        }
    }

    pub fn sem(message: impl Into<String>, expr: impl Into<String>, pos: Option<usize>) -> Self {
        Self {
            code: ErrorCode::SemError,
            message: message.into(),
            expr: expr.into(),
            pos,
        }
    }

    pub fn compile(message: impl Into<String>, expr: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::CompileError,
            message: message.into(),
            expr: expr.into(),
            pos: None,
        }
    }

    /// JSON error body matching the read-api failure shape (without `ok: false` wrapper).
    pub fn to_error_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "code": self.code.as_str(),
            "message": self.message,
            "expr": self.expr,
        });
        if let Some(pos) = self.pos {
            obj["pos"] = serde_json::json!(pos);
        }
        obj
    }
}
