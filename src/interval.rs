//! Reporting-period → interval milliseconds.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("unknown reporting_period `{0}`")]
pub struct IntervalError(pub String);

/// Map a reporting-period suffix to bar length in milliseconds.
pub fn interval_ms(reporting_period: &str) -> Result<i64, IntervalError> {
    let ms = match reporting_period {
        "1m" => 60_000,
        "5m" => 300_000,
        "15m" => 900_000,
        "30m" => 1_800_000,
        "1h" => 3_600_000,
        "2h" => 7_200_000,
        "4h" => 14_400_000,
        "6h" => 21_600_000,
        "12h" => 43_200_000,
        "1d" => 86_400_000,
        "1w" => 604_800_000,
        other => return Err(IntervalError(other.to_string())),
    };
    Ok(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_periods() {
        assert_eq!(interval_ms("1d").unwrap(), 86_400_000);
        assert_eq!(interval_ms("1h").unwrap(), 3_600_000);
        assert_eq!(interval_ms("5m").unwrap(), 300_000);
    }

    #[test]
    fn unknown_period() {
        assert!(interval_ms("3d").is_err());
    }
}
