# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-08-07

First public release of the MCDX indicator grammar → joint-analytics SQL compiler.

### Added

- Compile indicator expressions and batch forms into a Postgres CTE envelope (`data`, `obj`, `mcdx_asset` in `public`).
- Grammar: series buckets, domains (`$from:$to`, trailing `N@$end` / `N@latest`), result slices, `@self` / `@ticker` / `@$param` asset qualification, inherited postfix `[$from:$to]`, and batch-level inherited range `{ … }[$from:$to]`.
- Unaggregated source-qualified series: `[binance:close.1d]` with per-source `params_hash`.
- Object series (`obj`): raw fetch `[candles.1h]` and `->field` scalar projection `[candles.1h->close]`.
- Builtins: `AVG`, `VAR`, `STD`, `COUNT`, `RET`, `TR`, `EMA`, `RMA`, `RSI`, `REGR` / `REGR_SLOPE`, `SQRT`, `GREATEST`, `POWER`, `ABS`.
- Rust library (`mcdx_ql`), JSON compile API, SQL row mapping, and JNI Java bindings (`McdxQl.compile`).
- GitHub Packages Maven publish (JAR + Javadoc + attached `.crate`), CI bundles, and GitHub Pages docs.

### Changed

- Generated SQL targets unqualified `public` tables (`data`, `obj`, `mcdx_asset`) instead of `core.*` schema prefixes.

### Fixed

- Lookback version SQL codegen for `EMA` / `RMA` / `RSI` (avoid invalid `Long.MIN_VALUE` casts in `GREATEST`).
- Clippy `write_with_newline` in object-series SQL emission.

[0.1.0]: https://github.com/nilpferdschaefer/mcdx-ql/releases/tag/v0.1.0
