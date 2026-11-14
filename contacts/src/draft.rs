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
    pub multis: Vec<(Multi, Vec<(String, String)>)>,
    pub notes: String,
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
                .map(|m| (m, c.values(m).into_iter().map(|l| (l.label, l.value)).collect()))
                .collect(),
            notes: c.field(Field::Notes).to_owned(),
        }
    }

    pub fn scalar(&self, f: Field) -> &str {
        self.scalars.iter().find(|(g, _)| *g == f).map_or("", |(_, v)| v)
    }

    pub fn values(&self, m: Multi) -> &[(String, String)] {
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
        (!model::names_someone(&self.changes(&Draft::default())))
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
            for (label, v) in was {
                if !now.iter().any(|(l, n)| l == label && n.trim() == v.trim()) {
                    out.push(Change::RemoveExact(
                        *m,
                        model::Labeled {
                            label: label.clone(),
                            value: v.clone(),
                        },
                    ));
                }
            }
            for (label, v) in now {
                if !was.iter().any(|(l, w)| l == label && w.trim() == v.trim()) {
                    out.push(Change::Add(
                        *m,
                        model::Labeled {
                            label: label.clone(),
                            value: v.clone(),
                        },
                    ));
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

    #[test]
    fn a_draft_changes_only_what_was_edited() {
        let c = Contact(
            json!({
                "contactId": "A", "firstName": "Ann", "birthday": "1604-05-01",
                "phones": [{"label": "MOBILE", "field": "+1 555"}, {"label": "WORK", "field": "+1 666"}],
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let before = Draft::of(&c);
        assert_eq!(before.scalar(Field::Birthday), "05-01");
        assert!(before.changes(&before).is_empty());
        let mut after = before.clone();
        after.scalars.iter_mut().find(|(f, _)| *f == Field::Last).unwrap().1 = "Lee".into();
        after.multis[0].1 = vec![("MOBILE".into(), "+1 555".into()), ("HOME".into(), "+1 777".into())];
        let changes = after.changes(&before);
        assert_eq!(
            changes,
            [
                Change::Set(Field::Last, "Lee".into()),
                Change::RemoveExact(
                    Multi::Phone,
                    model::Labeled {
                        label: "WORK".into(),
                        value: "+1 666".into()
                    }
                ),
                Change::Add(
                    Multi::Phone,
                    model::Labeled {
                        label: "HOME".into(),
                        value: "+1 777".into()
                    }
                ),
            ]
        );
        assert!(Draft::default().problem(&Draft::default()).is_some());
        assert_eq!(after.problem(&before), None);
        // Emptying one row removes that value only: not another row holding
        // the same number written another way.
        let mut c = Contact(
            json!({ "phones": [{"label": "HOME", "field": "555-010-0199"}, {"label": "WORK", "field": "+1 555 010 0199"}] })
                .as_object()
                .unwrap()
                .clone(),
        );
        let before = Draft::of(&c);
        let mut after = before.clone();
        after.multis[0].1.remove(0);
        c.apply(&after.changes(&before)).unwrap();
        assert_eq!(
            c.values(Multi::Phone),
            [model::Labeled {
                label: "WORK".into(),
                value: "+1 555 010 0199".into()
            }]
        );
        // A value stored without a label keeps none when its row is
        // read back: saving the dialog does not turn it into a mobile number.
        let c = Contact(
            json!({ "phones": [{"field": "+1 555"}, {"label": "WORK", "field": "+1 666"}] })
                .as_object()
                .unwrap()
                .clone(),
        );
        let before = Draft::of(&c);
        let mut read = before.clone();
        read.multis[0].1 = before.multis[0]
            .1
            .iter()
            .map(|(label, value)| typed(Multi::Phone, Some(label), value))
            .collect();
        assert_eq!(read.changes(&before), []);
        assert_eq!(
            typed(Multi::Phone, None, "work=+1 777"),
            ("WORK".into(), "+1 777".into())
        );
        // A valid phone does not hide an invalid email after it.
        let mut bad = after.clone();
        bad.multis[1].1 = vec![("HOME".into(), "nope".into())];
        assert_eq!(
            bad.problem(&before).as_deref(),
            Some("\"nope\" is not an email address")
        );
    }

    /// A value iCloud already holds that this dialog would not accept (an
    /// email with no `@`) does not disable Save for an edit elsewhere.
    #[test]
    fn a_stored_value_the_dialog_would_refuse_does_not_block_another_edit() {
        let c = Contact(
            json!({ "firstName": "Ann", "emailAddresses": [{"label": "HOME", "field": "nope"}], "birthday": "1990-02-30" })
                .as_object()
                .unwrap()
                .clone(),
        );
        let before = Draft::of(&c);
        let mut after = before.clone();
        after.scalars.iter_mut().find(|(f, _)| *f == Field::Last).unwrap().1 = "Lee".into();
        assert_eq!(after.problem(&before), None);
        assert_eq!(after.changes(&before), [Change::Set(Field::Last, "Lee".into())]);
        // A value typed anew is still checked.
        after.multis[1].1.push(("WORK".into(), "also-nope".into()));
        assert!(after.problem(&before).is_some());
    }

    /// Saving the dialog leaves a stored value with spaces around it as it
    /// is (its place, its label or none, what else the entry holds): its
    /// row reads back trimmed, and that is not an edit.
    #[test]
    fn a_padded_value_left_alone_is_not_rewritten() {
        let c = Contact(
            json!({ "firstName": "Ann", "urls": [{"field": " https://p.example/ann ", "x-extra": 1}, {"label": "WORK", "field": "acme.example"}] })
                .as_object()
                .unwrap()
                .clone(),
        );
        let before = Draft::of(&c);
        let mut read = before.clone();
        for (m, rows) in &mut read.multis {
            for r in rows.iter_mut() {
                *r = typed(*m, Some(&r.0.clone()), &r.1);
            }
        }
        read.scalars.iter_mut().find(|(f, _)| *f == Field::Last).unwrap().1 = "Lee".into();
        assert_eq!(read.changes(&before), [Change::Set(Field::Last, "Lee".into())]);
    }
}
