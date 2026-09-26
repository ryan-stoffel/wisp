use jiff::Timestamp;
use jiff::fmt::temporal::DateTimePrinter;

use crate::error::StoreError;

/// Fixed at nine fractional digits, so lexicographic (TEXT) order always
/// agrees with chronological order. The default formatting trims trailing
/// zeros and drops an all-zero fraction entirely, which is how the old
/// `time`-based store could misorder two timestamps in the same second.
const PRINTER: DateTimePrinter = DateTimePrinter::new().precision(Some(9));

/// Formats `timestamp` as fixed-width RFC 3339 UTC, the format used for
/// every stored column.
pub(crate) fn format(timestamp: Timestamp) -> String {
    PRINTER.timestamp_to_string(&timestamp)
}

pub(crate) fn now() -> String {
    format(Timestamp::now())
}

/// Parses a stored RFC 3339 UTC timestamp. Accepts any fractional width,
/// including none, so rows written before this format was fixed-width still
/// read back correctly.
pub(crate) fn parse(text: &str) -> Result<Timestamp, StoreError> {
    Ok(text.parse()?)
}

#[cfg(test)]
mod tests {
    use super::format;

    #[test]
    fn format_is_fixed_width_and_orders_like_time() {
        // Same whole second, increasing nanoseconds. Variable-width RFC
        // 3339 trims trailing zeros, so `.5s` becomes the single digit "5"
        // while `.50001s` keeps five digits; since `Z` sorts after any
        // digit, the shorter (earlier) value would sort after the longer
        // (later) one as TEXT. Fixed width must not repeat that.
        let earlier: jiff::Timestamp = "2026-01-01T12:00:00.5Z".parse().unwrap();
        let later: jiff::Timestamp = "2026-01-01T12:00:00.50001Z".parse().unwrap();
        assert!(earlier < later);

        let formatted_earlier = format(earlier);
        let formatted_later = format(later);

        assert_eq!(formatted_earlier, "2026-01-01T12:00:00.500000000Z");
        assert_eq!(formatted_later, "2026-01-01T12:00:00.500010000Z");
        assert!(
            formatted_earlier < formatted_later,
            "fixed-width text order must match chronological order"
        );
    }
}
