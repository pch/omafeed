//! When a feed is fetched next: after a refresh, after failures, and when its server asks.
//! Pure functions of the time, so the store only records the timestamps they give.
use std::time::SystemTime;

/// The longest any feed waits: a day.
const MAX_WAIT_SECONDS: i64 = 86400;
/// The wait when a server asks for one in a form that cannot be read.
const DEFAULT_RETRY_SECONDS: i64 = 300;

/// When a feed refreshed at `now` is due again, `minutes` later.
pub fn after_success(now: i64, minutes: u32) -> i64 {
    now + i64::from(minutes) * 60
}

/// Seconds to wait after a failure, given the failures in a row before it: one minute,
/// doubling with each, up to about eight and a half hours.
pub fn backoff(earlier_failures: i64) -> i64 {
    (60 * 2_i64.pow(earlier_failures.clamp(0, 9) as u32)).min(MAX_WAIT_SECONDS)
}

/// When a feed that failed at `now` is due again: when its server asked (`retry_after`
/// seconds), or after the backoff for its earlier failures.
pub fn after_failure(now: i64, earlier_failures: i64, retry_after: Option<i64>) -> i64 {
    now + retry_after.unwrap_or_else(|| backoff(earlier_failures))
}

/// The seconds a `Retry-After` header asks for at `now`: a number of seconds or an HTTP date,
/// between one second and a day; five minutes if it cannot be read or the date has passed.
pub fn retry_after(value: &str, now: SystemTime) -> i64 {
    value
        .parse::<i64>()
        .ok()
        .or_else(|| {
            let date = httpdate::parse_http_date(value).ok()?;
            let wait = date.duration_since(now).ok()?;
            Some(wait.as_secs() as i64)
        })
        .unwrap_or(DEFAULT_RETRY_SECONDS)
        .clamp(1, MAX_WAIT_SECONDS)
}

/// The seconds an `X-RateLimit-Reset` header asks for at `now` (a Unix time): seconds until the
/// limit resets, or the Unix time it resets at as some APIs send, between one second and a day.
pub fn rate_limit_reset(value: &str, now: i64) -> Option<i64> {
    let reset = value.trim().parse::<f64>().ok().filter(|r| r.is_finite())? as i64;
    // No wait is a billion seconds long, but every Unix time since 2001 is.
    let wait = if reset > 1_000_000_000 {
        reset - now
    } else {
        reset
    };
    Some(wait.clamp(1, MAX_WAIT_SECONDS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_refreshed_feed_is_due_after_the_refresh_interval() {
        assert_eq!(after_success(1_000, 30), 2_800);
        assert_eq!(after_success(1_000, 1440), 87_400);
    }

    #[test]
    fn failures_back_off_from_a_minute_and_stop_doubling() {
        let waits: Vec<i64> = (0..12).map(backoff).collect();
        assert_eq!(
            waits,
            [
                60, 120, 240, 480, 960, 1920, 3840, 7680, 15360, 30720, 30720, 30720
            ]
        );
        assert_eq!(backoff(-3), 60);
        assert_eq!(after_failure(1_000, 2, None), 1_240);
        assert_eq!(after_failure(1_000, 2, Some(600)), 1_600);
    }

    #[test]
    fn retry_after_reads_seconds_and_dates_within_limits() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(retry_after("120", now), 120);
        assert_eq!(retry_after("0", now), 1);
        assert_eq!(retry_after("-5", now), 1);
        assert_eq!(retry_after("999999", now), 86_400);
        let later = httpdate::fmt_http_date(now + Duration::from_secs(600));
        assert_eq!(retry_after(&later, now), 600);
        let earlier = httpdate::fmt_http_date(now - Duration::from_secs(600));
        assert_eq!(retry_after(&earlier, now), 300);
        assert_eq!(retry_after("soon", now), 300);
    }

    #[test]
    fn rate_limit_resets_read_seconds_and_unix_times_within_limits() {
        let now = 1_700_000_000;
        assert_eq!(rate_limit_reset("45", now), Some(45));
        assert_eq!(rate_limit_reset(" 45.7 ", now), Some(45));
        assert_eq!(rate_limit_reset("0", now), Some(1));
        assert_eq!(rate_limit_reset("1700000600", now), Some(600));
        assert_eq!(rate_limit_reset("1600000000", now), Some(1));
        assert_eq!(rate_limit_reset("999999", now), Some(86_400));
        assert_eq!(rate_limit_reset("soon", now), None);
        assert_eq!(rate_limit_reset("NaN", now), None);
    }
}
