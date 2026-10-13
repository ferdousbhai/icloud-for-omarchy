//! A contact is kept as the card iCloud sent, whole: a `PUT` replaces the
//! card, so an edit must send back every field it did not touch (the photo,
//! social profiles, related names, dates, whatever this version does not
//! read). [`Contact`] reads the fields people use, and [`Change`]s are
//! applied to a copy of the card.
//!
//! Card fields: `contactId`, `etag`, `firstName`, `middleName`, `lastName`,
//! `prefix`, `suffix`, `nickName`, `companyName`, `jobTitle`, `department`,
//! `isCompany`, `phones`, `emailAddresses`, `urls` (each `[{label, field}]`),
//! `streetAddresses` (`[{label, field: {street, city, state, postalCode,
//! country, countryCode}}]`), `birthday` (`YYYY-MM-DD`), `notes`, `photo`
//! (`{url, crop, signature}`), `dateCreated`, `dateModified`, `normalized`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Contact(pub Map<String, Value>);

/// A phone, email or URL with its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Labeled {
    /// As iCloud stores it: `MOBILE`, `HOME`, `WORK`, ... or a custom one.
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Address {
    pub label: String,
    pub street: String,
    pub city: String,
    pub state: String,
    pub postal_code: String,
    pub country: String,
    pub country_code: String,
}

/// The single-valued fields an edit can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    First,
    Middle,
    Last,
    Nickname,
    Company,
    JobTitle,
    Department,
    Birthday,
    Notes,
}

impl Field {
    pub fn key(self) -> &'static str {
        match self {
            Field::First => "firstName",
            Field::Middle => "middleName",
            Field::Last => "lastName",
            Field::Nickname => "nickName",
            Field::Company => "companyName",
            Field::JobTitle => "jobTitle",
            Field::Department => "department",
            Field::Birthday => "birthday",
            Field::Notes => "notes",
        }
    }
}

/// The multi-valued fields an edit can add to or remove from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multi {
    Phone,
    Email,
    Url,
}

impl Multi {
    pub fn key(self) -> &'static str {
        match self {
            Multi::Phone => "phones",
            Multi::Email => "emailAddresses",
            Multi::Url => "urls",
        }
    }

    /// The label a new value gets when none is given (Contacts' own).
    pub fn default_label(self) -> &'static str {
        match self {
            Multi::Phone => "MOBILE",
            Multi::Email => "HOME",
            Multi::Url => "HOMEPAGE",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Multi::Phone => "phone number",
            Multi::Email => "email address",
            Multi::Url => "URL",
        }
    }

    /// Why `value` can't be one of these, if it can't.
    pub fn check(self, value: &str) -> Result<(), String> {
        match self {
            _ if value.trim().is_empty() => Err(format!("an empty {}", self.noun())),
            Multi::Phone if digits(value).is_empty() => Err(format!("\"{value}\" is not a phone number")),
            Multi::Email if !value.contains('@') => Err(format!("\"{value}\" is not an email address")),
            _ => Ok(()),
        }
    }

    /// Whether `stored` is the value `query` names: phone numbers by their
    /// digits (see [`same_number`]), emails ignoring case, URLs exactly.
    pub fn matches(self, stored: &str, query: &str) -> bool {
        match self {
            Multi::Phone => same_number(stored, query),
            Multi::Email => stored.trim().eq_ignore_ascii_case(query.trim()),
            Multi::Url => stored.trim() == query.trim(),
        }
    }
}

/// One intended change; applied to the card as it is on the server, so a
/// change made on another device meanwhile is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Set a field; empty removes it.
    Set(Field, String),
    Add(Multi, Labeled),
    /// Remove the value (see [`Multi::matches`]).
    Remove(Multi, String),
}

/// Whether two phone numbers are the same: equal digits, or one the other's
/// tail of seven digits or more (`555-010-0199` is `+1 555 010 0199`).
pub fn same_number(a: &str, b: &str) -> bool {
    let (a, b) = (digits(a), digits(b));
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    !short.is_empty() && (short == long || (short.len() >= 7 && long.ends_with(short.as_str())))
}

/// Whether a card made of `changes` names someone: a contact needs a name,
/// a company, a phone number or an email address.
pub fn names_someone(changes: &[Change]) -> bool {
    changes.iter().any(|c| match c {
        Change::Set(Field::First | Field::Middle | Field::Last | Field::Nickname | Field::Company, v) => !v.is_empty(),
        Change::Add(m, _) => matches!(m, Multi::Phone | Multi::Email),
        _ => false,
    })
}

pub fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Labels iCloud stores in capitals, as Contacts shows them.
pub fn label_display(label: &str) -> String {
    match label {
        "" => "other".into(),
        "IPHONE" => "iPhone".into(),
        l if l.chars().any(char::is_lowercase) => l.to_owned(),
        l => l.to_lowercase(),
    }
}

/// A typed label as iCloud stores it: the standard ones in capitals,
/// anything else as typed.
pub fn label_stored(label: &str) -> String {
    const STANDARD: &[&str] = &[
        "MOBILE", "IPHONE", "HOME", "WORK", "MAIN", "OTHER", "PAGER", "HOME FAX", "WORK FAX", "HOMEPAGE", "SCHOOL",
    ];
    let upper = label.trim().to_uppercase();
    if STANDARD.contains(&upper.as_str()) {
        upper
    } else {
        label.trim().to_owned()
    }
}

/// `label=value` or `value` (with `default` as the label). A label is a
/// word or two before the first `=`, so a URL's query is never one.
pub fn parse_labeled(s: &str, default: &str) -> Labeled {
    if let Some((label, value)) = s.split_once('=')
        && !label.trim().is_empty()
        && label
            .chars()
            .all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_')
    {
        return Labeled {
            label: label_stored(label),
            value: value.trim().to_owned(),
        };
    }
    Labeled {
        label: default.to_owned(),
        value: s.trim().to_owned(),
    }
}

/// `YYYY-MM-DD`, or `--MM-DD` / `MM-DD` for a birthday without a year
/// (stored with Apple's placeholder year 1604, as vCard does).
pub fn parse_birthday(s: &str) -> Result<String, String> {
    let s = s.trim();
    let bad = || format!("birthday \"{s}\": use YYYY-MM-DD, or MM-DD without a year");
    let parts: Vec<&str> = s.trim_start_matches("--").split('-').collect();
    let (y, m, d) = match parts.as_slice() {
        [y, m, d] if y.len() == 4 => (y.parse::<u32>().map_err(|_| bad())?, *m, *d),
        [m, d] => (1604, *m, *d),
        _ => return Err(bad()),
    };
    let m: u32 = m.parse().map_err(|_| bad())?;
    let d: u32 = d.parse().map_err(|_| bad())?;
    let days = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=12).contains(&m) || d == 0 || d > days[m as usize - 1] {
        return Err(bad());
    }
    Ok(format!("{y:04}-{m:02}-{d:02}"))
}

/// A stored birthday as people write it: `1990-05-01`, or `05-01` without a year.
pub fn birthday_display(b: &str) -> String {
    b.strip_prefix("1604-").unwrap_or(b).to_owned()
}

fn str_of<'a>(m: &'a Map<String, Value>, k: &str) -> &'a str {
    m.get(k).and_then(Value::as_str).unwrap_or("")
}

impl Contact {
    /// A new card with a fresh `contactId` (an uppercase UUID, as Apple's).
    /// `is_company` shows it by its company name.
    pub fn new(is_company: bool) -> Contact {
        let mut m = Map::new();
        m.insert(
            "contactId".into(),
            json!(uuid::Uuid::new_v4().to_string().to_uppercase()),
        );
        m.insert("isCompany".into(), json!(is_company));
        Contact(m)
    }

    pub fn get(&self, k: &str) -> &str {
        str_of(&self.0, k)
    }

    pub fn id(&self) -> &str {
        self.get("contactId")
    }

    pub fn etag(&self) -> &str {
        self.get("etag")
    }

    pub fn field(&self, f: Field) -> &str {
        self.get(f.key())
    }

    pub fn is_company(&self) -> bool {
        self.0.get("isCompany").and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn has_photo(&self) -> bool {
        self.0.get("photo").is_some_and(|p| p["url"].is_string())
    }

    /// The person's name (prefix, first, middle, last, suffix).
    pub fn person_name(&self) -> String {
        ["prefix", "firstName", "middleName", "lastName", "suffix"]
            .iter()
            .map(|k| self.get(k).trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// What Contacts shows in the list: the company for a company card,
    /// else the name, else whatever there is.
    pub fn display_name(&self) -> String {
        let company = self.field(Field::Company).trim();
        if self.is_company() && !company.is_empty() {
            return company.to_owned();
        }
        let name = self.person_name();
        if !name.is_empty() {
            return name;
        }
        let nick = self.field(Field::Nickname).trim();
        let first_of = |m: Multi| self.values(m).into_iter().next().map(|l| l.value);
        [nick, company]
            .into_iter()
            .find(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| first_of(Multi::Email))
            .or_else(|| first_of(Multi::Phone))
            .unwrap_or_else(|| "No Name".into())
    }

    /// Sorted as Contacts' "last, first": by last name, else the shown name.
    pub fn sort_key(&self) -> (String, String) {
        let last = self.field(Field::Last).trim();
        let primary = if last.is_empty() || self.is_company() {
            self.display_name()
        } else {
            last.to_owned()
        };
        (fold(&primary), fold(self.field(Field::First)))
    }

    /// Job title, department and (for a person) company, joined by `sep`.
    pub fn work(&self, sep: &str) -> String {
        let company = if self.is_company() {
            ""
        } else {
            self.field(Field::Company)
        };
        [self.field(Field::JobTitle), self.field(Field::Department), company]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(sep)
    }

    pub fn values(&self, m: Multi) -> Vec<Labeled> {
        self.0
            .get(m.key())
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let value = e["field"].as_str()?.to_owned();
                Some(Labeled {
                    label: e["label"].as_str().unwrap_or("").to_owned(),
                    value,
                })
            })
            .collect()
    }

    pub fn addresses(&self) -> Vec<Address> {
        self.0
            .get("streetAddresses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let f = e["field"].as_object()?;
                Some(Address {
                    label: e["label"].as_str().unwrap_or("").to_owned(),
                    street: str_of(f, "street").to_owned(),
                    city: str_of(f, "city").to_owned(),
                    state: str_of(f, "state").to_owned(),
                    postal_code: str_of(f, "postalCode").to_owned(),
                    country: str_of(f, "country").to_owned(),
                    country_code: str_of(f, "countryCode").to_owned(),
                })
            })
            .collect()
    }

    /// Whether `query` (folded) names this contact: in its name, nickname,
    /// company or an email, or, with three digits or more and no letters,
    /// in a phone number's digits.
    pub fn matches(&self, query: &str) -> bool {
        let q = fold(query.trim());
        if q.is_empty() {
            return true;
        }
        let texts = [
            self.display_name(),
            self.person_name(),
            self.field(Field::Nickname).to_owned(),
            self.field(Field::Company).to_owned(),
        ];
        if texts.iter().any(|t| fold(t).contains(&q)) {
            return true;
        }
        if self.values(Multi::Email).iter().any(|e| fold(&e.value).contains(&q)) {
            return true;
        }
        let d = digits(&q);
        d.len() >= 3
            && !q.chars().any(char::is_alphabetic)
            && self.values(Multi::Phone).iter().any(|p| digits(&p.value).contains(&d))
    }

    /// Applies `changes` to this card; an error names a value to remove
    /// that is not there.
    pub fn apply(&mut self, changes: &[Change]) -> Result<(), String> {
        for c in changes {
            match c {
                Change::Set(f, v) => {
                    if v.is_empty() {
                        self.0.remove(f.key());
                    } else {
                        self.0.insert(f.key().into(), json!(v));
                    }
                }
                Change::Add(..) if self.has(c) => {}
                Change::Add(m, l) => {
                    let list = self.0.entry(m.key()).or_insert(Value::Null);
                    if !list.is_array() {
                        *list = json!([]);
                    }
                    let arr = list.as_array_mut().expect("just made an array");
                    arr.push(json!({ "label": l.label, "field": l.value }));
                }
                Change::Remove(m, v) if self.has(c) => {
                    return Err(format!("{} has no {} {v}", self.display_name(), m.noun()));
                }
                Change::Remove(m, v) => {
                    let arr = self.0.get_mut(m.key()).and_then(Value::as_array_mut);
                    let arr = arr.expect("has() found the value in an array");
                    arr.retain(|e| !e["field"].as_str().is_some_and(|s| m.matches(s, v)));
                    if arr.is_empty() {
                        self.0.remove(m.key());
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether the card already is as `change` would make it.
    pub fn has(&self, change: &Change) -> bool {
        match change {
            Change::Set(f, v) => self.field(*f) == v,
            Change::Add(m, l) => self
                .values(*m)
                .iter()
                .any(|e| m.matches(&e.value, &l.value) && e.label == l.label),
            Change::Remove(m, v) => !self.values(*m).iter().any(|e| m.matches(&e.value, v)),
        }
    }
}

/// Lowercase, with typographic apostrophes as plain ones.
pub fn fold(s: &str) -> String {
    s.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(v: Value) -> Contact {
        Contact(v.as_object().unwrap().clone())
    }

    #[test]
    fn names_fall_back_like_contacts() {
        assert_eq!(
            card(json!({"firstName": "Ann", "lastName": "Lee"})).display_name(),
            "Ann Lee"
        );
        assert_eq!(
            card(json!({"companyName": "Acme", "isCompany": true, "firstName": "Bo"})).display_name(),
            "Acme"
        );
        assert_eq!(
            card(json!({"phones": [{"label": "MOBILE", "field": "+1 555"}]})).display_name(),
            "+1 555"
        );
        assert_eq!(card(json!({})).display_name(), "No Name");
    }

    #[test]
    fn an_edit_keeps_what_it_does_not_touch() {
        let mut c = card(json!({
            "contactId": "A", "etag": "e", "firstName": "Ann",
            "photo": {"url": "https://p", "crop": {"x": 0}},
            "profiles": [{"field": "x", "user": "ann"}],
            "phones": [{"label": "MOBILE", "field": "+1 (555) 010-0100"}],
        }));
        c.apply(&[
            Change::Set(Field::Last, "Lee".into()),
            Change::Set(Field::First, String::new()),
            Change::Add(Multi::Email, parse_labeled("work=ann@acme.com", "HOME")),
            Change::Remove(Multi::Phone, "15550100100".into()),
        ])
        .unwrap();
        assert_eq!(c.0["lastName"], "Lee");
        assert!(!c.0.contains_key("firstName") && !c.0.contains_key("phones"));
        assert_eq!(c.0["photo"]["url"], "https://p");
        assert_eq!(c.0["profiles"][0]["user"], "ann");
        assert_eq!(
            c.values(Multi::Email),
            [Labeled {
                label: "WORK".into(),
                value: "ann@acme.com".into()
            }]
        );
        assert!(c.apply(&[Change::Remove(Multi::Phone, "999".into())]).is_err());
    }

    #[test]
    fn labels_and_birthdays() {
        assert_eq!(parse_labeled("https://x.org/?a=b", "HOMEPAGE").label, "HOMEPAGE");
        assert_eq!(
            parse_labeled("WhatsApp=+62 812", "MOBILE"),
            Labeled {
                label: "WhatsApp".into(),
                value: "+62 812".into()
            }
        );
        assert_eq!(label_display("IPHONE"), "iPhone");
        assert_eq!(label_display("Ponsel"), "Ponsel");
        assert_eq!(parse_birthday("1990-5-1").unwrap(), "1990-05-01");
        assert_eq!(parse_birthday("--02-29").unwrap(), "1604-02-29");
        assert_eq!(birthday_display("1604-02-29"), "02-29");
        assert!(parse_birthday("1990-13-01").is_err());
    }

    #[test]
    fn matches_names_emails_and_phone_digits() {
        let c = card(
            json!({"firstName": "Zoë", "lastName": "O’Brien", "phones": [{"label": "MOBILE", "field": "+358 46 882"}], "emailAddresses": [{"label": "HOME", "field": "Zoe@Example.com"}]}),
        );
        assert!(c.matches("o'brien"));
        assert!(c.matches("zoe@example"));
        assert!(c.matches("46882"));
        assert!(!c.matches("12"));
    }
}
