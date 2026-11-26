//! Times as web services write them: an ISO 8601 stamp, a Retry-After
//! header, and the local clock time a wait ends at.

/// How long a 429 asks to wait: Retry-After in seconds or as an HTTP date,
/// a minute when it says nothing usable, never more than an hour and a half.
pub fn retry_after(header: Option<&str>, now: u64) -> u64 {
    let wait = header
        .map(str::trim)
        .and_then(|h| {
            h.parse::<u64>()
                .ok()
                .or_else(|| http_date(h).map(|t| t.saturating_sub(now)))
        })
        .unwrap_or(60);
    now + wait.clamp(1, 90 * 60)
}

/// `Wed, 21 Oct 2015 07:28:00 GMT` → unix seconds.
pub fn http_date(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    let [_, day, month, year, time, _] = parts.as_slice() else {
        return None;
    };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = MONTHS.iter().position(|x| x.eq_ignore_ascii_case(month))? as i64 + 1;
    let (h, rest) = time.split_once(':')?;
    let (mi, sec) = rest.split_once(':')?;
    let days = days_from_civil(year.parse().ok()?, m, day.parse().ok()?);
    let t = days * 86_400
        + h.parse::<i64>().ok()? * 3600
        + mi.parse::<i64>().ok()? * 60
        + sec.parse::<i64>().ok()?;
    u64::try_from(t).ok()
}

/// `2026-11-04T14:05:09.123-06:00` (or `Z`, or with a space) → unix seconds.
pub fn parse_iso(s: &str) -> Option<u64> {
    let s = s.trim();
    let date = s.get(..10)?;
    let mut d = date.split('-');
    let (y, m, day) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let time = s.get(11..19)?;
    let mut t = time.split(':');
    let (h, mi, sec): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    let rest = &s[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest.as_bytes().first() {
        Some(b'+') | Some(b'-') => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let (oh, om) = rest[1..]
                .split_once(':')
                .unwrap_or((rest.get(1..3)?, rest.get(3..5).unwrap_or("0")));
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60)
        }
        _ => 0,
    };
    u64::try_from(days_from_civil(y, m, day) * 86_400 + h * 3600 + mi * 60 + sec - offset).ok()
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "14:05" in local time.
pub fn clock(t: u64) -> String {
    match crate::platform::local_time(t) {
        Some(l) => format!("{:02}:{:02}", l.hour, l.minute),
        None => format!("{:02}:{:02} UTC", t % 86_400 / 3600, t % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limits_honor_retry_after() {
        let now = 1_000_000;
        assert_eq!(retry_after(Some("120"), now), now + 120);
        assert_eq!(retry_after(None, now), now + 60);
        assert_eq!(retry_after(Some("soon"), now), now + 60);
        assert_eq!(retry_after(Some("999999"), now), now + 90 * 60);
        // 2015-10-21 07:28:00 UTC is 1445412480.
        assert_eq!(
            http_date("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(1_445_412_480)
        );
        assert_eq!(
            retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT"), 1_445_412_480 - 30),
            1_445_412_480
        );
    }

    #[test]
    fn iso_times() {
        assert_eq!(parse_iso("2015-10-21T07:28:00Z"), Some(1_445_412_480));
        assert_eq!(
            parse_iso("2015-10-21T01:28:00.123-06:00"),
            Some(1_445_412_480)
        );
        assert_eq!(parse_iso("2015-10-21 07:28:00"), Some(1_445_412_480));
    }
}
