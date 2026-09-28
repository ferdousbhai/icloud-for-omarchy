//! The cookie jar: icloud.com cookies with their expiry, merged by name.
//! Header parsing is ported from icloud-md's `session.js`
//! (`parseCookieHeader`, `parseSetCookieName`, `mergeSetCookiesIntoSession`).

use serde::{Deserialize, Serialize};

/// The persistent sign-in cookie; its expiry is the session's `ExpiresAt`.
pub const TOKEN: &str = "X-APPLE-WEBAUTH-TOKEN";
/// Find My's authorization, a session cookie set once the password has
/// been entered on www.icloud.com/find; without it `findme` answers 450.
pub const FIND_MY: &str = "X-APPLE-WEBAUTH-FMIP";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    #[serde(default = "default_domain")]
    pub domain: String,
    #[serde(default = "default_path")]
    pub path: String,
    /// Unix seconds; `None` for a session cookie.
    #[serde(default)]
    pub expires: Option<u64>,
}

fn default_domain() -> String {
    ".icloud.com".into()
}

fn default_path() -> String {
    "/".into()
}

impl Cookie {
    pub fn new(name: &str, value: &str) -> Cookie {
        Cookie {
            name: name.into(),
            value: value.into(),
            domain: default_domain(),
            path: default_path(),
            expires: None,
        }
    }
}

/// True for icloud.com and any subdomain, with or without a leading dot.
pub fn is_icloud_domain(domain: &str) -> bool {
    let host = domain.trim_start_matches('.');
    host == "icloud.com" || host.ends_with(".icloud.com")
}

/// `Name1=Value1; Name2=Value2` of the cookies not yet expired at `now`.
pub fn header(cookies: &[Cookie], now: u64) -> String {
    cookies
        .iter()
        .filter(|c| c.expires.is_none_or(|e| e > now))
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The token cookie's expiry, 0 when it is a session cookie or missing.
pub fn token_expiry(cookies: &[Cookie]) -> u64 {
    cookies
        .iter()
        .find(|c| c.name == TOKEN)
        .and_then(|c| c.expires)
        .unwrap_or(0)
}

/// The jar holds an unexpired Find My cookie.
pub fn find_my_cookie(cookies: &[Cookie], now: u64) -> bool {
    cookies
        .iter()
        .any(|c| c.name == FIND_MY && c.expires.is_none_or(|e| e > now))
}

/// Removes the cookie named `name`. Returns whether there was one.
pub fn remove(cookies: &mut Vec<Cookie>, name: &str) -> bool {
    let before = cookies.len();
    cookies.retain(|c| c.name != name);
    cookies.len() != before
}

/// Parses a `Name1=Value1; Name2=Value2` header into ordered pairs. A name
/// seen twice keeps its first position and its last value, like a JS `Map`.
pub fn parse_cookie_header(header: &str) -> Vec<(String, String)> {
    let mut cookies: Vec<(String, String)> = Vec::new();
    for part in header.split(';') {
        let part = part.trim();
        let Some(eq) = part.find('=') else { continue };
        let (name, value) = (&part[..eq], &part[eq + 1..]);
        match cookies.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = value.to_string(),
            None => cookies.push((name.to_string(), value.to_string())),
        }
    }
    cookies
}

/// One parsed `Set-Cookie` header.
#[derive(Debug, PartialEq, Eq)]
pub struct SetCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    /// Unix seconds from `Max-Age` (preferred) or `Expires`.
    pub expires: Option<u64>,
}

pub fn parse_set_cookie(header: &str, now: u64) -> Option<SetCookie> {
    let mut parts = header.split(';');
    let first = parts.next()?.trim();
    let eq = first.find('=')?;
    let name = first[..eq].trim();
    if name.is_empty() {
        return None;
    }
    let mut cookie = SetCookie {
        name: name.to_string(),
        value: first[eq + 1..].to_string(),
        domain: None,
        path: None,
        expires: None,
    };
    let mut max_age = None;
    for attr in parts {
        let (key, value) = match attr.find('=') {
            Some(i) => (attr[..i].trim(), attr[i + 1..].trim()),
            None => (attr.trim(), ""),
        };
        match key.to_ascii_lowercase().as_str() {
            "domain" if !value.is_empty() => cookie.domain = Some(value.to_string()),
            "path" if !value.is_empty() => cookie.path = Some(value.to_string()),
            "expires" => {
                cookie.expires = parse_cookie_date(value);
            }
            "max-age" => {
                if let Ok(secs) = value.parse::<i64>() {
                    max_age = Some(if secs <= 0 { 0 } else { now.saturating_add(secs as u64) });
                }
            }
            _ => {}
        }
    }
    if max_age.is_some() {
        cookie.expires = max_age;
    }
    Some(cookie)
}

/// A cookie `Expires` date as unix seconds, parsed leniently as RFC 6265
/// §5.1.1 says to: servers send more than the strict HTTP-date forms (Apple
/// sends `Wed, 28-Oct-2026 11:47:33 GMT`), and a date that fails to parse
/// would turn a 30-day cookie into a session one.
fn parse_cookie_date(value: &str) -> Option<u64> {
    let is_delimiter = |c: char| {
        let c = c as u32;
        c == 0x09
            || (0x20..=0x2F).contains(&c)
            || (0x3B..=0x40).contains(&c)
            || (0x5B..=0x60).contains(&c)
            || (0x7B..=0x7E).contains(&c)
    };
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let leading_digits = |t: &str, min: usize, max: usize| -> Option<u64> {
        let n = t.chars().take_while(char::is_ascii_digit).count();
        (min..=max).contains(&n).then(|| t[..n].parse().ok()).flatten()
    };
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for token in value.split(is_delimiter).filter(|t| !t.is_empty()) {
        if time.is_none() {
            let mut parts = token.splitn(3, ':');
            if let (Some(h), Some(m), Some(s)) = (parts.next(), parts.next(), parts.next())
                && let (Some(h), Some(m), Some(s)) = (
                    leading_digits(h, 1, 2),
                    leading_digits(m, 1, 2),
                    leading_digits(s, 1, 2),
                )
            {
                time = Some((h, m, s));
                continue;
            }
        }
        if day.is_none()
            && let Some(d) = leading_digits(token, 1, 2)
        {
            day = Some(d);
            continue;
        }
        if month.is_none() && token.len() >= 3 {
            let prefix = token[..3].to_ascii_lowercase();
            if let Some(i) = MONTHS.iter().position(|m| *m == prefix) {
                month = Some(i as u64 + 1);
                continue;
            }
        }
        if year.is_none()
            && let Some(y) = leading_digits(token, 2, 4)
        {
            year = Some(match y {
                70..=99 => y + 1900,
                0..=69 => y + 2000,
                _ => y,
            });
        }
    }
    let ((h, m, s), day, month, year) = (time?, day?, month?, year?);
    if !(1..=31).contains(&day) || year < 1601 || h > 23 || m > 59 || s > 59 {
        return None;
    }
    // Days from 1970-01-01 to the date (Howard Hinnant's days_from_civil).
    let (y, mo) = (year as i64 - i64::from(month <= 2), month as i64);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + (h * 3600 + m * 60 + s) as i64;
    u64::try_from(secs).ok()
}

/// Applies `Set-Cookie` headers to the jar by name: existing cookies update
/// in place, new names are appended, an already-expired one (Apple's way of
/// deleting) is removed. Returns whether anything changed.
pub fn merge_set_cookies<S: AsRef<str>>(jar: &mut Vec<Cookie>, headers: &[S], now: u64) -> bool {
    let mut changed = false;
    for header in headers {
        let Some(set) = parse_set_cookie(header.as_ref(), now) else {
            continue;
        };
        let pos = jar.iter().position(|c| c.name == set.name);
        if set.expires.is_some_and(|e| e <= now) {
            if let Some(i) = pos {
                jar.remove(i);
                changed = true;
            }
            continue;
        }
        let cookie = match pos {
            Some(i) => &mut jar[i],
            None => {
                jar.push(Cookie::new(&set.name, ""));
                changed = true;
                jar.last_mut().expect("just pushed")
            }
        };
        if cookie.value != set.value || cookie.expires != set.expires {
            changed = true;
        }
        cookie.value = set.value;
        cookie.expires = set.expires;
        if let Some(domain) = set.domain {
            cookie.domain = domain;
        }
        if let Some(path) = set.path {
            cookie.path = path;
        }
    }
    changed
}

/// Adopts the values of a plain cookie header (icloud-md's jar) by name.
/// Known cookies keep their expiry; new names become session cookies.
pub fn adopt_header(jar: &mut Vec<Cookie>, header: &str) -> bool {
    let mut changed = false;
    for (name, value) in parse_cookie_header(header) {
        match jar.iter_mut().find(|c| c.name == name) {
            Some(c) if c.value == value => {}
            Some(c) => {
                c.value = value;
                changed = true;
            }
            None => {
                jar.push(Cookie::new(&name, &value));
                changed = true;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_790_000_000;

    fn jar(pairs: &[(&str, &str)]) -> Vec<Cookie> {
        pairs.iter().map(|(n, v)| Cookie::new(n, v)).collect()
    }

    #[test]
    fn rotation_updates_in_place_and_appends_new_names() {
        let mut j = jar(&[("A", "1"), (TOKEN, "old"), ("B", "2")]);
        let changed = merge_set_cookies(
            &mut j,
            &[
                "X-APPLE-WEBAUTH-TOKEN=new; Domain=.icloud.com; Path=/; Max-Age=2592000; Secure; HttpOnly",
                "C=3; Path=/",
            ],
            NOW,
        );
        assert!(changed);
        assert_eq!(header(&j, NOW), "A=1; X-APPLE-WEBAUTH-TOKEN=new; B=2; C=3");
        assert_eq!(token_expiry(&j), NOW + 2_592_000);
    }

    #[test]
    fn cookie_dates_parse_leniently() {
        // 2026-10-28T11:47:33Z
        let want = Some(1_793_188_053);
        assert_eq!(parse_cookie_date("Wed, 28-Oct-2026 11:47:33 GMT"), want);
        assert_eq!(parse_cookie_date("Wed, 28 Oct 2026 11:47:33 GMT"), want);
        assert_eq!(parse_cookie_date("Wednesday, 28-Oct-26 11:47:33 GMT"), want);
        assert_eq!(parse_cookie_date("Wed Oct 28 11:47:33 2026"), want);
        assert_eq!(parse_cookie_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_cookie_date("Sat, 29 Feb 2020 12:00:00 GMT"), Some(1_582_977_600));
        assert_eq!(parse_cookie_date("not a date"), None);
        assert_eq!(parse_cookie_date("Wed, 28-Oct-2026 GMT"), None);
        let set = parse_set_cookie(
            "X-APPLE-WEBAUTH-TOKEN=v; Domain=.icloud.com; Path=/; Expires=Wed, 28-Oct-2026 11:47:33 GMT; Secure",
            NOW,
        )
        .unwrap();
        assert_eq!(set.expires, want);
    }

    #[test]
    fn expires_attribute_and_deletion() {
        let mut j = jar(&[("A", "1"), ("B", "2")]);
        assert!(merge_set_cookies(
            &mut j,
            &[
                "A=x; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
                "B=3; expires=Wed, 21 Oct 2037 07:28:00 GMT"
            ],
            NOW
        ));
        assert_eq!(j.len(), 1);
        assert_eq!(j[0].value, "3");
        assert_eq!(j[0].expires, Some(2_139_722_880));
    }

    #[test]
    fn unchanged_rotation_is_not_a_change() {
        let mut j = jar(&[("A", "1"), ("B", "2")]);
        assert!(!merge_set_cookies(&mut j, &["B=2; Path=/"], NOW));
        assert!(!merge_set_cookies::<&str>(&mut j, &[], NOW));
        assert!(!merge_set_cookies(&mut j, &["garbage", "", "=x"], NOW));
        assert!(!merge_set_cookies(&mut j, &["Gone=1; Max-Age=0"], NOW));
    }

    #[test]
    fn values_may_contain_equals_and_quotes() {
        let mut j = jar(&[("T", "\"v=1:a==\"")]);
        merge_set_cookies(&mut j, &["T=\"v=1:b==\"; Secure"], NOW);
        assert_eq!(header(&j, NOW), "T=\"v=1:b==\"");
    }

    #[test]
    fn expired_cookies_are_not_sent() {
        let mut j = jar(&[("A", "1"), ("B", "2")]);
        j[0].expires = Some(NOW - 1);
        assert_eq!(header(&j, NOW), "B=2");
    }

    #[test]
    fn duplicate_names_keep_first_position_last_value() {
        let parsed = parse_cookie_header("A=1; B=2; A=3;; noequals");
        assert_eq!(parsed, vec![("A".into(), "3".into()), ("B".into(), "2".into())]);
    }

    #[test]
    fn adopting_a_header_keeps_expiry() {
        let mut j = jar(&[(TOKEN, "old"), ("B", "2")]);
        j[0].expires = Some(NOW + 10);
        assert!(adopt_header(&mut j, "X-APPLE-WEBAUTH-TOKEN=new; B=2; C=3"));
        assert_eq!(j[0].value, "new");
        assert_eq!(j[0].expires, Some(NOW + 10));
        assert_eq!(j[2], Cookie::new("C", "3"));
        assert!(!adopt_header(&mut j, "B=2"));
    }

    #[test]
    fn find_my_cookie_and_removal() {
        let mut j = jar(&[("A", "1"), (FIND_MY, "f")]);
        assert!(find_my_cookie(&j, NOW));
        j[1].expires = Some(NOW);
        assert!(!find_my_cookie(&j, NOW));
        assert!(remove(&mut j, FIND_MY));
        assert!(!remove(&mut j, FIND_MY));
        assert_eq!(header(&j, NOW), "A=1");
    }

    #[test]
    fn icloud_domains() {
        assert!(is_icloud_domain(".icloud.com"));
        assert!(is_icloud_domain("setup.icloud.com"));
        assert!(!is_icloud_domain("icloud.com.evil"));
        assert!(!is_icloud_domain("apple.com"));
    }
}
