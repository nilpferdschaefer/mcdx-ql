//! Conformance / golden test suite for the canonical indicator engine.
//!
//! Provenance of the cases below:
//!   * `ram_*`   — ported from `mcdx-ram/src/ql/eval.rs` `#[cfg(test)]` (the
//!                 origin of this math). No coverage loss on extraction.
//!   * `java_*`  — golden numbers replicated from the Flink Java tests under
//!                 `mcdx-analytics/flink/src/test/java/com/mcdx/...`
//!                 (`IndicatorMathTest`, `EmaCloseSqlParityTest`,
//!                 `AtrWilderSqlParityTest`, `RsiWilderSqlParityTest`,
//!                 `JointAnalyticsCalculatorTest`).
//!   * `sql_*`   — the mcdx-ql SQL emitter (`compile.rs` `ema_sql`/`rma_tr_sql`/
//!                 `rsi_sql`) carries no numeric fixtures (its tests are
//!                 SQL-shape only); its documented algorithm is re-expressed
//!                 here as executable golden vectors so the Rust engine is
//!                 provably at parity with the SQL seeding.
//!   * `converge_*` — the two former parity gaps (HLC true range; RSI flat
//!                 window). The user chose the **Java definition as canonical**,
//!                 so the engine + SQL now MATCH Java. These assert convergence
//!                 (engine == SQL == Java), replacing the old divergence tests.

use super::*;

// ---- helpers ---------------------------------------------------------------

/// Build a close-only series with `version == index` (matches RAM test bars).
fn series(vals: &[f64]) -> SeriesOut {
    let n = vals.len();
    SeriesOut {
        values: vals.iter().map(|v| Some(*v)).collect(),
        versions: (0..n as i64).collect(),
        warmup: vec![true; n],
    }
}

/// Build aligned (high, low, close) series from `(high, low, close)` candles.
fn ohlc(bars: &[(f64, f64, f64)]) -> (SeriesOut, SeriesOut, SeriesOut) {
    let highs: Vec<f64> = bars.iter().map(|b| b.0).collect();
    let lows: Vec<f64> = bars.iter().map(|b| b.1).collect();
    let closes: Vec<f64> = bars.iter().map(|b| b.2).collect();
    (series(&highs), series(&lows), series(&closes))
}

/// Last defined value of a series (the "tip").
fn tip(s: &SeriesOut) -> Option<f64> {
    s.values.iter().rev().find_map(|v| *v)
}

/// All defined values, in order.
fn defined(s: &SeriesOut) -> Vec<f64> {
    s.values.iter().filter_map(|v| *v).collect()
}

const EPS: f64 = 1e-9;
const EPS_TIGHT: f64 = 1e-12;

// ---- reference implementations (mirror Java / SQL algorithms) ---------------

/// Inception-SMA-seeded EMA tip — mirrors Java `IndicatorMath.emaValues` and the
/// SQL `ema_sql` recursion exactly. Used to prove the engine matches both.
fn ref_ema_tip(closes: &[f64], period: usize) -> Option<f64> {
    if period == 0 || closes.len() < period {
        return None;
    }
    let k = 2.0 / (period as f64 + 1.0);
    let mut ema = closes[..period].iter().sum::<f64>() / period as f64;
    for &c in &closes[period..] {
        ema = c * k + ema * (1.0 - k);
    }
    Some(ema)
}

/// HLC (full) Wilder ATR tip — mirrors Java `IndicatorMath.atr` / `trueRange` and
/// the SQL `rma_tr_sql`. `bars`: (high, low, close). This is the CANONICAL ATR.
fn ref_wilder_atr_hlc(bars: &[(f64, f64, f64)], period: usize) -> Option<f64> {
    if bars.len() < period + 1 {
        return None;
    }
    let tr = |i: usize| -> f64 {
        let (h, l, _c) = bars[i];
        let prev_close = bars[i - 1].2;
        (h - l).max((h - prev_close).abs()).max((l - prev_close).abs())
    };
    let mut atr = (1..=period).map(tr).sum::<f64>() / period as f64;
    for i in (period + 1)..bars.len() {
        atr = (atr * (period as f64 - 1.0) + tr(i)) / period as f64;
    }
    Some(atr)
}

/// Wilder RSI tip with the CANONICAL (Java) degenerate rule:
/// `avg_loss==0 -> avg_gain>0 ? 100 : 50`.
fn ref_wilder_rsi(closes: &[f64], period: usize) -> Option<f64> {
    if closes.len() < period + 1 {
        return None;
    }
    let (mut ag, mut al) = (0.0, 0.0);
    for i in 1..=period {
        let d = closes[i] - closes[i - 1];
        if d >= 0.0 {
            ag += d;
        } else {
            al -= d;
        }
    }
    ag /= period as f64;
    al /= period as f64;
    for i in (period + 1)..closes.len() {
        let d = closes[i] - closes[i - 1];
        let (g, l) = if d >= 0.0 { (d, 0.0) } else { (0.0, -d) };
        ag = (ag * (period as f64 - 1.0) + g) / period as f64;
        al = (al * (period as f64 - 1.0) + l) / period as f64;
    }
    Some(rsi_from_avgs(ag, al))
}

// ===========================================================================
// RAM-ported tests (origin: mcdx-ram/src/ql/eval.rs)
// ===========================================================================

/// Ported verbatim: `ema_seeds_past_leading_none`. The load-bearing subtlety —
/// EMA seeds from the first *contiguous* run of `period` defined values, letting
/// `EMA(REGR_SLOPE(...), k)` seed past the inner reducer's warmup Nones.
#[test]
fn ram_ema_seeds_past_leading_none() {
    let s = SeriesOut {
        values: vec![None, None, Some(2.0), Some(2.0), Some(2.0), Some(2.0)],
        versions: vec![0, 0, 3, 4, 5, 6],
        warmup: vec![false, false, true, true, true, true],
    };
    let out = ema_series(&s, 3);
    // First contiguous run of 3 = indices 2,3,4 → seed at idx 4 = SMA(2,2,2) = 2.
    assert_eq!(out.values[0], None);
    assert_eq!(out.values[3], None); // still warming (before seed idx)
    assert!((out.values[4].unwrap() - 2.0).abs() < 1e-12);
    assert!((out.values[5].unwrap() - 2.0).abs() < 1e-12);
    assert!(out.warmup[4] && out.warmup[5]);
}

/// Engine-level form of RAM `sma_tip_matches_hand_calc`: last 5 of 1..=20 → 18.
#[test]
fn ram_sma_tip_matches_hand_calc() {
    let closes: Vec<f64> = (1..=20).map(|x| x as f64).collect();
    let out = rolling_avg(&series(&closes), 5);
    assert!((tip(&out).unwrap() - 18.0).abs() < EPS);
}

/// Engine-level form of RAM `ema_seed_is_sma`.
#[test]
fn ram_ema_seed_is_sma() {
    let out = ema_series(&series(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 3);
    let d = defined(&out);
    assert!((d[0] - 2.0).abs() < EPS); // SMA(1,2,3) = 2
    assert!((d[1] - 3.0).abs() < EPS); // 4*0.5 + 2*0.5 = 3
}

/// Engine-level form of RAM `atr_wilder_seed`, updated to the HLC true range.
/// Uses the RAM test-bar convention high=close+1, low=close-1 so the case is
/// directly comparable to the RAM integration test.
#[test]
fn ram_atr_wilder_seed_hlc() {
    let closes = [10.0, 11.0, 12.0, 13.0, 14.0, 16.0];
    let bars: Vec<(f64, f64, f64)> = closes.iter().map(|&c| (c + 1.0, c - 1.0, c)).collect();
    let (h, l, c) = ohlc(&bars);
    let out = wilder_atr(&h, &l, &c, 3);
    let d = defined(&out);
    // TR (HLC) = [.,2,2,2,2,3]; seed AVG(2,2,2)=2, then (2*2+2)/3=2, then (2*2+3)/3=7/3.
    assert!((d[0] - 2.0).abs() < EPS);
    assert!((d[1] - 2.0).abs() < EPS);
    assert!((d[2] - (7.0 / 3.0)).abs() < EPS);
    assert!((tip(&out).unwrap() - ref_wilder_atr_hlc(&bars, 3).unwrap()).abs() < EPS_TIGHT);
}

/// Engine-level form of RAM `daily_beta_and_smoothings` core: perfectly collinear
/// asset/market returns → rolling OLS slope == beta at every warm bar.
#[test]
fn ram_regr_slope_recovers_beta() {
    let beta = 2.0;
    let rets = [0.01, -0.02, 0.015, -0.005, 0.02, -0.01, 0.008, -0.012, 0.005, 0.017];
    let mut mkt = vec![1000.0];
    let mut asset = vec![100.0];
    for &r in &rets {
        mkt.push(mkt.last().unwrap() * (1.0 + r));
        asset.push(asset.last().unwrap() * (1.0 + beta * r));
    }
    // regress asset_ret on mkt_ret over trailing window 5.
    let mkt_ret = ret_series(&series(&mkt));
    let asset_ret = ret_series(&series(&asset));
    let slope = rolling_regr_slope(&asset_ret, &mkt_ret, 5);
    assert!((tip(&slope).unwrap() - 2.0).abs() < EPS);
}

// ===========================================================================
// Java conformance goldens (origin: flink/src/test/java/com/mcdx/...)
// ===========================================================================

/// `IndicatorMathTest.sma_on_flat_series`: 5 bars @100 → 100.
#[test]
fn java_sma_on_flat_series() {
    let out = rolling_avg(&series(&[100.0; 5]), 5);
    assert!((tip(&out).unwrap() - 100.0).abs() < EPS);
}

/// `IndicatorMathTest.bollinger_bands_use_population_stdev`: closes {10,12,14,16,18},
/// period 5 → mean 14, population var 8, stdev sqrt(8).
#[test]
fn java_population_stdev_and_var() {
    let closes = [10.0, 12.0, 14.0, 16.0, 18.0];
    assert!((tip(&rolling_avg(&series(&closes), 5)).unwrap() - 14.0).abs() < EPS);
    assert!((tip(&rolling_var(&series(&closes), 5)).unwrap() - 8.0).abs() < EPS);
    assert!((tip(&rolling_std(&series(&closes), 5)).unwrap() - 8.0_f64.sqrt()).abs() < EPS);
}

/// `JointAnalyticsCalculatorTest.emaCloseSqlUsesInceptionSmaSeed`: with only
/// `period` closes [10,20,30], EMA == SMA seed == 20.
#[test]
fn java_ema_seed_equals_sma_when_exactly_period() {
    let out = ema_series(&series(&[10.0, 20.0, 30.0]), 3);
    assert!((tip(&out).unwrap() - 20.0).abs() < EPS_TIGHT);
}

/// `EmaCloseSqlParityTest`: closes {10,12,11,15,14,16}, periods 2..=6, engine EMA
/// tip must equal the inception-SMA-seeded reference (== Java == SQL).
#[test]
fn java_ema_parity_over_periods() {
    let closes = [10.0, 12.0, 11.0, 15.0, 14.0, 16.0];
    for period in 2..=6usize {
        let got = tip(&ema_series(&series(&closes), period)).unwrap();
        let want = ref_ema_tip(&closes, period).unwrap();
        assert!((got - want).abs() < EPS_TIGHT, "period {period}: {got} vs {want}");
    }
    // Anchored hand-calc for period 3: seed 11, α=0.5 → 13, 13.5, 14.75.
    let d = defined(&ema_series(&series(&closes), 3));
    for (g, w) in d.iter().zip([11.0, 13.0, 13.5, 14.75]) {
        assert!((g - w).abs() < EPS_TIGHT);
    }
}

/// `AtrWilderSqlParityTest`: real OHLC candles (high≠low≠close), periods 2..=5.
/// Engine `wilder_atr` (HLC) must equal the canonical HLC reference.
#[test]
fn java_atr_hlc_parity_over_periods() {
    // (high, low, close) with genuine intrabar range.
    let bars = [
        (102.0, 99.0, 100.0),
        (104.0, 100.0, 102.0),
        (103.0, 100.0, 101.0),
        (107.0, 101.0, 105.0),
        (106.0, 103.0, 104.0),
        (110.0, 104.0, 108.0),
        (109.0, 106.0, 107.0),
        (112.0, 107.0, 110.0),
        (111.0, 108.0, 109.0),
        (115.0, 109.0, 112.0),
    ];
    let (h, l, c) = ohlc(&bars);
    for period in 2..=5usize {
        let got = tip(&wilder_atr(&h, &l, &c, period)).unwrap();
        let want = ref_wilder_atr_hlc(&bars, period).unwrap();
        assert!((got - want).abs() < EPS_TIGHT, "period {period}: {got} vs {want}");
    }
    // Anchored: period 3 seed = AVG(TR@1,2,3). TR1=max(4,4,0)=4, TR2=max(3,1,2)=3,
    // TR3=max(6,6,0)=6 → seed (4+3+6)/3 = 13/3.
    assert!((defined(&wilder_atr(&h, &l, &c, 3))[0] - 13.0 / 3.0).abs() < EPS_TIGHT);
}

/// `RsiWilderSqlParityTest`: closes {100,102,101,105,104,108,107,110,109,112,111,115},
/// periods 2..=5. Engine RSI == Wilder reference.
#[test]
fn java_rsi_parity_over_periods() {
    let closes = [
        100.0, 102.0, 101.0, 105.0, 104.0, 108.0, 107.0, 110.0, 109.0, 112.0, 111.0, 115.0,
    ];
    for period in 2..=5usize {
        let got = tip(&wilder_rsi(&series(&closes), period)).unwrap();
        let want = ref_wilder_rsi(&closes, period).unwrap();
        assert!((got - want).abs() < EPS_TIGHT, "period {period}: {got} vs {want}");
    }
}

/// `IndicatorMathTest.macd_line_is_fast_minus_slow_ema` building block: on rising
/// closes 100+i, MACD = EMA(12) − EMA(26); here we assert the two EMAs are
/// well-defined and monotone-consistent (fast tip > slow tip on an uptrend).
#[test]
fn java_macd_component_emas() {
    let closes: Vec<f64> = (0..40).map(|i| 100.0 + i as f64).collect();
    let fast = tip(&ema_series(&series(&closes), 12)).unwrap();
    let slow = tip(&ema_series(&series(&closes), 26)).unwrap();
    assert!(fast > slow); // fast EMA leads on a monotone uptrend
    assert!((fast - ref_ema_tip(&closes, 12).unwrap()).abs() < EPS_TIGHT);
    assert!((slow - ref_ema_tip(&closes, 26).unwrap()).abs() < EPS_TIGHT);
}

// ===========================================================================
// SQL conformance goldens (origin: mcdx-ql compile.rs documented algorithm)
// ===========================================================================

/// SQL `rma_tr_sql` uses HLC true range `GREATEST(h-l, |h-prevC|, |l-prevC|)` and
/// seeds AVG(tr) over ords 2..=period+1 → identical to the engine. Golden vector.
#[test]
fn sql_atr_hlc_matches_engine() {
    let bars = [
        (52.0, 48.0, 50.0),
        (56.0, 51.0, 55.0),
        (55.0, 52.0, 53.0),
        (61.0, 54.0, 60.0),
        (59.0, 55.0, 58.0),
        (66.0, 60.0, 65.0),
    ];
    let (h, l, c) = ohlc(&bars);
    let out = wilder_atr(&h, &l, &c, 3);
    // TR1=max(5,6,1)=6, TR2=max(3,0,3)=3, TR3=max(7,8,1)=8 → seed (6+3+8)/3=17/3.
    assert!((defined(&out)[0] - 17.0 / 3.0).abs() < EPS_TIGHT);
    assert!((tip(&out).unwrap() - ref_wilder_atr_hlc(&bars, 3).unwrap()).abs() < EPS_TIGHT);
}

/// SQL `rsi_sql` degenerate branch: `avg_loss = 0 AND avg_gain > 0 → 100.0`.
/// Strictly rising closes.
#[test]
fn sql_rsi_all_gains_is_100() {
    let closes = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    assert!((tip(&wilder_rsi(&series(&closes), 3)).unwrap() - 100.0).abs() < EPS_TIGHT);
}

/// SQL VAR/STD are population form `E[x^2] - E[x]^2` (matches engine & Java).
#[test]
fn sql_population_var_identity() {
    let closes = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
    // Known textbook population variance of this set = 4.
    assert!((tip(&rolling_var(&series(&closes), 8)).unwrap() - 4.0).abs() < EPS);
}

// ===========================================================================
// Convergence tests — the two former parity gaps, now resolved.
// Canonical definition = Java. Engine == SQL == Java.
// ===========================================================================

/// CONVERGENCE (was GAP 1) — ATR True Range is now the full HLC true range in all
/// three implementations. On real (non-collapsed) candles the engine `wilder_atr`
/// equals the canonical HLC reference (Java `IndicatorMath.atr` / SQL `rma_tr_sql`).
#[test]
fn converge_atr_is_hlc_true_range() {
    // Candles with genuine intrabar range (high != low != close). The old
    // close-to-close definition would give a DIFFERENT number here — this case
    // is what a c2c regression would fail on.
    let bars = [
        (11.0, 9.0, 10.0),
        (13.0, 10.0, 12.0),
        (12.5, 10.5, 11.0),
        (16.0, 11.0, 15.0),
        (15.5, 13.0, 14.0),
        (17.0, 14.0, 16.0),
        (16.5, 15.0, 15.5),
        (18.0, 15.5, 17.0),
    ];
    let (h, l, c) = ohlc(&bars);
    for period in 2..=4usize {
        let engine = tip(&wilder_atr(&h, &l, &c, period)).unwrap();
        let canonical = ref_wilder_atr_hlc(&bars, period).unwrap();
        assert!(
            (engine - canonical).abs() < EPS_TIGHT,
            "period {period}: engine={engine} canonical={canonical}"
        );
    }
    // Prove it is genuinely HLC, not close-to-close: a pure c2c ATR on these
    // closes differs from the engine result.
    let closes: Vec<f64> = bars.iter().map(|b| b.2).collect();
    let c2c_trs: Vec<f64> = closes.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
    let mut c2c = c2c_trs[..3].iter().sum::<f64>() / 3.0;
    for &tr in &c2c_trs[3..] {
        c2c = (c2c * 2.0 + tr) / 3.0;
    }
    let engine = tip(&wilder_atr(&h, &l, &c, 3)).unwrap();
    assert!(
        (engine - c2c).abs() > 0.1,
        "engine ATR must reflect HLC range, not close-to-close (engine={engine} c2c={c2c})"
    );
}

/// The standalone `tr_series` builtin math (HLC) — anchored hand-calc.
#[test]
fn tr_series_is_hlc() {
    let bars = [(11.0, 9.0, 10.0), (13.0, 10.0, 12.0), (12.5, 10.5, 11.0)];
    let (h, l, c) = ohlc(&bars);
    let tr = tr_series(&h, &l, &c);
    assert_eq!(tr.values[0], None); // no prior close
    // idx1: max(13-10, |13-10|, |10-10|) = 3 ; idx2: max(12.5-10.5, |12.5-12|, |10.5-12|)=2
    assert!((tr.values[1].unwrap() - 3.0).abs() < EPS_TIGHT);
    assert!((tr.values[2].unwrap() - 2.0).abs() < EPS_TIGHT);
}

/// CONVERGENCE (was GAP 2) — RSI on a perfectly flat window now returns 50.0 in
/// all three implementations (engine == SQL == Java `IndicatorMath.rsi`).
#[test]
fn converge_rsi_flat_window_is_50() {
    let flat = [100.0; 6];
    let engine = tip(&wilder_rsi(&series(&flat), 3)).unwrap();
    assert!((engine - 50.0).abs() < EPS_TIGHT); // engine == SQL == Java
    // Direct check of the degenerate rule at the leaf.
    assert!((rsi_from_avgs(0.0, 0.0) - 50.0).abs() < EPS_TIGHT); // flat → 50
    assert!((rsi_from_avgs(3.0, 0.0) - 100.0).abs() < EPS_TIGHT); // all gains → 100
}
