# mcdx-ql

Compile MCDX indicator expression grammar into the joint-analytics SQL envelope that runs against `core.data`.

This crate is **SQL generation only** — no HTTP, no DB I/O. The datastore read-api (or analytics) owns execution and the response envelope (`ok`, `rows`, pagination).

## Quick start

```rust
use std::collections::BTreeMap;
use mcdx_ql::{compile, CompileRequest, ParamValue};

let q = compile(&CompileRequest {
    expr: "AVG([close; $from:$to], $period)".into(),
    reporting_period: "1d".into(),
    assets: vec!["BTC".into(), "ETH".into()],
    params: BTreeMap::from([
        ("period".into(), ParamValue::Int(14)),
        ("from".into(), ParamValue::Int(1_700_000_000_000)),
        ("to".into(), ParamValue::Int(1_700_086_400_000)),
    ]),
    after_ts: -1,
    limit: 16,
    publish_from: None,
})?;

// q.sql  — full WITH … SELECT envelope
// q.binds — positional ? binds: coins, dirty_from, dirty_to, after_ts, lim,
//           publish_from, max_lookback, indicators
```

## Grammar (summary)

| Form | Example |
|------|---------|
| Series + domain | `[close; $from:$to]` |
| Series @ asset | `[close@TOTALCRYPTOMARKETCAP; $from:$to]`, `[close@$benchmark; $from:$to]` |
| Trailing window sugar | `AVG([close; $from:$to], $period)` ≡ `AVG(…, t-($period-1), t)` |
| Lookback | `t`, `t0`, `t-INT`, `t-($period-1)` |
| Batch | `{ sma_14: AVG([close; $from:$to], 14), ema_14: EMA([close; $from:$to], 14) }` |

**Rules**

- Domain is required on every series; all domains must resolve to the same `[$from,$to]`.
- Bare series names / bare identifiers are illegal — series need `[]`, params need `$`.
- `$name` values come only from request `params` (missing → fail loud).
- Request-level `dirty_from` / `dirty_to` in `params` are rejected (domain lives in the grammar).
- `t` / `t0` are illegal inside the domain slot.

### Builtins

`AVG` `VAR` `STD` `COUNT` `RET` `TR` `EMA` `RMA` `RSI` `REGR_SLOPE` `SQRT` `GREATEST` `POWER` `ABS`

### Warmup (derived)

| Form | Warmup |
|------|--------|
| Trailing `$period` | `COUNT(*) >= $period` |
| `EMA(…, $period)` | `cardinality(closes_to_date) >= $period` |
| `RMA(TR(…), $period)` / `RSI(…, $period)` | `cardinality(closes_to_date) >= $period+1` |

Version is `MAX(version)` over the same frame as the primary window (or `GREATEST` of frames for multi-window exprs).

## Output shape

`CompiledQuery` includes:

- `sql` — shared CTE pipeline (`params` → `ordered` → `enriched` → optional `market_*` → `windowed` → `unpivoted` → `ranked`)
- `binds` — eight positional parameters matching the `params` CTE
- `domain` — resolved `{from,to}` ms
- `max_lookback` — drives `dirty_from - max_lookback * interval_ms` scan padding
- `scaffolds` — which enriched columns / market CTEs were enabled
- `indicators` — stem names for the `LATERAL VALUES` unpivot (`value` for single expr; batch keys otherwise)

Result rows from SQL are `(coin, indicator, timestamp_start, timestamp_end, value, version, warmup_complete)`. Null computed values are filtered in SQL (`WHERE u.value IS NOT NULL`). Mapping onto the HTTP `rows[]` object (`asset`, scalar `value` xor `object`) is the read-api’s job.

## Errors

```rust
err.to_error_json()
// { "code": "parse_error"|"sem_error"|"compile_error", "message": "...", "expr": "...", "pos": 4 }
```

## Develop

```bash
cargo test
```
