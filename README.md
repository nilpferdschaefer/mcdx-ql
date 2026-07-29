# mcdx-ql

Compile MCDX indicator expression grammar into the joint-analytics SQL envelope that runs against `core.data`.

This crate is **SQL generation only** — no HTTP, no DB I/O. The datastore read-api (or analytics) owns execution and the response envelope (`ok`, `rows`, pagination).

## Quick start

```rust
use std::collections::BTreeMap;
use mcdx_ql::{compile, CompileRequest, ParamValue};

let q = compile(&CompileRequest {
    expr: "AVG([close.1d; $from:$to], $period)".into(),
    reporting_period: None, // bucket comes from `.1d` in the grammar
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
// q.binds — coins, dirty_from, dirty_to, after_ts, lim, publish_from, max_lookback, indicators
//           (dirty_* are NULL when domain is omitted → latest bar)
```

## Grammar (summary)

| Form | Example |
|------|---------|
| Series + bucket | `[close.1d]`, `[close.1h]`, `[close.5m]` |
| Series + bucket + domain | `[close.1d; $from:$to]` |
| Latest (no domain) | `[close.1d]` → emit the latest available bar |
| Series @ asset | `[close.1d@TOTALCRYPTOMARKETCAP; $from:$to]`, `[close.1d@$benchmark]` |
| Trailing window sugar | `AVG([close.1d; $from:$to], $period)` ≡ `AVG(…, t-($period-1), t)` |
| Lookback | `t`, `t0`, `t-INT`, `t-($period-1)` |
| Batch | `{ sma_14: AVG([close.1d], 14), ema_14: EMA([close.1d], 14) }` |

**Rules**

- Bucket (`.1d` / `.1h` / …) is **required** on every series; all buckets in one expr must match.
- Domain `; $from:$to` is optional; omit it to evaluate the latest available `timestamp_start`.
- When present, all absolute domains must resolve to the same `[$from,$to]`; cannot mix latest + absolute.
- Bare series names / bare identifiers are illegal — series need `[]`, params need `$`.
- `$name` values come only from request `params` (missing → fail loud).
- Request-level `dirty_from` / `dirty_to` and conflicting `reporting_period` are rejected (grammar owns domain + bucket).
- `t` / `t0` are illegal inside the domain slot.

### Builtins

`AVG` `VAR` `STD` `COUNT` `RET` `TR` `EMA` `RMA` `RSI` `REGR_SLOPE` `SQRT` `GREATEST` `POWER` `ABS`

### Warmup (derived)

| Form | Warmup |
|------|--------|
| Trailing `$period` | `COUNT(*) >= $period` |
| `EMA(…, $period)` | `array_length(closes_to_date, 1) >= $period` |
| `RMA(TR(…), $period)` / `RSI(…, $period)` | `array_length(closes_to_date, 1) >= $period+1` |

Version is `MAX(version)` over the same frame as the primary window (or `GREATEST` of frames for multi-window exprs).

### Worked analytics stems (§4.5)

| Stem | Expr |
|------|------|
| `sma_14` | `AVG([close.1d; $from:$to], $period)` |
| `vol_96` | `STD(RET([close.1d; $from:$to]), $period) * SQRT($bars_per_year)` |
| `ema_48` | `EMA([close.1d; $from:$to], $period)` |
| `atr_14` | `RMA(TR([close.1d; $from:$to]), $period)` |
| `rsi_14` | `RSI([close.1d; $from:$to], $period)` |
| `sep_atr` | `(AVG(…, $fast) - AVG(…, $slow)) / RMA(TR(…), $atr)` |
| `beta_31` | `REGR_SLOPE(RET([close.1d; …]), RET([close.1d@$benchmark; …]), $period)` |

`bb_14` (object mid/upper/lower) is deferred.

## Output shape

`CompiledQuery` includes:

- `sql` — CTE pipeline (`params` → `bounds` → `ordered` → `enriched` → optional `market_ret` → `windowed` → `unpivoted` → `ranked`)
- `binds` — eight positional parameters; `dirty_from`/`dirty_to` are `NULL` in latest mode
- `reporting_period` — derived from series bucket (e.g. `1d`)
- `domain` — `Absolute { from_ms, to_ms, … }` or `Latest`
- `max_lookback` — pads scan before `emit_from`
- `scaffolds` / `indicators` — as before

SQL columns → response fields (§4.6): `coin`→`asset`, plus `timestamp_*` / `value` / `version` / `warmup_complete`. Use `map_sql_row` for scalar-vs-object discrimination. Null computed values are filtered in SQL (`WHERE u.value IS NOT NULL`).

## Errors

```rust
err.to_error_json()
// { "code": "parse_error"|"sem_error"|"compile_error", "message": "...", "expr": "...", "pos": 4 }
```

## Using from other `nilpferdschaefer` repos

This crate is **not** published to crates.io. Sibling private repos should depend via git (CI publishes versioned bundles + rustdoc).

### Prefer: git dependency

```toml
[dependencies]
mcdx_ql = { git = "https://github.com/nilpferdschaefer/mcdx-ql", tag = "v0.1.0" }
# or track main:
# mcdx_ql = { git = "https://github.com/nilpferdschaefer/mcdx-ql", branch = "main" }
```

Private git deps need a credential with read access (SSH deploy key, `GITHUB_TOKEN` / `GH_TOKEN` with `contents: read`, or `CARGO_NET_GIT_FETCH_WITH_CLI=true` + `gh` auth).

### Local artifact (crate + rustdoc)

Every CI run on `main` / PRs and every `v*.*.*` tag uploads
`mcdx_ql-<version>-bundle.tar.gz` containing:

| Path | Contents |
|------|----------|
| `mcdx_ql-<version>.crate` | `cargo package` output |
| `docs/` | rustdoc HTML — open `docs/mcdx_ql/index.html` |

```bash
# from a workflow artifact or GitHub Release asset
tar xzf mcdx_ql-0.1.0-bundle.tar.gz
open mcdx_ql-0.1.0/docs/mcdx_ql/index.html

# optional path dependency after unpacking the .crate
cargo unpack mcdx_ql-0.1.0/mcdx_ql-0.1.0.crate --output vendor
# Cargo.toml: mcdx_ql = { path = "vendor/mcdx_ql-0.1.0" }
```

### Docs site

On push to `main` and on version tags, rustdoc is deployed to GitHub Pages:

https://nilpferdschaefer.github.io/mcdx-ql/

Enable once under **Settings → Pages → Build and deployment → GitHub Actions** (private Pages requires an eligible GitHub plan).

### Cutting a release

1. Bump `version` in `Cargo.toml`
2. Tag `vX.Y.Z` matching that version and push the tag
3. `Release` workflow attaches the bundle + `.crate` to the GitHub Release and refreshes Pages

## Develop

```bash
cargo test
cargo doc --open --no-deps
```
