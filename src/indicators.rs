//! Canonical indicator math engine (series-in → series-out).
//!
//! This module is the **single source of truth** for MCDX indicator arithmetic.
//! It was promoted verbatim out of `mcdx-ram/src/ql/eval.rs` so that the live
//! (RAM) path, the materializing workers, and any golden/conformance tests all
//! evaluate the exact same fiddly seeding logic (Wilder seeding, EMA warmup past
//! a leading `None` run). Do not fork this math — every back-end calls here.
//!
//! The engine is intentionally free of any store / candle / plan dependency: it
//! operates purely on [`SeriesOut`] (a value/version/warmup triple parallel to
//! the source bars). Callers align raw columns onto bar timestamps and feed the
//! resulting series in; the engine returns a new series.

/// Error returned by the (few) fallible engine ops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndicatorError {
    /// Two series fed to a binary op had different lengths.
    LengthMismatch,
}

impl std::fmt::Display for IndicatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Message preserved byte-for-byte from the original RAM eval so
            // downstream error text does not drift.
            IndicatorError::LengthMismatch => write!(f, "series length mismatch in binary op"),
        }
    }
}

impl std::error::Error for IndicatorError {}

/// A computed series aligned 1:1 with the source bars.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesOut {
    /// Parallel to source bars; `None` = not defined / warmup incomplete at that bar.
    pub values: Vec<Option<f64>>,
    pub versions: Vec<i64>,
    pub warmup: Vec<bool>,
}

impl SeriesOut {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Element-wise binary combine of two aligned series.
    pub fn map_zip<F>(&self, other: &SeriesOut, f: F) -> Result<SeriesOut, IndicatorError>
    where
        F: Fn(f64, f64) -> f64,
    {
        if self.len() != other.len() {
            return Err(IndicatorError::LengthMismatch);
        }
        let n = self.len();
        let mut values = Vec::with_capacity(n);
        let mut versions = Vec::with_capacity(n);
        let mut warmup = Vec::with_capacity(n);
        for i in 0..n {
            match (self.values[i], other.values[i]) {
                (Some(a), Some(b)) => {
                    values.push(Some(f(a, b)));
                    versions.push(self.versions[i].max(other.versions[i]));
                    warmup.push(self.warmup[i] && other.warmup[i]);
                }
                _ => {
                    values.push(None);
                    versions.push(self.versions[i].max(other.versions[i]));
                    warmup.push(false);
                }
            }
        }
        Ok(SeriesOut {
            values,
            versions,
            warmup,
        })
    }

    /// Element-wise unary map, preserving `None`s / versions / warmup.
    pub fn map_unary<F>(&self, f: F) -> SeriesOut
    where
        F: Fn(f64) -> f64,
    {
        SeriesOut {
            values: self.values.iter().map(|v| v.map(&f)).collect(),
            versions: self.versions.clone(),
            warmup: self.warmup.clone(),
        }
    }
}

/// Period-over-period return: `cur/prev - 1`, guarded to positive inputs and a
/// 5x (500%) abs move cap (a spike guard). Leading bar is `None`.
pub fn ret_series(s: &SeriesOut) -> SeriesOut {
    let n = s.len();
    let mut values = vec![None; n];
    let mut warmup = vec![false; n];
    for i in 1..n {
        match (s.values[i - 1], s.values[i]) {
            (Some(prev), Some(cur)) if prev > 0.0 && cur > 0.0 => {
                let r = cur / prev - 1.0;
                if r.abs() > 5.0 {
                    values[i] = None;
                    warmup[i] = false;
                } else {
                    values[i] = Some(r);
                    warmup[i] = true;
                }
            }
            _ => {}
        }
    }
    SeriesOut {
        values,
        versions: s.versions.clone(),
        warmup,
    }
}

/// Wilder true range: `max(high-low, |high-prevClose|, |low-prevClose|)`.
///
/// This is the canonical (HLC) definition, matching the Flink Java
/// `IndicatorMath.trueRange`. Index 0 is `None` (there is no prior close), and
/// any missing input at `i` or the prior close suppresses `TR[i]`.
pub fn tr_series(high: &SeriesOut, low: &SeriesOut, close: &SeriesOut) -> SeriesOut {
    let n = high.len().min(low.len()).min(close.len());
    let mut values = vec![None; n];
    let mut warmup = vec![false; n];
    for i in 1..n {
        if let (Some(h), Some(l), Some(prev_close)) =
            (high.values[i], low.values[i], close.values[i - 1])
        {
            let tr = (h - l).max((h - prev_close).abs()).max((l - prev_close).abs());
            values[i] = Some(tr);
            warmup[i] = true;
        }
    }
    let versions: Vec<i64> = (0..n).map(|i| close.versions[i]).collect();
    SeriesOut {
        values,
        versions,
        warmup,
    }
}

/// Simple moving average over a trailing window of `period` defined values.
pub fn rolling_avg(s: &SeriesOut, period: usize) -> SeriesOut {
    rolling_stat(s, period, |window| {
        let sum: f64 = window.iter().sum();
        Some(sum / period as f64)
    })
}

/// Population variance over a trailing window (`E[x^2] - E[x]^2`).
pub fn rolling_var(s: &SeriesOut, period: usize) -> SeriesOut {
    rolling_stat(s, period, |window| {
        let mean = window.iter().sum::<f64>() / period as f64;
        let mean_sq = window.iter().map(|x| x * x).sum::<f64>() / period as f64;
        Some(mean_sq - mean * mean)
    })
}

/// Population standard deviation over a trailing window (clamped non-negative).
pub fn rolling_std(s: &SeriesOut, period: usize) -> SeriesOut {
    rolling_stat(s, period, |window| {
        let mean = window.iter().sum::<f64>() / period as f64;
        let mean_sq = window.iter().map(|x| x * x).sum::<f64>() / period as f64;
        Some((mean_sq - mean * mean).max(0.0).sqrt())
    })
}

/// Count of defined values in the trailing window (always `period` when warm).
pub fn rolling_count(s: &SeriesOut, period: usize) -> SeriesOut {
    rolling_stat(s, period, |_window| Some(period as f64))
}

/// Generic trailing-window reducer. Emits only when the full `period` window is
/// contiguously defined (any `None` in the window suppresses the output).
pub fn rolling_stat<F>(s: &SeriesOut, period: usize, f: F) -> SeriesOut
where
    F: Fn(&[f64]) -> Option<f64>,
{
    let n = s.len();
    let mut values = vec![None; n];
    let mut versions = s.versions.clone();
    let mut warmup = vec![false; n];
    if period == 0 {
        return SeriesOut {
            values,
            versions,
            warmup,
        };
    }
    for i in 0..n {
        if i + 1 < period {
            continue;
        }
        let start = i + 1 - period;
        let mut window = Vec::with_capacity(period);
        let mut ok = true;
        let mut ver = i64::MIN;
        for j in start..=i {
            match s.values[j] {
                Some(v) => {
                    window.push(v);
                    ver = ver.max(s.versions[j]);
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && window.len() == period {
            values[i] = f(&window);
            versions[i] = ver;
            warmup[i] = values[i].is_some();
        }
    }
    SeriesOut {
        values,
        versions,
        warmup,
    }
}

/// EMA seeded with SMA of the first contiguous run of `period` defined values.
///
/// Seeding from the first *contiguous* run (rather than index 0) is what lets an
/// EMA wrap a windowed reducer: `EMA(REGR_SLOPE(...), k)` — the inner regression
/// series has leading `None`s during its own warmup, so index-0 seeding would
/// (wrongly) yield an empty series. When the input has no leading gap this is
/// identical to the previous behaviour (seed at index `period - 1`).
pub fn ema_series(s: &SeriesOut, period: usize) -> SeriesOut {
    let n = s.len();
    let mut values = vec![None; n];
    let mut versions = s.versions.clone();
    let mut warmup = vec![false; n];
    if period == 0 || n < period {
        return SeriesOut {
            values,
            versions,
            warmup,
        };
    }
    // Find the first index where `period` contiguous defined values begin.
    let mut start = None;
    let mut i = 0;
    while i + period <= n {
        if (i..i + period).all(|j| s.values[j].is_some()) {
            start = Some(i);
            break;
        }
        i += 1;
    }
    let start = match start {
        Some(s) => s,
        None => {
            return SeriesOut {
                values,
                versions,
                warmup,
            }
        }
    };
    let seed_end = start + period; // exclusive
    let alpha = 2.0 / (period as f64 + 1.0);
    let seed_sum: f64 = s.values[start..seed_end].iter().map(|v| v.unwrap()).sum();
    let mut ema = seed_sum / period as f64;
    let seed_idx = seed_end - 1;
    values[seed_idx] = Some(ema);
    warmup[seed_idx] = true;
    versions[seed_idx] = s.versions[start..seed_end]
        .iter()
        .copied()
        .max()
        .unwrap_or(0);

    for i in seed_end..n {
        match s.values[i] {
            Some(v) => {
                ema = v * alpha + ema * (1.0 - alpha);
                values[i] = Some(ema);
                warmup[i] = true;
                versions[i] = s.versions[i];
            }
            None => {
                // Break the EMA chain on gaps (recurrence is undefined across a hole).
                break;
            }
        }
    }
    SeriesOut {
        values,
        versions,
        warmup,
    }
}

/// Wilder ATR over the HLC true range: seed AVG(TR) for bars 2..=period+1, then
/// Wilder-smooth. TR is the canonical `max(h-l, |h-prevC|, |l-prevC|)`
/// (see [`tr_series`]), matching the Flink Java `IndicatorMath.atr`.
pub fn wilder_atr(high: &SeriesOut, low: &SeriesOut, close: &SeriesOut, period: usize) -> SeriesOut {
    let trs = tr_series(high, low, close);
    let n = trs.len();
    let need = period + 1;
    let mut values = vec![None; n];
    let mut versions = trs.versions.clone();
    let mut warmup = vec![false; n];
    if n < need {
        return SeriesOut {
            values,
            versions,
            warmup,
        };
    }
    // Seed: average of TR at indices 1..period (ords 2..=period+1 in 1-based).
    let mut seed = 0.0;
    for i in 1..=period {
        match trs.values[i] {
            Some(v) => seed += v,
            None => {
                return SeriesOut {
                    values,
                    versions,
                    warmup,
                }
            }
        }
    }
    let mut atr = seed / period as f64;
    let seed_idx = period; // 0-based index == period (1-based ord period+1)
    values[seed_idx] = Some(atr);
    warmup[seed_idx] = true;
    versions[seed_idx] = trs.versions[..=seed_idx]
        .iter()
        .copied()
        .max()
        .unwrap_or(0);

    for i in (seed_idx + 1)..n {
        match trs.values[i] {
            Some(tr) => {
                atr = (atr * (period as f64 - 1.0) + tr) / period as f64;
                values[i] = Some(atr);
                warmup[i] = true;
                versions[i] = trs.versions[i];
            }
            None => break,
        }
    }
    SeriesOut {
        values,
        versions,
        warmup,
    }
}

/// Wilder RSI over close-to-close gains/losses; seed = SMA of first `period`
/// gains/losses, then Wilder-smoothed.
pub fn wilder_rsi(closes: &SeriesOut, period: usize) -> SeriesOut {
    let n = closes.len();
    let need = period + 1;
    let mut values = vec![None; n];
    let versions = closes.versions.clone();
    let mut warmup = vec![false; n];
    if n < need {
        return SeriesOut {
            values,
            versions,
            warmup,
        };
    }
    let mut gains = vec![0.0; n];
    let mut losses = vec![0.0; n];
    for i in 1..n {
        match (closes.values[i - 1], closes.values[i]) {
            (Some(prev), Some(cur)) => {
                let d = cur - prev;
                gains[i] = d.max(0.0);
                losses[i] = (-d).max(0.0);
            }
            _ => {
                return SeriesOut {
                    values,
                    versions,
                    warmup,
                }
            }
        }
    }
    let mut avg_gain = gains[1..=period].iter().sum::<f64>() / period as f64;
    let mut avg_loss = losses[1..=period].iter().sum::<f64>() / period as f64;
    let seed_idx = period;
    values[seed_idx] = Some(rsi_from_avgs(avg_gain, avg_loss));
    warmup[seed_idx] = true;

    for i in (seed_idx + 1)..n {
        avg_gain = (avg_gain * (period as f64 - 1.0) + gains[i]) / period as f64;
        avg_loss = (avg_loss * (period as f64 - 1.0) + losses[i]) / period as f64;
        values[i] = Some(rsi_from_avgs(avg_gain, avg_loss));
        warmup[i] = true;
    }
    SeriesOut {
        values,
        versions,
        warmup,
    }
}

/// RSI from smoothed average gain/loss. Degenerate `avg_loss == 0` branch matches
/// the Flink Java `IndicatorMath.rsi`: all-gains → 100, perfectly flat → 50.
pub fn rsi_from_avgs(avg_gain: f64, avg_loss: f64) -> f64 {
    if avg_loss == 0.0 {
        if avg_gain > 0.0 {
            100.0
        } else {
            50.0
        }
    } else {
        100.0 - (100.0 / (1.0 + (avg_gain / avg_loss)))
    }
}

/// Rolling OLS slope of `y` on `x` over a trailing window of `period`.
pub fn rolling_regr_slope(y: &SeriesOut, x: &SeriesOut, period: usize) -> SeriesOut {
    let n = y.len().min(x.len());
    let mut values = vec![None; n];
    let mut versions = vec![0; n];
    let mut warmup = vec![false; n];
    if period < 2 {
        return SeriesOut {
            values,
            versions,
            warmup,
        };
    }
    for i in 0..n {
        if i + 1 < period {
            continue;
        }
        let start = i + 1 - period;
        let mut ys = Vec::with_capacity(period);
        let mut xs = Vec::with_capacity(period);
        let mut ver = i64::MIN;
        let mut ok = true;
        for j in start..=i {
            match (y.values[j], x.values[j]) {
                (Some(yy), Some(xx)) => {
                    ys.push(yy);
                    xs.push(xx);
                    ver = ver.max(y.versions[j]).max(x.versions[j]);
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        values[i] = regr_slope(&ys, &xs);
        versions[i] = ver;
        warmup[i] = values[i].is_some();
    }
    SeriesOut {
        values,
        versions,
        warmup,
    }
}

/// OLS slope of `ys` on `xs`. `None` if fewer than 2 points or zero x-variance.
pub fn regr_slope(ys: &[f64], xs: &[f64]) -> Option<f64> {
    let n = ys.len() as f64;
    if n < 2.0 {
        return None;
    }
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for i in 0..ys.len() {
        let dx = xs[i] - mean_x;
        num += dx * (ys[i] - mean_y);
        den += dx * dx;
    }
    if den == 0.0 {
        None
    } else {
        Some(num / den)
    }
}

#[cfg(test)]
mod tests;
