//! JSON wire API for FFI / JNI consumers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::compile::{compile, BindValue, CompileRequest, CompiledQuery, Scaffolds};
use crate::error::Error;
use crate::sem::{Domain, ParamValue};

/// JSON request matching [`CompileRequest`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompileRequestJson {
    pub expr: String,
    #[serde(default)]
    pub reporting_period: Option<String>,
    pub assets: Vec<String>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamValueJson>,
    #[serde(default = "default_after_ts")]
    pub after_ts: i64,
    #[serde(default = "default_limit")]
    pub limit: i32,
    #[serde(default)]
    pub publish_from: Option<i64>,
    /// Series stems stored in `obj` (object/candle values). Optional; the
    /// caller resolves these from `series_slot`. Empty = all scalar.
    #[serde(default)]
    pub obj_data_types: Vec<String>,
    /// Series stems stored in the scalar `data` fact table (`kind='data'`),
    /// resolved from `series_slot` scoped to the requested assets. Optional; a
    /// stem here (other than `close`) compiles to a raw `data` fetch. Empty =
    /// only `close` is a valid scalar series.
    #[serde(default)]
    pub scalar_data_types: Vec<String>,
}

fn default_after_ts() -> i64 {
    -1
}
fn default_limit() -> i32 {
    16
}

/// JSON param value: bare number, string, or tagged object.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ParamValueJson {
    Int(i64),
    Float(f64),
    Text(String),
}

impl From<ParamValueJson> for ParamValue {
    fn from(v: ParamValueJson) -> Self {
        match v {
            ParamValueJson::Int(i) => ParamValue::Int(i),
            ParamValueJson::Float(f) => ParamValue::Float(f),
            ParamValueJson::Text(s) => ParamValue::Text(s),
        }
    }
}

impl From<&ParamValue> for ParamValueJson {
    fn from(v: &ParamValue) -> Self {
        match v {
            ParamValue::Int(i) => ParamValueJson::Int(*i),
            ParamValue::Float(f) => ParamValueJson::Float(*f),
            ParamValue::Text(s) => ParamValueJson::Text(s.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BindValueJson {
    TextArray { value: Vec<String> },
    BigInt { value: i64 },
    Null,
    Int { value: i32 },
    Text { value: String },
}

impl From<&BindValue> for BindValueJson {
    fn from(v: &BindValue) -> Self {
        match v {
            BindValue::TextArray(a) => BindValueJson::TextArray { value: a.clone() },
            BindValue::BigInt(i) => BindValueJson::BigInt { value: *i },
            BindValue::Null => BindValueJson::Null,
            BindValue::Int(i) => BindValueJson::Int { value: *i },
            BindValue::Text(s) => BindValueJson::Text { value: s.clone() },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DomainJson {
    Absolute {
        from_param: String,
        to_param: String,
        from_ms: i64,
        to_ms: i64,
    },
    Full,
    TrailingLatest { bars: i32, end_offset: i32 },
    FromStart { start: i32, count: i32 },
}

impl From<&Domain> for DomainJson {
    fn from(d: &Domain) -> Self {
        match d {
            Domain::Absolute {
                from_param,
                to_param,
                from_ms,
                to_ms,
            } => DomainJson::Absolute {
                from_param: from_param.clone(),
                to_param: to_param.clone(),
                from_ms: *from_ms,
                to_ms: *to_ms,
            },
            Domain::Full => DomainJson::Full,
            Domain::TrailingLatest { bars, end_offset } => DomainJson::TrailingLatest {
                bars: *bars,
                end_offset: *end_offset,
            },
            Domain::FromStart { start, count } => DomainJson::FromStart {
                start: *start,
                count: *count,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldsJson {
    pub bar_ret: bool,
    pub closes_to_date: bool,
    pub highs_to_date: bool,
    pub lows_to_date: bool,
    pub market_tickers: Vec<String>,
}

impl From<&Scaffolds> for ScaffoldsJson {
    fn from(s: &Scaffolds) -> Self {
        Self {
            bar_ret: s.bar_ret,
            closes_to_date: s.closes_to_date,
            highs_to_date: s.highs_to_date,
            lows_to_date: s.lows_to_date,
            market_tickers: s.market_tickers.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledQueryJson {
    pub sql: String,
    pub binds: Vec<BindValueJson>,
    pub reporting_period: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub params_hash: String,
    pub expr: String,
    pub domain: DomainJson,
    pub indicators: Vec<String>,
    pub max_lookback: i32,
    pub scaffolds: ScaffoldsJson,
    pub interval_ms: i64,
}

impl From<&CompiledQuery> for CompiledQueryJson {
    fn from(q: &CompiledQuery) -> Self {
        Self {
            sql: q.sql.clone(),
            binds: q.binds.iter().map(BindValueJson::from).collect(),
            reporting_period: q.reporting_period.clone(),
            source: q.source.clone(),
            params_hash: q.params_hash.clone(),
            expr: q.expr.clone(),
            domain: DomainJson::from(&q.domain),
            indicators: q.indicators.clone(),
            max_lookback: q.max_lookback,
            scaffolds: ScaffoldsJson::from(&q.scaffolds),
            interval_ms: q.interval_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorJson {
    pub code: String,
    pub message: String,
    pub expr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos: Option<usize>,
}

impl From<&Error> for ErrorJson {
    fn from(e: &Error) -> Self {
        Self {
            code: e.code.as_str().to_string(),
            message: e.message.clone(),
            expr: e.expr.clone(),
            pos: e.pos,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompileResponseJson {
    Ok {
        ok: bool,
        #[serde(flatten)]
        result: CompiledQueryJson,
    },
    Err {
        ok: bool,
        error: ErrorJson,
    },
}

impl CompileRequestJson {
    pub fn into_request(self) -> CompileRequest {
        CompileRequest {
            expr: self.expr,
            reporting_period: self.reporting_period,
            assets: self.assets,
            params: self
                .params
                .into_iter()
                .map(|(k, v)| (k, ParamValue::from(v)))
                .collect(),
            after_ts: self.after_ts,
            limit: self.limit,
            publish_from: self.publish_from,
            obj_data_types: self.obj_data_types.into_iter().collect(),
            scalar_data_types: self.scalar_data_types.into_iter().collect(),
        }
    }
}

/// Compile from a JSON request body; returns a JSON response envelope.
pub fn compile_json(request_json: &str) -> String {
    let parsed: Result<CompileRequestJson, _> = serde_json::from_str(request_json);
    let req = match parsed {
        Ok(r) => r.into_request(),
        Err(e) => {
            return serde_json::to_string(&CompileResponseJson::Err {
                ok: false,
                error: ErrorJson {
                    code: "parse_error".into(),
                    message: format!("invalid compile request JSON: {e}"),
                    expr: String::new(),
                    pos: None,
                },
            })
            .unwrap_or_else(|_| {
                r#"{"ok":false,"error":{"code":"parse_error","message":"json encode failed","expr":""}}"#
                    .into()
            });
        }
    };

    match compile(&req) {
        Ok(q) => {
            let mut body = serde_json::json!({
                "ok": true,
                "sql": q.sql,
                "binds": q.binds.iter().map(BindValueJson::from).collect::<Vec<_>>(),
                "reporting_period": q.reporting_period,
                "params_hash": q.params_hash,
                "expr": q.expr,
                "domain": DomainJson::from(&q.domain),
                "indicators": q.indicators,
                "max_lookback": q.max_lookback,
                "scaffolds": ScaffoldsJson::from(&q.scaffolds),
                "interval_ms": q.interval_ms,
            });
            if let Some(source) = q.source {
                body["source"] = serde_json::Value::String(source);
            }
            serde_json::to_string(&body).unwrap_or_else(|e| {
                format!(
                    r#"{{"ok":false,"error":{{"code":"compile_error","message":"json encode failed: {e}","expr":""}}}}"#
                )
            })
        }
        Err(e) => serde_json::to_string(&CompileResponseJson::Err {
            ok: false,
            error: ErrorJson::from(&e),
        })
        .unwrap_or_else(|_| {
            r#"{"ok":false,"error":{"code":"compile_error","message":"json encode failed","expr":""}}"#
                .into()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_json_avg() {
        let req = r#"{
            "expr": "AVG([close.1d; $from:$to], $period)",
            "assets": ["BTC"],
            "params": {"period": 14, "from": 100, "to": 200}
        }"#;
        let out = compile_json(req);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true);
        assert!(v["sql"].as_str().unwrap().contains("AVG(e.close)"));
        assert_eq!(v["reporting_period"], "1d");
        assert!(v.get("source").is_none());
        assert_eq!(
            v["params_hash"],
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    #[test]
    fn compile_json_unaggregated_source() {
        let req = r#"{
            "expr": "AVG([binance:close.1d; $from:$to], $period)",
            "assets": ["BTC"],
            "params": {"period": 14, "from": 100, "to": 200}
        }"#;
        let out = compile_json(req);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["source"], "binance");
        assert_eq!(
            v["params_hash"],
            "691508c082c9c6b7be0aaed0f8a914bca6e8b2333ffadd9b297f367d4e83aa87"
        );
        assert!(v["sql"]
            .as_str()
            .unwrap()
            .contains("params_hash = '691508c082c9c6b7be0aaed0f8a914bca6e8b2333ffadd9b297f367d4e83aa87'"));
    }

    #[test]
    fn compile_json_error() {
        let req = r#"{"expr": "AVG([cloze.1d], 14)", "assets": ["BTC"], "params": {"period": 14}}"#;
        let out = compile_json(req);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "sem_error");
    }
}
