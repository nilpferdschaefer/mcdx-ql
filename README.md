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
//           (dirty_* are NULL when emit bounds come from available data / slices)
```

## Grammar (summary)

| Form | Example |
|------|---------|
| Series + bucket | `[close.1d]`, `[close.1h]`, `[close.5m]` |
| Absolute emit range | `[close.1d; $from:$to]` → one row per bar in range |
| Trailing N bars to date | `[close.1d; 100@$end]` → 100 bars ending at `$end` (inclusive) |
| Trailing N bars to latest | `[close.1d; 100@latest]` / `[close.1d; $n@latest]` |
| Full series (no domain) | `[close.1d]` → largest possible result series from available data |
| Result index / slice | `AVG([close.1d], 14)[-1]`, `…[4]`, `…[-10:-1]`, `…[4:10]` |
| Series @ asset | `[close.1d@self]`, `[close.1d@ETH; 100@$end]`, `[close.1d@$benchmark]` |
| Inherited range (postfix) | `REGR(RET([close.1h@self]), RET([close.1h@$b]), 31)[$from:$to]` |
| Batch inherited range | `{ close: [close.1h], ema: EMA([close.1h], 14) }[$from:$to]` |
| Trailing window sugar | `AVG([close.1d; 100@$end], $period)` ≡ `AVG(…, t-($period-1), t)` |
| Lookback | `t`, `t0`, `t-INT`, `t-($period-1)` |
| Batch | `{ sma_14: AVG([close.1d], 14)[-1], … }` |

**Array-default results + slices**

Timeseries ops return the **largest possible result series** from the source data (e.g. N closes and an M-period SMA → **N−M+1** values). Reduce with postfix indexing (inclusive slices; positives are **1-based**; negatives count from the end):

| Slice | Meaning |
|-------|---------|
| `expr[-1]` | last value |
| `expr[4]` | 4th possible value |
| `expr[-10:-1]` | last 10 values |
| `expr[4:10]` / `expr[:5]` / `expr[-10:]` | inclusive ranges; open ends allowed |

The compiler restricts SQL emit bounds so trailing slices like `[-1]` do not scan more history than needed (beyond lookback padding).

**Emit domain vs lookback**

- **Emit domain** (`$from:$to`, `N@$end`, `N@latest`, a postfix `expr[$from:$to]`, omitted=full, or a result slice) chooses which bars appear in the result — backpopulate a whole range in **one** query.
- **Lookback** (`$period` / `t-(N-1), t`) is the window used **at each emit bar** (e.g. 31 for beta).

Example — correlation of BTC vs ETH for 100 daily bars ending 15 May 2026:

```text
REGR(
  RET([close.1d@self; 100@$end]),
  RET([close.1d@ETH; 100@$end]),
  $period
)
```

with `assets=["BTC"]`, `params.end` = bar-open ms for 2026-05-15, `params.period=31`. Returns up to 100 rows (then `limit` / `after_ts`). `@self` is the row asset (`BTC`); it must be explicit because the expression also references `@ETH`.

**Inherited range (postfix `[$from:$to]`)**

Instead of repeating the range on every series, you can apply it once as a
postfix at any expression level; every descendant series inherits it:

```text
REGR(
  RET([close.1h@self]),
  RET([close.1h@$benchmark]),
  31
)[$from:$to]
```

is exactly equivalent to writing `; $from:$to` inside each series. The range is
distinguished from integer result slices (`[-1]`, `[4:10]`) by the leading `$`.

The same postfix may follow a **batch** so every member shares one emit range:

```text
{
  close: [close.1h@self],
  vol:   STD(RET([close.1h@self]), $vol_n) * SQRT($bars_per_year),
  beta:  REGR(RET([close.1h@self]), RET([close.1h@$benchmark]), $beta_n),
  ema:   EMA([close.1h@self], $ema_n)
}[$from:$to]
```

(`@self` is required on every series once any member references another asset,
e.g. `$benchmark` in `beta`.)

A range may be specified at **only one level** along any path: if a parent
`[$from:$to]` is present (including a batch-level postfix), a descendant series
(or nested `[$from:$to]`) may not also declare one. Doing so is a syntax error
pointing at the conflict.

**Rules**

- Bucket (`.1d` / `.1h` / …) is **required** on every series; all buckets in one expr must match.
- Domain is optional; omit it for the full possible series. Use `[-1]` for the latest value, or `N@$end` / `N@latest` for backfills.
- All series domains (and result slices) in one expr must resolve to the same emit range.
- A postfix `expr[$from:$to]` (or `{ … }[$from:$to]` on a batch) sets the emit range for the whole subtree; descendants inherit it and may not also declare a range (enforced at parse time).
- Bare series names / bare identifiers are illegal — series need `[]`, params need `$`.
- Once any series is `@`-qualified, every series must be qualified (`@self` / `@TICKER` / `@$name`); mixing an implicit row series with a qualified one is rejected.
- `$name` values come only from request `params` (missing → fail loud).
- Request-level `dirty_from` / `dirty_to` and conflicting `reporting_period` are rejected.
- `t` / `t0` are illegal inside the domain slot.

### Builtins

`AVG` `VAR` `STD` `COUNT` `RET` `TR` `EMA` `RMA` `RSI` `REGR` (alias `REGR_SLOPE`) `SQRT` `GREATEST` `POWER` `ABS`

**Regression (`REGR`)**

`REGR(y, x, $period)` is the trailing `$period`-bar linear-regression slope of `y`
on `x` (the classic beta). `REGR_SLOPE` is kept as an alias. Both compile to the
Postgres `REGR_SLOPE(y, x)` window aggregate.

```text
REGR(
  RET([close.1h@self; $from:$to]),       -- row asset returns  (y)
  RET([close.1h@$benchmark; $from:$to]), -- benchmark returns  (x)
  31
)
```

is the 31-period 1h beta of the request asset against `$benchmark`.

**Explicit qualification for multi-asset expressions.** Whenever an expression
references more than one asset — i.e. any series is `@`-qualified — **every**
series must be qualified after `@`, so the comparison is unambiguous. Qualify the
per-row request asset with `@self`, a literal ticker with `@TICKER`, or a param
with `@$name`. Mixing an implicit (unqualified) row series with an `@`-qualified
series is rejected. Each distinct qualified ticker gets its own `market_ret` CTE,
so you can also regress two explicit tickers, e.g.
`REGR(RET([close.1h@BTC; …]), RET([close.1h@ETH; …]), 31)`.

Single-asset expressions are unchanged: a bare `[close.1d]` (implicit row asset)
is fine as long as no other asset is referenced, so indicators that reuse the row
close several times (e.g. `sep_atr`) need no `@self`.

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
| `beta_31` | `REGR(RET([close.1d@self; …]), RET([close.1d@$benchmark; …]), $period)` |

`bb_14` (object mid/upper/lower) is deferred.

## Output shape

`CompiledQuery` includes:

- `sql` — CTE pipeline (`params` → `bounds` → `ordered` → `enriched` → optional `market_ret` → `windowed` → `unpivoted` → `ranked`)
- `binds` — eight positional parameters; `dirty_from`/`dirty_to` are set only for absolute domains
- `reporting_period` — derived from series bucket (e.g. `1d`)
- `domain` — `Absolute` / `Full` / `TrailingLatest` / `FromStart`
- `max_lookback` — pads scan before `emit_from`
- `scaffolds` / `indicators` — as before

SQL columns → response fields (§4.6): `coin`→`asset`, plus `timestamp_*` / `value` / `version` / `warmup_complete`. Use `map_sql_row` for scalar-vs-object discrimination. Null computed values are filtered in SQL (`WHERE u.value IS NOT NULL`).

## Errors

```rust
err.to_error_json()
// { "code": "parse_error"|"sem_error"|"compile_error", "message": "...", "expr": "...", "pos": 4 }
```

## Using from other `nilpferdschaefer` repos

This crate is **not** published to crates.io (GitHub Packages has no Cargo registry). Sibling repos should depend via **git** (Rust) or **GitHub Packages Maven** (Java). CI also uploads workflow/release bundles and rustdoc/Javadoc to Pages.

### GitHub Packages (Maven — JAR + crate together)

[Publish GitHub Packages](.github/workflows/publish-packages.yml) deploys one Maven package `com.nilpferdschaefer:mcdx-ql` containing:

| Artifact | Classifier | Contents |
|----------|------------|----------|
| `mcdx-ql-<ver>.jar` | (default) | Java bindings + JNI |
| `mcdx-ql-<ver>-javadoc.jar` | `javadoc` | Javadoc |
| `mcdx-ql-<ver>-crate.crate` | `crate` | Rust `cargo package` output |

Registry: `https://maven.pkg.github.com/nilpferdschaefer/mcdx-ql`

| Trigger | Published version | Overwrite? |
|---------|-------------------|------------|
| `main` / `workflow_dispatch` | `X.Y.Z-SNAPSHOT` | yes (dev) |
| tag `vX.Y.Z` | `X.Y.Z` | no (immutable release) |

```xml
<repositories>
  <repository>
    <id>github</id>
    <url>https://maven.pkg.github.com/nilpferdschaefer/mcdx-ql</url>
  </repository>
</repositories>

<dependency>
  <groupId>com.nilpferdschaefer</groupId>
  <artifactId>mcdx-ql</artifactId>
  <!-- dev: -->
  <version>0.1.0-SNAPSHOT</version>
  <!-- release: <version>0.1.0</version> -->
</dependency>
```

Authenticate with a PAT (or `GITHUB_TOKEN` in Actions) that has `read:packages` — put it in `~/.m2/settings.xml` under server id `github` (username = your GitHub login).

Package UI: https://github.com/nilpferdschaefer/mcdx-ql/packages

GitHub Packages is **not** a Cargo registry. For Cargo builds use a git dependency; grab the attached `-crate.crate` from Packages only if you want to vendor/download the packed crate next to the JAR.

### Rust: git dependency

```toml
[dependencies]
mcdx_ql = { git = "https://github.com/nilpferdschaefer/mcdx-ql", tag = "v0.1.0" }
# or track main:
# mcdx_ql = { git = "https://github.com/nilpferdschaefer/mcdx-ql", branch = "main" }
```

Private git deps need a credential with read access (SSH deploy key, `GITHUB_TOKEN` / `GH_TOKEN` with `contents: read`, or `CARGO_NET_GIT_FETCH_WITH_CLI=true` + `gh` auth).

### Java API

```java
import com.nilpferdschaefer.mcdxql.McdxQl;

String response = McdxQl.compile(
    "{"
        + "\"expr\":\"AVG([close.1d; $from:$to], $period)\","
        + "\"assets\":[\"BTC\",\"ETH\"],"
        + "\"params\":{\"period\":14,\"from\":1700000000000,\"to\":1700086400000}"
        + "}");
// {"ok":true,"sql":"WITH params AS ...", "binds":[...], ...}
```

Build locally (needs JDK 11+ + Maven):

```bash
./scripts/build-jar.sh
# → java/target/mcdx-ql-0.1.0.jar  (embeds native/linux-x86_64/libmcdx_ql.so on Linux CI/dev)
```

The JAR currently embeds the **linux-x86_64** native library from the Ubuntu builder; other platforms can run `./scripts/build-jar.sh` on that host.

### Local artifact bundle

Every CI run on `main` / PRs and every `v*.*.*` tag uploads
`mcdx_ql-<version>-bundle.tar.gz` containing:

| Path | Contents |
|------|----------|
| `mcdx_ql-<version>.crate` | `cargo package` output |
| `docs/` | rustdoc HTML — open `docs/mcdx_ql/index.html` |
| `java/mcdx-ql-<version>.jar` | Java bindings + embedded JNI lib |
| `java/mcdx-ql-<version>-javadoc.jar` | Javadoc JAR |
| `java/javadoc/` | Javadoc HTML |
| `java/native/` | raw native libs |

```bash
# from a workflow artifact or GitHub Release asset
tar xzf mcdx_ql-0.1.0-bundle.tar.gz
open mcdx_ql-0.1.0/docs/mcdx_ql/index.html
java -cp mcdx_ql-0.1.0/java/mcdx-ql-0.1.0.jar com.nilpferdschaefer.mcdxql.SmokeTest
```

### Docs site

On push to `main` and on version tags, docs are deployed to GitHub Pages:

https://nilpferdschaefer.github.io/mcdx-ql/

- Rust: `/mcdx_ql/`
- Java: `/javadoc/`

Enable once under **Settings → Pages → Build and deployment → GitHub Actions** (private Pages requires an eligible GitHub plan).

### Cutting a release

1. Bump `version` in `Cargo.toml` (and keep `java/pom.xml` in sync — `build-jar.sh` patches it)
2. Tag `vX.Y.Z` matching that version and push the tag
3. `Release` workflow attaches the bundle + JAR to the GitHub Release and refreshes Pages
4. `Publish GitHub Packages` deploys immutable `com.nilpferdschaefer:mcdx-ql:X.Y.Z`

## Develop

```bash
cargo test
cargo test --features jni
cargo doc --open --no-deps
./scripts/build-jar.sh          # JAR + smoke test
./scripts/package-bundle.sh     # dist/mcdx_ql-*-bundle.tar.gz
```
