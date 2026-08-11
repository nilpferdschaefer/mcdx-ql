# mcdx-ql function reference

Every builtin operator in the grammar: its arity, whether it takes a trailing
window, and the **SQL it lowers to**. mcdx-ql is SQL-generation only, so this
documents the `value_sql` the compiler emits (`src/compile.rs`). Series,
domains, slices, and batch grammar live in the [README](../README.md); this file
is the operator catalogue.

Parse a name with `CallOp::parse`; the same names are the only bare identifiers
the grammar accepts (everything else needs `[]` for a series or `$` for a param).

## Conventions

- **`x`, `y`** — a subexpression (a `[stem.bucket]` series or a nested op).
- **Trailing window** — `OP(x, $period)` or `OP(x, 14)` frames the last *N* bars
  as `win`; `$period` comes from request params. Windowed ops are marked
  **W** below.
- **Warmup** — a windowed op emits once its frame holds enough bars (noted per
  op). Scalar ops inherit their argument's warmup, version, and period unchanged.
- **Population stats** — `VAR` / `STD` divide by *N*, not *N−1* (see below).

## At a glance

| Op | Args | W | Lowers to | Notes |
|----|------|:-:|-----------|-------|
| `AVG(x, $n)`   | 1 + win | ● | `AVG(x) OVER win` | simple moving average over *N* bars |
| `COUNT(x, $n)` | 1 + win | ● | `COUNT(*) OVER win` | bars present in the frame |
| `VAR(x, $n)`   | 1 + win | ● | `AVG(x*x) OVER win - POWER(AVG(x) OVER win, 2)` | **population** variance (÷ *N*) |
| `STD(x, $n)`   | 1 + win | ● | `SQRT(GREATEST(0, VAR))` | **population** std dev (÷ *N*) |
| `EMA(x, $n)`   | 1 + win | ● | inception-SMA-seeded EMA over closes-to-date | seed = SMA of first *N*; `k = 2/(N+1)` |
| `RMA(TR([close…]), $n)` | 1 + win | ● | Wilder RMA of TR = Wilder ATR | only this form compiles (see below) |
| `RSI([close…], $n)` | 1 + win | ● | Wilder RSI over closes-to-date | `100 − 100/(1 + avg_gain/avg_loss)` |
| `REGR(y, x, $n)` | 2 + win | ● | `REGR_SLOPE(y, x) OVER win` | OLS slope β of *y* on *x*; alias `REGR_SLOPE` |
| `RET(x)`       | 1 | | `x/LAG(x) − 1`, guarded | per-bar simple return (see guards) |
| `TR([close…])` | 1 | | `ABS(close − LAG(close))` | close-to-close true range |
| `SQRT(x)`      | 1 | | `SQRT(x)` | |
| `ABS(x)`       | 1 | | `ABS(x)` | |
| `POWER(a, b)`  | 2 | | `POWER(a, b)` | |
| `GREATEST(a, b, …)` | ≥2 | | `GREATEST(…)` | element-wise max; there is **no** `LEAST` |
| `SIN` `COS` `TAN` `(x)` | 1 | | same-named SQL fn | trigonometric, **radians** |
| `ASIN` `ACOS` `ATAN` `(x)` | 1 | | same-named SQL fn | inverse trigonometric |
| `SINH` `COSH` `TANH` `(x)` | 1 | | same-named SQL fn | hyperbolic — **PostgreSQL 12+** |
| `ASINH` `ACOSH` `ATANH` `(x)` | 1 | | same-named SQL fn | inverse hyperbolic — **PostgreSQL 12+** |
| `+` `−` `*` `/` | binary | | `+` `−` `*` `/` | division by zero **errors** in Postgres — floor denominators |

## Statistics — population, not sample

`VAR` and `STD` are **population** statistics, computed as `E[x²] − E[x]²`:

```sql
VAR(x, $n) →  AVG(x*x) OVER win - POWER(AVG(x) OVER win, 2)
STD(x, $n) →  SQRT(GREATEST(0, AVG(x*x) OVER win - POWER(AVG(x) OVER win, 2)))
```

They divide by *N*, not *N−1*, so they match `STDDEV_POP` (and analytics'
`closeStdevPop`) — not `STDDEV` / `STDDEV_SAMP`. `STD` wraps the variance in
`SQRT(GREATEST(0, …))` so float error near zero variance can't produce a NaN.
Warmup for both: *N* bars in the frame.

## Moving averages

- **`AVG`** — simple moving average of the argument over the trailing *N* bars.
- **`EMA`** — inception EMA over the close-to-date series: seed with the SMA of
  the first *N* values, then recurse `ema = x·k + ema·(1−k)` with `k = 2/(N+1)`.
  Warmup: *N* values. (Matches analytics `emaCloseSql` / Twelve Data.)

## Wilder family

- **`TR([close…])`** — close-to-close true range, `ABS(close − prev_close)`. This
  build uses the close-only TR path (not the high/low/close range). Warmup: one
  prior close.
- **`RMA(TR([close…]), $n)`** — Wilder RMA of true range, i.e. **Wilder ATR**:
  seed with the average TR over bars `2..N+1`, then Wilder smoothing. Only the
  `RMA(TR([close…]), $n)` form compiles; any other `RMA` argument is a compile
  error. Warmup: *N+1* bars.
- **`RSI([close…], $n)`** — Wilder RSI, `100 − 100/(1 + avg_gain/avg_loss)` with
  Wilder-smoothed average gain / loss. Warmup: *N+1* bars.

## Returns

`RET(x)` is the per-bar simple return `x / prev(x) − 1`, guarded to NULL when:

- the previous value is NULL or `≤ 0`,
- the current value is `≤ 0`, or
- the magnitude exceeds `5` (a 500 %+ move — treated as an outlier).

Two fast paths bypass the general form: `RET([close@self])` reads the
precomputed `e.bar_ret`, and `RET([close@$benchmark])` reads the market-join
`market_ret`.

## Regression

`REGR(y, x, $n)` (alias `REGR_SLOPE`) is the ordinary-least-squares slope of *y*
on *x* over the trailing *N* bars — the classic beta when *y* and *x* are
returns. Lowers to `REGR_SLOPE(y, x) OVER win`; warmup *N* bars. See the README
for the market-benchmark pattern (`REGR(RET([close@self]), RET([close@$benchmark]), $n)`).

## Scalar transforms

`SQRT`, `ABS`, `POWER`, `GREATEST`, and the trigonometric / hyperbolic family are
pure element-wise maps. They take no window and **inherit their argument's
warmup, version, and period** — so `TANH(AVG([close.1h], $n))` warms exactly when
the inner `AVG` does. Each lowers to the same-named SQL function:

- **Trigonometric** (radians): `SIN` `COS` `TAN` `ASIN` `ACOS` `ATAN`
- **Hyperbolic**: `SINH` `COSH` `TANH` `ASINH` `ACOSH` `ATANH` — these require
  **PostgreSQL 12+**.

`GREATEST` takes ≥ 2 arguments and returns their element-wise maximum; there is
no `LEAST`, so a lower bound is spelled with arithmetic or a negated `GREATEST`.
`POWER(a, b)` is `a^b`.

### Guarding division

Postgres raises `division by zero` on any `/ 0` — including `0/0` — and it fails
the **whole** statement, not just one row. Floor denominators that can reach zero,
e.g. `x / GREATEST(STD([close.1h], $n), [close.1h] / 10000)`.

## Windowing & warmup summary

A trailing window `OP(x, $period)` (or literal `OP(x, N)`) frames the last *N*
bars. Windowed ops emit once the frame holds *N* bars — except `RMA` and `RSI`,
which need *N+1*. Scalar ops carry their argument's warmup unchanged. Adding a
new builtin means touching three places: `CallOp` (parse + `as_str`) in
`ast.rs`, the arity/window rule in `sem.rs`, and the lowering in `compile.rs`.
