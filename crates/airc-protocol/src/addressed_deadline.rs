//! Required sender expiry carried inside the signed addressed envelope.
//! Hosts must have synchronized wall clocks. Pre-epoch and range errors refuse
//! admission; this module cannot detect clock skew between hosts.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use airc_core::Headers;

pub const EXPIRY_HEADER: &str = "airc.addressed.expires_unix_ms";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineError {
    Missing,
    Malformed,
    Elapsed,
    Clock,
    OutOfRange,
}

/// Encode before signing. Floor to milliseconds so rounding never extends the
/// sender's budget; a sub-millisecond remaining budget may be refused.
pub fn stamp(
    headers: &mut Headers,
    now: SystemTime,
    remaining: Duration,
) -> Result<(), DeadlineError> {
    if remaining.is_zero() {
        return Err(DeadlineError::Elapsed);
    }
    now.duration_since(UNIX_EPOCH)
        .map_err(|_| DeadlineError::Clock)?;
    let expires = now
        .checked_add(remaining)
        .ok_or(DeadlineError::OutOfRange)?;
    let millis = expires
        .duration_since(UNIX_EPOCH)
        .map_err(|_| DeadlineError::Clock)?
        .as_millis();
    let millis = u64::try_from(millis).map_err(|_| DeadlineError::OutOfRange)?;
    headers.insert(EXPIRY_HEADER.into(), millis.to_string());
    Ok(())
}

/// Recompute at every admission/consumption boundary. Never replace absent or
/// expired sender authority with a new local timeout.
pub fn remaining(headers: &Headers, now: SystemTime) -> Result<Duration, DeadlineError> {
    let raw = headers.get(EXPIRY_HEADER).ok_or(DeadlineError::Missing)?;
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(DeadlineError::Malformed);
    }
    let millis = raw.parse::<u64>().map_err(|_| DeadlineError::Malformed)?;
    let expires = UNIX_EPOCH
        .checked_add(Duration::from_millis(millis))
        .ok_or(DeadlineError::OutOfRange)?;
    now.duration_since(UNIX_EPOCH)
        .map_err(|_| DeadlineError::Clock)?;
    let budget = expires
        .duration_since(now)
        .map_err(|_| DeadlineError::Elapsed)?;
    if budget.is_zero() {
        return Err(DeadlineError::Elapsed);
    }
    Ok(budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_is_required_and_never_renewed_by_receipt() {
        let now = UNIX_EPOCH + Duration::from_secs(10);
        let mut headers = Headers::new();
        assert_eq!(remaining(&headers, now), Err(DeadlineError::Missing));
        for value in ["", "+11000", "-1", " 11000", "18446744073709551616"] {
            headers.insert(EXPIRY_HEADER.into(), value.into());
            assert_eq!(remaining(&headers, now), Err(DeadlineError::Malformed));
        }
        stamp(&mut headers, now, Duration::from_secs(1)).unwrap();
        assert_eq!(
            remaining(&headers, now + Duration::from_millis(900)),
            Ok(Duration::from_millis(100))
        );
        assert_eq!(
            remaining(&headers, now + Duration::from_secs(1)),
            Err(DeadlineError::Elapsed)
        );
        assert_eq!(
            remaining(&headers, now + Duration::from_secs(2)),
            Err(DeadlineError::Elapsed)
        );
    }

    #[test]
    fn rounding_cannot_extend_budget_and_pre_epoch_clock_is_refused() {
        let now = UNIX_EPOCH + Duration::from_secs(10);
        let mut headers = Headers::new();
        stamp(&mut headers, now, Duration::from_micros(1500)).unwrap();
        assert_eq!(remaining(&headers, now), Ok(Duration::from_millis(1)));
        assert_eq!(
            remaining(&headers, UNIX_EPOCH - Duration::from_secs(1)),
            Err(DeadlineError::Clock)
        );
        assert_eq!(
            stamp(&mut headers, now, Duration::ZERO),
            Err(DeadlineError::Elapsed)
        );
    }
}
