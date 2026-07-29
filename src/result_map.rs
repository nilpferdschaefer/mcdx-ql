//! Result-row field mapping from SQL columns (§4.6).
//!
//! The executor/read-api owns JSON shaping; this module documents the column →
//! response field correspondence and helpers for scalar-vs-object discrimination.

use serde_json::{Map, Value};

/// SQL column names produced by [`crate::compile`].
pub mod sql_columns {
    pub const COIN: &str = "coin";
    pub const INDICATOR: &str = "indicator";
    pub const TIMESTAMP_START: &str = "timestamp_start";
    pub const TIMESTAMP_END: &str = "timestamp_end";
    pub const VALUE: &str = "value";
    pub const VERSION: &str = "version";
    pub const WARMUP_COMPLETE: &str = "warmup_complete";
}

/// One compute row in the read-api success envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorComputeRow {
    pub asset: String,
    pub timestamp_start: i64,
    pub timestamp_end: i64,
    pub value: Option<f64>,
    pub object: Option<Map<String, Value>>,
    pub version: i64,
    pub warmup_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapRowError {
    pub message: String,
}

impl MapRowError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Map a SQL result tuple into an [`IndicatorComputeRow`].
///
/// `value` may be a JSON object encoded as text (deferred BB/MACD/ADX); otherwise
/// it is treated as a scalar. Exactly one of scalar `value` / `object` is set.
pub fn map_sql_row(
    coin: impl Into<String>,
    timestamp_start: i64,
    timestamp_end: i64,
    value: SqlValue,
    version: i64,
    warmup_complete: bool,
) -> Result<IndicatorComputeRow, MapRowError> {
    match value {
        SqlValue::Scalar(v) => Ok(IndicatorComputeRow {
            asset: coin.into(),
            timestamp_start,
            timestamp_end,
            value: Some(v),
            object: None,
            version,
            warmup_complete,
        }),
        SqlValue::ObjectJson(text) => {
            let parsed: Value = serde_json::from_str(&text)
                .map_err(|e| MapRowError::new(format!("invalid object JSON in value: {e}")))?;
            let object = match parsed {
                Value::Object(map) => map,
                _ => {
                    return Err(MapRowError::new(
                        "object payload must be a JSON object",
                    ))
                }
            };
            Ok(IndicatorComputeRow {
                asset: coin.into(),
                timestamp_start,
                timestamp_end,
                value: None,
                object: Some(object),
                version,
                warmup_complete,
            })
        }
        SqlValue::Null => Err(MapRowError::new(
            "null computed values must be omitted (never mapped into rows)",
        )),
    }
}

/// Discriminated SQL `value` cell.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Scalar(f64),
    /// JSON object text (multi-field indicators).
    ObjectJson(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_scalar() {
        let row = map_sql_row("BTC", 1, 2, SqlValue::Scalar(42.0), 3, true).unwrap();
        assert_eq!(row.asset, "BTC");
        assert_eq!(row.value, Some(42.0));
        assert!(row.object.is_none());
    }

    #[test]
    fn maps_object() {
        let row = map_sql_row(
            "BTC",
            1,
            2,
            SqlValue::ObjectJson(r#"{"mid":1.0,"upper":2.0}"#.into()),
            3,
            true,
        )
        .unwrap();
        assert!(row.value.is_none());
        assert!(row.object.unwrap().contains_key("mid"));
    }

    #[test]
    fn rejects_null() {
        assert!(map_sql_row("BTC", 1, 2, SqlValue::Null, 3, true).is_err());
    }
}
