//! Small std-only helpers: UTC dates, RFC 3339 timestamps, hosts, IPs and ids.

use std::net::{IpAddr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch (0 for times before it).
fn unix_millis(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX / 2),
        Err(_) => 0,
    }
}

/// (year, month, day) of a count of days since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The UTC date of `t`, `"YYYY-MM-DD"`.
pub(crate) fn utc_day(t: SystemTime) -> String {
    let (y, m, d) = civil_from_days(unix_millis(t).div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// RFC 3339 in UTC with milliseconds: `"2026-10-13T18:04:05.123Z"`.
pub(crate) fn rfc3339(t: SystemTime) -> String {
    let ms = unix_millis(t);
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    let in_day = ms.rem_euclid(86_400_000);
    let (h, min, s, milli) = (
        in_day / 3_600_000,
        (in_day / 60_000) % 60,
        (in_day / 1000) % 60,
        in_day % 1000,
    );
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}.{milli:03}Z")
}

/// A UUID v7 (RFC 9562): a millisecond timestamp, then random bits. The event's idempotency key.
pub(crate) fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Lowercase hostname without port: `"Example.com:8080"` -> `"example.com"`, `"[::1]:3000"` -> `"[::1]"`.
pub(crate) fn normalize_host(host: &str) -> String {
    let h = host.trim().to_ascii_lowercase();
    if h.starts_with('[') {
        return match h.find(']') {
            Some(end) => h[..=end].to_owned(),
            None => h,
        };
    }
    match (h.find(':'), h.rfind(':')) {
        (Some(first), Some(last)) if first == last => h[..first].to_owned(),
        _ => h,
    }
}

/// The hostname of an absolute URL (`scheme://[user@]host[:port]/…`), normalized; empty when there is none.
pub(crate) fn host_of_url(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    normalize_host(host)
}

/// `"::ffff:203.0.113.9"` -> `"203.0.113.9"`, `"203.0.113.9:5678"` -> `"203.0.113.9"`, so the same visitor hashes
/// the same whatever the socket family or proxy format. Anything unparseable is kept as given (trimmed).
pub(crate) fn normalize_ip(ip: &str) -> String {
    let ip = ip.trim();
    if let Ok(addr) = ip.parse::<IpAddr>() {
        return addr.to_canonical().to_string();
    }
    if let Ok(addr) = ip.parse::<SocketAddr>() {
        return addr.ip().to_canonical().to_string();
    }
    ip.to_owned()
}

/// `"1"`, `"true"`, `"on"`, `"yes"` / `"0"`, `"false"`, `"off"`, `"no"`, `""`; anything else is `None`.
pub(crate) fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" | "" => Some(false),
        _ => None,
    }
}

/// `None` for empty strings.
pub(crate) fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    #[test]
    fn dates_and_timestamps_are_utc() {
        assert_eq!(utc_day(at(0)), "1970-01-01");
        assert_eq!(rfc3339(at(0)), "1970-01-01T00:00:00.000Z");
        // 2026-10-13T18:04:05.123Z
        assert_eq!(rfc3339(at(1_791_914_645_123)), "2026-10-13T18:04:05.123Z");
        assert_eq!(utc_day(at(1_791_914_645_123)), "2026-10-13");
        // Leap day and the last millisecond of a day.
        assert_eq!(rfc3339(at(1_709_251_199_999)), "2024-02-29T23:59:59.999Z");
        assert_eq!(utc_day(at(1_709_251_200_000)), "2024-03-01");
        assert_eq!(utc_day(at(951_782_400_000)), "2000-02-29");
    }

    #[test]
    fn ids_are_uuid_v7() {
        let id = new_id();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "7");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(id, new_id());
    }

    #[test]
    fn hosts() {
        assert_eq!(normalize_host("Example.com:8080"), "example.com");
        assert_eq!(normalize_host(" example.com "), "example.com");
        assert_eq!(normalize_host("[::1]:3000"), "[::1]");
        assert_eq!(normalize_host("::1"), "::1");
        assert_eq!(
            host_of_url("https://Shop.Example.com:8443/a?b#c"),
            "shop.example.com"
        );
        assert_eq!(host_of_url("http://user:pw@example.com/"), "example.com");
        assert_eq!(host_of_url("/relative"), "");
    }

    #[test]
    fn ips() {
        assert_eq!(normalize_ip("::ffff:203.0.113.9"), "203.0.113.9");
        assert_eq!(normalize_ip(" 203.0.113.9 "), "203.0.113.9");
        assert_eq!(normalize_ip("203.0.113.9:5678"), "203.0.113.9");
        assert_eq!(normalize_ip("[2001:db8::1]:443"), "2001:db8::1");
        assert_eq!(normalize_ip("unknown"), "unknown");
    }

    #[test]
    fn bools() {
        assert_eq!(parse_bool("False"), Some(false));
        assert_eq!(parse_bool("1"), Some(true));
        assert_eq!(parse_bool("maybe"), None);
    }
}
