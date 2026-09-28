//! Cookie jar handling, ported from icloud-md's `session.js`
//! (`parseCookieHeader`, `parseSetCookieName`, `mergeSetCookiesIntoSession`).

/// Parses a `Name1=Value1; Name2=Value2` header into ordered pairs. A name
/// seen twice keeps its first position and its last value, like a JS `Map`.
pub(crate) fn parse_cookie_header(header: &str) -> Vec<(String, String)> {
    let mut cookies: Vec<(String, String)> = Vec::new();
    for part in header.split(';') {
        let part = part.trim();
        let Some(eq) = part.find('=') else { continue };
        set(&mut cookies, &part[..eq], &part[eq + 1..]);
    }
    cookies
}

/// The `Name=Value` pair of one `Set-Cookie` header, attributes ignored.
pub(crate) fn parse_set_cookie(header: &str) -> Option<(&str, &str)> {
    let first = header.split(';').next()?.trim();
    if first.is_empty() {
        return None;
    }
    let eq = first.find('=')?;
    Some((&first[..eq], &first[eq + 1..]))
}

/// Applies the cookies a response rotated to `jar`. Existing cookies keep
/// their order and update in place; new names are appended. Returns `None`
/// when nothing changed, so the caller can skip the write.
pub(crate) fn merge_set_cookies<S: AsRef<str>>(jar: &str, set_cookies: &[S]) -> Option<String> {
    let mut cookies = parse_cookie_header(jar);
    let mut changed = false;
    for header in set_cookies {
        let Some((name, value)) = parse_set_cookie(header.as_ref()) else {
            continue;
        };
        if cookies.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()) != Some(value) {
            changed = true;
        }
        set(&mut cookies, name, value);
    }
    changed.then(|| {
        cookies
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

fn set(cookies: &mut Vec<(String, String)>, name: &str, value: &str) {
    match cookies.iter_mut().find(|(n, _)| n == name) {
        Some(entry) => entry.1 = value.to_string(),
        None => cookies.push((name.to_string(), value.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_updates_in_place_and_appends_new_names() {
        let jar = "A=1; X-APPLE-WEBAUTH-TOKEN=old; B=2";
        let merged = merge_set_cookies(
            jar,
            &[
                "X-APPLE-WEBAUTH-TOKEN=new; Domain=.icloud.com; Path=/; Secure; HttpOnly",
                "C=3; Path=/",
            ],
        )
        .unwrap();
        assert_eq!(merged, "A=1; X-APPLE-WEBAUTH-TOKEN=new; B=2; C=3");
    }

    #[test]
    fn unchanged_rotation_is_none() {
        assert_eq!(merge_set_cookies("A=1; B=2", &["B=2; Path=/"]), None);
        assert_eq!(merge_set_cookies::<&str>("A=1", &[]), None);
        assert_eq!(merge_set_cookies("A=1", &["garbage", ""]), None);
    }

    #[test]
    fn values_may_contain_equals_and_quotes() {
        let merged = merge_set_cookies("T=\"v=1:a==\"", &["T=\"v=1:b==\"; Secure"]).unwrap();
        assert_eq!(merged, "T=\"v=1:b==\"");
    }

    #[test]
    fn duplicate_names_keep_first_position_last_value() {
        let parsed = parse_cookie_header("A=1; B=2; A=3;; noequals");
        assert_eq!(parsed, vec![("A".into(), "3".into()), ("B".into(), "2".into())]);
    }
}
