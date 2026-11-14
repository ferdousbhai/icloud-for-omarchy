//! The add and edit dialog's fields as typed, and the [`Change`]s from what
//! the dialog opened with to them. No GTK here: the window
//! (`src/bin/app/window.rs`) reads its rows into a [`Draft`], and the tests
//! drive the same code.

use crate::model::{self, Change, Contact, Field, Multi};

/// The dialog's single-line fields, in the order shown (two groups of
/// three, then the rest).
pub const SCALARS: &[(Field, &str)] = &[
    (Field::First, "First name"),
    (Field::Last, "Last name"),
    (Field::Company, "Company"),
    (Field::JobTitle, "Job title"),
    (Field::Nickname, "Nickname"),
    (Field::Birthday, "Birthday (1990-05-01, or 05-01)"),
];

/// The dialog's fields as typed: values stored as iCloud stores them
/// (birthday excepted: as typed).
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub scalars: Vec<(Field, String)>,
    pub multis: Vec<(Multi, Vec<Row>)>,
    pub notes: String,
    /// The card has a middle name, which the dialog does not show: it names
    /// someone, as it does for `icloud-contacts add --middle`.
    pub middle_name: bool,
}

/// One phone, email or URL row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Which of the card's stored values (of this kind, in order) the row
    /// was filled with; `None` for one typed anew. Two rows holding the same
    /// value are told apart by it.
    pub origin: Option<usize>,
    pub label: String,
    pub value: String,
}

impl Draft {
    pub fn of(c: &Contact) -> Draft {
        Draft {
            scalars: SCALARS
                .iter()
                .map(|(f, _)| {
                    let v = c.field(*f);
                    (
                        *f,
                        if *f == Field::Birthday {
                            model::birthday_display(v)
                        } else {
                            v.to_owned()
                        },
                    )
                })
                .collect(),
            multis: [Multi::Phone, Multi::Email, Multi::Url]
                .into_iter()
                .map(|m| {
                    let rows = (c.values(m).into_iter().enumerate())
                        .map(|(i, l)| Row {
                            origin: Some(i),
                            label: l.label,
                            value: l.value,
                        })
                        .collect();
                    (m, rows)
                })
                .collect(),
            notes: c.field(Field::Notes).to_owned(),
            middle_name: !c.field(Field::Middle).trim().is_empty(),
        }
    }

    pub fn scalar(&self, f: Field) -> &str {
        self.scalars.iter().find(|(g, _)| *g == f).map_or("", |(_, v)| v)
    }

    pub fn values(&self, m: Multi) -> &[Row] {
        self.multis.iter().find(|(n, _)| *n == m).map_or(&[], |(_, v)| v)
    }

    /// What is wrong with it, while something is. Only what changed from
    /// `before` is checked: a value iCloud already holds (a phone with no
    /// digits, typed on another device) does not keep the rest of an edit
    /// from being saved.
    pub fn problem(&self, before: &Draft) -> Option<String> {
        for c in self.changes(before) {
            let checked = match c {
                Change::Set(Field::Birthday, _) => model::parse_birthday(self.scalar(Field::Birthday)).map(drop),
                Change::Add(m, l) => m.check(&l.value),
                _ => Ok(()),
            };
            if let Err(e) = checked {
                return Some(e);
            }
        }
        (!before.middle_name && !model::names_someone(&self.changes(&Draft::default())))
            .then(|| "A contact needs a name, a company, a phone number or an email.".into())
    }

    /// The changes from `before` to this draft. A row's text is read back
    /// trimmed, so a stored value with spaces around it is the same value:
    /// left alone, not removed and written again.
    pub fn changes(&self, before: &Draft) -> Vec<Change> {
        let mut out = Vec::new();
        for (f, v) in &self.scalars {
            let v = v.trim();
            if v != before.scalar(*f).trim() {
                let v = if *f == Field::Birthday {
                    model::parse_birthday(v).unwrap_or_else(|_| v.to_owned())
                } else {
                    v.to_owned()
                };
                out.push(Change::Set(*f, v));
            }
        }
        if self.notes != before.notes {
            out.push(Change::Set(Field::Notes, self.notes.clone()));
        }
        for (m, now) in &self.multis {
            let was = before.values(*m);
            let labeled = |r: &Row| model::Labeled {
                label: r.label.clone(),
                value: r.value.clone(),
            };
            // A stored value goes when its row is gone or retyped: that one
            // entry, also when another holds the same. The last first, so
            // the count of those before each stays as it was.
            for (i, w) in was.iter().enumerate().rev() {
                let kept = now
                    .iter()
                    .any(|r| r.origin == Some(i) && r.value.trim() == w.value.trim());
                if !kept {
                    let nth = was[..i]
                        .iter()
                        .filter(|o| o.label == w.label && o.value == w.value)
                        .count();
                    out.push(Change::RemoveExact(*m, labeled(w), nth));
                }
            }
            // Added: a row typed anew, or retyped. One that is a copy of
            // another row is kept should another device remove that other
            // meanwhile (an Add the card already has is no change).
            for r in now {
                let retyped = r
                    .origin
                    .is_none_or(|i| was.get(i).is_none_or(|w| w.value.trim() != r.value.trim()));
                if retyped {
                    out.push(Change::Add(*m, labeled(r)));
                }
            }
        }
        out
    }
}

/// The label and value a dialog row holds (`label`: the stored value's,
/// or `None` for the row that adds one). A stored value's row keeps its
/// label, whatever it is (one stored with none is shown as "other");
/// the row to add one (`label` `None`) may start with a label.
pub fn typed(m: Multi, label: Option<&str>, text: &str) -> (String, String) {
    match label {
        Some(label) => (label.to_owned(), text.trim().to_owned()),
        None => {
            let l = model::parse_labeled(text, m.default_label());
            (l.label, l.value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(v: serde_json::Value) -> Contact {
        Contact(v.as_object().unwrap().clone())
    }

    fn row(origin: Option<usize>, label: &str, value: &str) -> Row {
        Row {
            origin,
            label: label.into(),
            value: value.into(),
        }
    }

    fn labeled(label: &str, value: &str) -> model::Labeled {
        model::Labeled {
            label: label.into(),
            value: value.into(),
        }
    }

    /// The rows as the window reads them back: each through [`typed`].
    fn read_back(d: &Draft) -> Draft {
        let mut d = d.clone();
        for (m, rows) in &mut d.multis {
            for r in rows.iter_mut() {
                let (label, value) = typed(*m, Some(&r.label), &r.value);
                (r.label, r.value) = (label, value);
            }
        }
        d
    }

    fn set_last(d: &mut Draft, v: &str) {
        d.scalars.iter_mut().find(|(f, _)| *f == Field::Last).unwrap().1 = v.into();
    }

    #[test]
    fn a_draft_changes_only_what_was_edited() {
        let c = card(json!({
            "contactId": "A", "firstName": "Ann", "birthday": "1604-05-01",
            "phones": [{"label": "MOBILE", "field": "+1 555"}, {"label": "WORK", "field": "+1 666"}],
        }));
        let before = Draft::of(&c);
        assert_eq!(before.scalar(Field::Birthday), "05-01");
        assert!(before.changes(&before).is_empty());
        let mut after = before.clone();
        set_last(&mut after, "Lee");
        after.multis[0].1 = vec![row(Some(0), "MOBILE", "+1 555"), row(None, "HOME", "+1 777")];
        assert_eq!(
            after.changes(&before),
            [
                Change::Set(Field::Last, "Lee".into()),
                Change::RemoveExact(Multi::Phone, labeled("WORK", "+1 666"), 0),
                Change::Add(Multi::Phone, labeled("HOME", "+1 777")),
            ]
        );
        assert!(Draft::default().problem(&Draft::default()).is_some());
        assert_eq!(after.problem(&before), None);
        // Emptying one row removes that value only: not another row holding
        // the same number written another way.
        let mut c = card(
            json!({ "phones": [{"label": "HOME", "field": "555-010-0199"}, {"label": "WORK", "field": "+1 555 010 0199"}] }),
        );
        let before = Draft::of(&c);
        let mut after = before.clone();
        after.multis[0].1.remove(0);
        c.apply(&after.changes(&before)).unwrap();
        assert_eq!(c.values(Multi::Phone), [labeled("WORK", "+1 555 010 0199")]);
        // A value stored without a label keeps none when its row is
        // read back: saving the dialog does not turn it into a mobile number.
        let c = card(json!({ "phones": [{"field": "+1 555"}, {"label": "WORK", "field": "+1 666"}] }));
        let before = Draft::of(&c);
        assert_eq!(read_back(&before).changes(&before), []);
        assert_eq!(
            typed(Multi::Phone, None, "work=+1 777"),
            ("WORK".into(), "+1 777".into())
        );
        // A valid phone does not hide an invalid email after it.
        let mut bad = before.clone();
        bad.multis[0].1.push(row(None, "HOME", "+1 888"));
        bad.multis[1].1 = vec![row(None, "HOME", "nope")];
        assert_eq!(
            bad.problem(&before).as_deref(),
            Some("\"nope\" is not an email address")
        );
    }

    /// A value iCloud already holds that this dialog would not accept (an
    /// email with no `@`) does not disable Save for an edit elsewhere.
    #[test]
    fn a_stored_value_the_dialog_would_refuse_does_not_block_another_edit() {
        let c = card(
            json!({ "firstName": "Ann", "emailAddresses": [{"label": "HOME", "field": "nope"}], "birthday": "1990-02-30" }),
        );
        let before = Draft::of(&c);
        let mut after = before.clone();
        set_last(&mut after, "Lee");
        assert_eq!(after.problem(&before), None);
        assert_eq!(after.changes(&before), [Change::Set(Field::Last, "Lee".into())]);
        // A value typed anew is still checked.
        after.multis[1].1.push(row(None, "WORK", "also-nope"));
        assert!(after.problem(&before).is_some());
    }

    /// Saving the dialog leaves a stored value with spaces around it as it
    /// is (its place, its label or none, what else the entry holds): its
    /// row reads back trimmed, and that is not an edit.
    #[test]
    fn a_padded_value_left_alone_is_not_rewritten() {
        let c = card(
            json!({ "firstName": "Ann", "urls": [{"field": " https://p.example/ann ", "x-extra": 1}, {"label": "WORK", "field": "acme.example"}] }),
        );
        let before = Draft::of(&c);
        let mut read = read_back(&before);
        set_last(&mut read, "Lee");
        assert_eq!(read.changes(&before), [Change::Set(Field::Last, "Lee".into())]);
    }

    /// A row retyped into a copy of another row is an addition: when
    /// another device removed that other row meanwhile, what was typed is
    /// kept.
    #[test]
    fn a_row_retyped_into_a_copy_of_another_is_kept() {
        let c = card(
            json!({ "firstName": "Ann", "urls": [{"label": "WORK", "field": "x.org"}, {"label": "WORK", "field": "acme.example"}] }),
        );
        let before = Draft::of(&c);
        let mut after = before.clone();
        after.multis[2].1[1].value = "x.org".into();
        let changes = after.changes(&before);
        assert_eq!(
            changes,
            [
                Change::RemoveExact(Multi::Url, labeled("WORK", "acme.example"), 0),
                Change::Add(Multi::Url, labeled("WORK", "x.org"))
            ]
        );
        let mut same = c.clone();
        same.apply(&changes).unwrap();
        assert_eq!(same.values(Multi::Url), [labeled("WORK", "x.org")]);
        let mut elsewhere = card(json!({ "firstName": "Ann", "urls": [{"label": "WORK", "field": "acme.example"}] }));
        elsewhere.apply(&changes).unwrap();
        assert_eq!(elsewhere.values(Multi::Url), [labeled("WORK", "x.org")]);
    }

    /// One of two identical entries can be removed: the other stays, with
    /// whatever else it holds.
    #[test]
    fn one_of_two_identical_entries_is_removed_alone() {
        let c = card(json!({ "firstName": "Ann", "phones": [
            {"label": "WORK", "field": "+1 555", "x": 1}, {"label": "HOME", "field": "+1 666"},
            {"label": "WORK", "field": "+1 555", "x": 2},
        ] }));
        for (gone, kept) in [(0, 2), (2, 1)] {
            let before = Draft::of(&c);
            let mut after = before.clone();
            after.multis[0].1.retain(|r| r.origin != Some(gone));
            let mut saved = c.clone();
            saved.apply(&after.changes(&before)).unwrap();
            let phones = saved.0["phones"].as_array().unwrap();
            assert_eq!(phones.len(), 2);
            assert!(phones.iter().any(|p| p["x"] == kept), "{phones:?}");
        }
        // Both go when both rows are emptied.
        let before = Draft::of(&c);
        let mut after = before.clone();
        after.multis[0].1.retain(|r| r.label != "WORK");
        let mut saved = c.clone();
        saved.apply(&after.changes(&before)).unwrap();
        assert_eq!(saved.values(Multi::Phone), [labeled("HOME", "+1 666")]);
    }

    /// A card whose only name is its middle name (not shown in the dialog)
    /// still names someone, as `add --middle` does.
    #[test]
    fn a_middle_name_alone_names_someone() {
        let c = card(json!({ "middleName": "Bea", "notes": "x" }));
        let before = Draft::of(&c);
        let mut after = before.clone();
        after.notes = "y".into();
        assert_eq!(after.problem(&before), None);
        assert!(model::names_someone(&[Change::Set(Field::Middle, "Bea".into())]));
    }

    /// A value stored with no label, retyped, is stored with no label.
    #[test]
    fn a_retyped_value_without_a_label_keeps_none() {
        let mut c = card(json!({ "firstName": "Ann", "phones": [{"field": "+1 555"}] }));
        let before = Draft::of(&c);
        let mut after = before.clone();
        after.multis[0].1[0].value = "+1 777".into();
        c.apply(&after.changes(&before)).unwrap();
        assert_eq!(c.0["phones"], json!([{ "field": "+1 777" }]));
    }
}
