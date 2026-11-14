//! iCloud Contacts over the `contacts` web service, as icloud.com's own
//! Contacts reaches it. Reads are GETs, writes are POSTs of a JSON body:
//!
//! - `GET {contacts}/co/startup`: the tokens every later call carries
//!   (`prefToken`, `syncToken`), the "me" card's id, and the first 500
//!   contacts (unused: [`Client::fetch`] reads them all from the next call).
//! - `GET {contacts}/co/contacts?prefToken&syncToken&limitTo=0&offset=0`:
//!   every contact.
//! - `POST {contacts}/co/contacts/card/?prefToken&syncToken[&method=PUT|DELETE]`
//!   with `{"contacts": [card]}`: creates (no method), replaces (`PUT`, the
//!   whole card, with its `etag`) or deletes (`DELETE`, `contactId` and
//!   `etag`).
//!
//! There is no public description of it. The shapes here were checked
//! against iCloud itself, and two of its answers are easy to misread:
//!
//! - A write's answer is a changeset: its `contacts` are every card changed
//!   since the `syncToken` the request carried, which can include other
//!   people's cards before (or instead of) the one written. The card written
//!   is always found by its `contactId`, never by position.
//! - A refused card is still HTTP 200, listed in `errors` with its own
//!   `errorCode` (412 when its `etag` is stale: it changed elsewhere).
//!
//! A `syncToken` iCloud no longer accepts is HTTP 420 ("Invalid sync
//! token"); [`Error::is_stale_token`] says so, and the caller asks
//! `/co/startup` for fresh tokens and tries once more. Tokens are base64 and
//! must be percent-encoded in the query.
//!
//! All HTTP goes through [`SessionTransport`] (icloud-app's), over `icloud_session::Session`;
//! the tests point that at a fake server with icloud-session's mock mode.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub use icloud_app::SessionTransport;

/// The query every call carries (icloud.com's own).
const QUERY: &str = "clientVersion=2.1&locale=en_US&order=last%2Cfirst";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Anything icloud-session failed with: signed out, offline, an HTTP
    /// status, the daemon or the keyring out of reach, an answer that is not
    /// JSON. Kept whole, so the command line reports it as every iCloud
    /// tool does.
    #[error(transparent)]
    Session(#[from] icloud_session::Error),
    /// iCloud answered 200 but refused the card (412: changed elsewhere).
    #[error("iCloud refused the change ({code}): {message}")]
    Refused { code: i64, message: String },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The iCloud web session is gone: the window shows its sign-in banner.
    pub fn is_sign_in(&self) -> bool {
        matches!(self, Error::Session(icloud_session::Error::SignInRequired))
    }

    /// The card changed on another device since its `etag` was read.
    pub fn is_conflict(&self) -> bool {
        matches!(self, Error::Refused { code: 412, .. })
    }

    /// iCloud no longer accepts the tokens the request carried.
    pub fn is_stale_token(&self) -> bool {
        matches!(self, Error::Session(icloud_session::Error::Http { status: 420, .. }))
    }
}

fn bad_response(msg: String) -> Error {
    icloud_session::Error::BadResponse(msg).into()
}

/// The pair every call after `/co/startup` carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub pref: String,
    pub sync: String,
}

impl Tokens {
    fn query(&self) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("prefToken", &self.pref)
            .append_pair("syncToken", &self.sync)
            .finish()
    }

    /// The tokens a write's answer carries, else these.
    fn after(&self, answer: &Value) -> Tokens {
        let take = |k: &str, old: &str| answer[k].as_str().unwrap_or(old).to_owned();
        Tokens {
            pref: take("prefToken", &self.pref),
            sync: take("syncToken", &self.sync),
        }
    }
}

/// Every contact, as iCloud sent them.
#[derive(Debug)]
pub struct Fetched {
    pub tokens: Tokens,
    /// The "me" card's contactId, when the account has one.
    pub me: Option<String>,
    pub cards: Vec<Map<String, Value>>,
}

pub struct Client<'a> {
    t: &'a SessionTransport,
    base: String,
}

impl<'a> Client<'a> {
    pub fn connect(t: &'a SessionTransport) -> Result<Self> {
        let base = t
            .service_url("contacts")?
            .ok_or_else(|| Error::Other("iCloud did not offer the contacts service for this account".into()))?;
        Ok(Client {
            t,
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    /// Fresh tokens and the "me" card's id.
    pub fn startup(&self) -> Result<(Tokens, Option<String>)> {
        let v = self.t.get_json(&format!("{}/co/startup?{QUERY}", self.base))?;
        let token = |k: &str| {
            v[k].as_str()
                .map(str::to_owned)
                .ok_or_else(|| bad_response(format!("/co/startup has no {k}")))
        };
        let tokens = Tokens {
            pref: token("prefToken")?,
            sync: token("syncToken")?,
        };
        let me = v["meCardId"].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        Ok((tokens, me))
    }

    /// Every contact, with fresh tokens.
    pub fn fetch(&self) -> Result<Fetched> {
        let (tokens, me) = self.startup()?;
        let mut v = self.t.get_json(&format!(
            "{}/co/contacts?{QUERY}&{}&limitTo=0&offset=0",
            self.base,
            tokens.query()
        ))?;
        let Value::Array(cards) = v["contacts"].take() else {
            return Err(bad_response("/co/contacts has no contacts".into()));
        };
        let cards = cards
            .into_iter()
            .filter_map(|c| match c {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .collect();
        Ok(Fetched { tokens, me, cards })
    }

    fn card_call(&self, tokens: &Tokens, method: Option<&str>, card: Value) -> Result<Value> {
        let method = method.map(|m| format!("&method={m}")).unwrap_or_default();
        let url = format!("{}/co/contacts/card/?{QUERY}&{}{method}", self.base, tokens.query());
        Ok(self.t.post_json(&url, &json!({ "contacts": [card] }))?)
    }

    /// Creates `card` (`method` `None`; the card carries its new
    /// `contactId`) or replaces it (`Some("PUT")`: whole, so a field left
    /// out is removed, as of its `etag`). Answers the card as saved and the
    /// tokens to carry next.
    pub fn write(
        &self,
        tokens: &Tokens,
        method: Option<&str>,
        card: &Map<String, Value>,
    ) -> Result<(Map<String, Value>, Tokens)> {
        let id = card
            .get("contactId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("a card to write needs its contactId".into()))?;
        let answer = self.card_call(tokens, method, Value::Object(card.clone()))?;
        saved(&answer, id, tokens)
    }

    /// Deletes the card `id` as of its `etag`; answers the tokens to carry next.
    pub fn delete(&self, tokens: &Tokens, id: &str, etag: &str) -> Result<Tokens> {
        let answer = self.card_call(tokens, Some("DELETE"), json!({ "contactId": id, "etag": etag }))?;
        refused(&answer, id)?;
        let deleted = answer["deletes"]["contactIds"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|d| d == id));
        if !deleted {
            return Err(bad_response(format!(
                "iCloud's answer does not list contact {id} as deleted"
            )));
        }
        Ok(tokens.after(&answer))
    }
}

/// The card's own refusal in `errors`, if any.
fn refused(answer: &Value, id: &str) -> Result<()> {
    let Some(errors) = answer["errors"].as_array() else {
        return Ok(());
    };
    match errors.iter().find(|e| e["contactId"] == id).or(errors.first()) {
        Some(e) => Err(Error::Refused {
            code: e["errorCode"].as_i64().unwrap_or(0),
            message: e["errorMessage"].as_str().unwrap_or("no reason given").to_owned(),
        }),
        None => Ok(()),
    }
}

/// The card `id` in a write's answer: found by its id, since the answer is a
/// changeset that may hold other cards too.
fn saved(answer: &Value, id: &str, tokens: &Tokens) -> Result<(Map<String, Value>, Tokens)> {
    refused(answer, id)?;
    let card = answer["contacts"]
        .as_array()
        .and_then(|cards| cards.iter().find(|c| c["contactId"] == id))
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| bad_response(format!("iCloud's answer has no contact {id}")))?;
    Ok((card, tokens.after(answer)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64_tokens() {
        let t = Tokens {
            pref: "p+/=".into(),
            sync: "HwoQ/F8+q==".into(),
        };
        assert_eq!(t.query(), "prefToken=p%2B%2F%3D&syncToken=HwoQ%2FF8%2Bq%3D%3D");
    }

    #[test]
    fn a_write_answer_is_read_by_id_not_position() {
        let answer = json!({
            "syncToken": "s2",
            "contacts": [
                { "contactId": "OTHER", "firstName": "Someone else", "etag": "e1" },
                { "contactId": "MINE", "firstName": "Ann", "etag": "e2" },
            ],
        });
        let tokens = Tokens {
            pref: "p".into(),
            sync: "s1".into(),
        };
        let (card, after) = saved(&answer, "MINE", &tokens).unwrap();
        assert_eq!(card["firstName"], "Ann");
        assert_eq!(
            after,
            Tokens {
                pref: "p".into(),
                sync: "s2".into()
            }
        );
        assert!(matches!(
            saved(&answer, "GONE", &tokens),
            Err(Error::Session(icloud_session::Error::BadResponse(_)))
        ));
    }

    #[test]
    fn a_refused_card_is_an_error() {
        let answer = json!({ "errors": [{ "contactId": "MINE", "errorCode": 412, "errorMessage": "Persist Error" }] });
        let e = saved(&answer, "MINE", &Tokens::default()).unwrap_err();
        assert!(e.is_conflict(), "{e}");
    }
}
