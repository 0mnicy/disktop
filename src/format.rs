const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

/// Formats a byte count with binary units, e.g. `1.5 GiB`.
pub fn bytes(value: f64) -> String {
    let mut value = value.max(0.0);
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_values_stay_in_bytes() {
        assert_eq!(bytes(0.0), "0 B");
        assert_eq!(bytes(1023.0), "1023 B");
    }

    #[test]
    fn scales_through_binary_units() {
        assert_eq!(bytes(1024.0), "1.0 KiB");
        assert_eq!(bytes(1536.0 * 1024.0), "1.5 MiB");
        assert_eq!(bytes(476.0 * 1024.0 * 1024.0 * 1024.0), "476.0 GiB");
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
        assert_eq!(rate(2048.0), "2.0 KiB/s");
    }
}
