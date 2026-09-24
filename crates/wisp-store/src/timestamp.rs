use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::StoreError;

/// Formats the current time as RFC 3339 UTC, the timestamp format used for
/// every stored column.
pub(crate) fn now() -> Result<String, StoreError> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

/// Parses a stored RFC 3339 UTC timestamp.
pub(crate) fn parse(text: &str) -> Result<OffsetDateTime, StoreError> {
    Ok(OffsetDateTime::parse(text, &Rfc3339)?)
}
