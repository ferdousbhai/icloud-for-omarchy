//! Contacts as vCard 3.0 (RFC 2426), the format every address book
//! imports: `icloud-contacts export`. Only what [`Contact`] reads is
//! written; the photo is not.

use crate::model::{Contact, Field, Multi};

/// Escapes a text value: backslash, comma, semicolon and newlines.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace(';', "\\;")
        .replace("\r\n", "\\n")
        .replace('\n', "\\n")
}

/// A `TYPE=` parameter from an iCloud label: the standard ones as vCard
/// names them, a custom one quoted.
fn kind(label: &str) -> String {
    match label {
        "MOBILE" | "IPHONE" => "CELL".into(),
        "" => "OTHER".into(),
        l if l.chars().all(|c| c.is_ascii_uppercase()) => l.into(),
        l => format!("\"{}\"", l.replace('"', "'")),
    }
}

/// Folds a line at 75 octets (on a character boundary), as RFC 2425 asks.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut len = 0;
    for ch in line.chars() {
        if len + ch.len_utf8() > 75 {
            out.push_str("\r\n ");
            len = 1;
        }
        out.push(ch);
        len += ch.len_utf8();
    }
    out.push_str("\r\n");
    out
}

pub fn card(c: &Contact) -> String {
    let mut lines = vec!["BEGIN:VCARD".to_owned(), "VERSION:3.0".to_owned()];
    let n = ["lastName", "firstName", "middleName", "prefix", "suffix"].map(|k| esc(c.get(k)));
    lines.push(format!("N:{}", n.join(";")));
    lines.push(format!("FN:{}", esc(&c.display_name())));
    for (prop, f) in [
        ("NICKNAME", Field::Nickname),
        ("TITLE", Field::JobTitle),
        ("NOTE", Field::Notes),
    ] {
        let v = c.field(f);
        if !v.is_empty() {
            lines.push(format!("{prop}:{}", esc(v)));
        }
    }
    let (company, dept) = (c.field(Field::Company), c.field(Field::Department));
    if !company.is_empty() || !dept.is_empty() {
        lines.push(format!("ORG:{};{}", esc(company), esc(dept)));
    }
    if c.is_company() {
        lines.push("X-ABShowAs:COMPANY".into());
    }
    for p in c.values(Multi::Phone) {
        lines.push(format!("TEL;TYPE={}:{}", kind(&p.label), esc(&p.value)));
    }
    for e in c.values(Multi::Email) {
        lines.push(format!("EMAIL;TYPE=INTERNET,{}:{}", kind(&e.label), esc(&e.value)));
    }
    for u in c.values(Multi::Url) {
        lines.push(format!("URL;TYPE={}:{}", kind(&u.label), esc(&u.value)));
    }
    for a in c.addresses() {
        let parts = ["", "", &a.street, &a.city, &a.state, &a.postal_code, &a.country].map(esc);
        lines.push(format!("ADR;TYPE={}:{}", kind(&a.label), parts.join(";")));
    }
    let bday = c.field(Field::Birthday);
    if !bday.is_empty() {
        // A birthday without a year is --MM-DD in vCard.
        lines.push(format!(
            "BDAY:{}",
            bday.strip_prefix("1604").map_or(bday.to_owned(), |md| format!("-{md}"))
        ));
    }
    lines.push(format!("UID:{}", esc(c.id())));
    lines.push("END:VCARD".into());
    lines.iter().map(|l| fold(l)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_a_vcard() {
        let c = Contact(
            json!({
                "contactId": "ID1", "firstName": "Ann", "lastName": "Lee, Jr",
                "companyName": "Acme; Inc", "birthday": "1604-02-29", "notes": "line 1\nline 2",
                "phones": [{"label": "IPHONE", "field": "+1 555"}, {"label": "WhatsApp", "field": "+62 8"}],
                "emailAddresses": [{"label": "WORK", "field": "ann@acme.com"}],
                "streetAddresses": [{"label": "HOME", "field": {"street": "1 Main St", "city": "Oslo", "countryCode": "no"}}],
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let v = card(&c);
        for line in [
            "N:Lee\\, Jr;Ann;;;\r\n",
            "FN:Ann Lee\\, Jr\r\n",
            "ORG:Acme\\; Inc;\r\n",
            "TEL;TYPE=CELL:+1 555\r\n",
            "TEL;TYPE=\"WhatsApp\":+62 8\r\n",
            "EMAIL;TYPE=INTERNET,WORK:ann@acme.com\r\n",
            "ADR;TYPE=HOME:;;1 Main St;Oslo;;;\r\n",
            "BDAY:--02-29\r\n",
            "NOTE:line 1\\nline 2\r\n",
        ] {
            assert!(v.contains(line), "{line:?} in\n{v}");
        }
        assert!(v.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n") && v.ends_with("END:VCARD\r\n"));
    }

    #[test]
    fn folds_long_lines() {
        let folded = fold(&"é".repeat(60));
        assert!(folded.split("\r\n").all(|l| l.len() <= 75));
    }
}
