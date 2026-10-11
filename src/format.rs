const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

/// Widest output of [`bytes`] below the top unit, such as `999.9 MiB`.
pub const BYTES_WIDTH: usize = 9;
/// Widest output of [`rate`] below the top unit, such as `999.9 MiB/s`.
pub const RATE_WIDTH: usize = BYTES_WIDTH + 2;

/// Formats a byte count with binary units, e.g. `1.5 GiB`.
///
/// Steps up a unit before the number would need four digits, so `1000 MiB`
/// reads as `1.0 GiB` and every value fits in [`BYTES_WIDTH`].
pub fn bytes(value: f64) -> String {
    let mut value = value.max(0.0);
    let mut unit = 0;
    while unit < UNITS.len() - 1 {
        // Whole bytes round at .5; the larger units show one decimal.
        let limit = if unit == 0 { 999.5 } else { 999.95 };
        if value < limit {
            break;
        }
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Formats a throughput in bytes per second, e.g. `12.3 MiB/s`.
pub fn rate(bytes_per_sec: f64) -> String {
    format!("{}/s", bytes(bytes_per_sec))
}

/// [`bytes`] right-aligned in a fixed-width slot, so text next to it holds
/// still as the value changes.
pub fn bytes_fixed(value: f64) -> String {
    format!("{:>BYTES_WIDTH$}", bytes(value))
}

/// [`rate`] right-aligned in a fixed-width slot, so text next to it holds
/// still as the value changes.
pub fn rate_fixed(bytes_per_sec: f64) -> String {
    format!("{:>RATE_WIDTH$}", rate(bytes_per_sec))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * KIB;
    const GIB: f64 = 1024.0 * MIB;

    #[test]
    fn small_values_stay_in_bytes() {
        assert_eq!(bytes(0.0), "0 B");
        assert_eq!(bytes(999.0), "999 B");
    }

    #[test]
    fn scales_through_binary_units() {
        assert_eq!(bytes(KIB), "1.0 KiB");
        assert_eq!(bytes(1.5 * MIB), "1.5 MiB");
        assert_eq!(bytes(476.0 * GIB), "476.0 GiB");
    }

    #[test]
    fn steps_up_before_four_digits() {
        assert_eq!(bytes(999.6), "1.0 KiB");
        assert_eq!(bytes(1023.0), "1.0 KiB");
        assert_eq!(bytes(999.9 * MIB), "999.9 MiB");
        assert_eq!(bytes(999.96 * MIB), "1.0 GiB");
        assert_eq!(bytes(1000.0 * MIB), "1.0 GiB");
    }

    #[test]
    fn stops_at_the_largest_unit() {
        assert_eq!(bytes(1024f64.powi(6)), "1024.0 PiB");
    }

    #[test]
    fn negative_values_clamp_to_zero() {
        assert_eq!(bytes(-5.0), "0 B");
    }

    #[test]
    fn rate_appends_per_second() {
        assert_eq!(rate(2.0 * KIB), "2.0 KiB/s");
    }

    #[test]
    fn fixed_width_values_never_change_width() {
        // Sweep from nothing to petabytes, including every unit boundary.
        let mut value = 0.0;
        while value < 1024f64.powi(5) * 999.0 {
            assert!(bytes(value).len() <= BYTES_WIDTH, "{}", bytes(value));
            assert_eq!(bytes_fixed(value).len(), BYTES_WIDTH, "{value}");
            assert_eq!(rate_fixed(value).len(), RATE_WIDTH, "{value}");
            value = value * 1.07 + 1.0;
        }
        assert_eq!(rate_fixed(0.0), "      0 B/s");
        assert_eq!(rate_fixed(553.5 * MIB), "553.5 MiB/s");
    }
}
