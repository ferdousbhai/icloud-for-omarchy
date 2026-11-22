//! A note both sides edited opens on its conflict blocks, one version picked
//! per block, instead of on the raw markers ("Edit as text" shows those).
//! Markers with no versions to pick from (nested, out of order) open on a
//! way out instead: remove the markers, or go back to the last synced text,
//! after the note as it is was backed up.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use icloud_notes::backend::{Conflict, ConflictLine};
use omui::i18n::bind_label;
use omui::icons::name;
use omui::{tr, trn};

use crate::window::Ui;

/// From this width a block's two versions stand side by side.
const SIDE_BY_SIDE: i32 = 560;

pub struct Conflicts {
    /// The version picker, in place of the editor's text.
    pub picker: gtk::ScrolledWindow,
    column: gtk::Box,
    /// The card over the text of a note whose markers cannot be read.
    pub unreadable: gtk::Box,
    unreadable_text: gtk::Label,
    synced: gtk::Button,
    /// The side picked for each block: "local", "remote", "both" or "".
    choices: RefCell<Vec<String>>,
    /// Edit as text: the markers in the editor, until another note opens.
    pub as_text: std::cell::Cell<bool>,
}

fn card_box(class: &str) -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .css_classes(["conflict-card", class])
        .build()
}

/// The yellow card's heading and text, with a warning icon.
fn warning(title: &gtk::Label, text: &gtk::Label) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let icon = omui::icon(name::WARNING);
    icon.add_css_class("warning");
    icon.set_valign(gtk::Align::Start);
    row.append(&icon);
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    title.add_css_class("heading");
    column.append(title);
    text.add_css_class("dim-label");
    column.append(text);
    row.append(&column);
    row
}

impl Conflicts {
    pub fn new() -> Conflicts {
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(20)
            .margin_start(32)
            .margin_end(32)
            .margin_top(14)
            .margin_bottom(32)
            .build();
        let picker = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&column)
            .visible(false)
            .build();
        let unreadable = card_box("warning-card");
        unreadable.set_margin_start(32);
        unreadable.set_margin_end(32);
        unreadable.set_margin_top(14);
        unreadable.set_visible(false);
        let unreadable_text = omui::label("", "");
        let heading = omui::label("", "");
        bind_label(&heading, "This note has conflict markers Notes can't read.");
        unreadable.append(&warning(&heading, &unreadable_text));
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let as_text = gtk::Button::builder()
            .css_classes(["flat"])
            .hexpand(true)
            .halign(gtk::Align::Start)
            .action_name("win.conflict-as-text")
            .build();
        bind_label(&as_text, "Edit as text");
        let synced = gtk::Button::builder()
            .action_name("win.recover-conflict")
            .action_target(&"synced".to_variant())
            .build();
        bind_label(&synced, "Use the last synced version");
        let strip = gtk::Button::builder()
            .css_classes(["suggested-action"])
            .action_name("win.recover-conflict")
            .action_target(&"strip".to_variant())
            .build();
        bind_label(&strip, "Remove the markers");
        buttons.append(&as_text);
        buttons.append(&synced);
        buttons.append(&strip);
        unreadable.append(&buttons);
        Conflicts {
            picker,
            column,
            unreadable,
            unreadable_text,
            synced,
            choices: RefCell::default(),
            as_text: std::cell::Cell::new(false),
        }
    }

    pub fn choices(&self) -> Vec<String> {
        self.choices.borrow().clone()
    }
}

impl Ui {
    /// The open note's conflicts on screen: the picker, the way out of
    /// unreadable markers, or neither. True when the picker shows in place
    /// of the text.
    pub fn show_conflicts(self: &Rc<Self>) -> bool {
        let c = &self.conflicts;
        let (blocks, unreadable, synced_copy, locked, syncing) = {
            let mut b = self.backend.borrow_mut();
            let locked = !b.read_only_reason().is_empty();
            let open = !b.current_note().is_empty();
            let synced_copy = open && b.note_has_synced_copy();
            (
                if open { b.note_conflicts() } else { Vec::new() },
                open && b.note_conflicts_unreadable(),
                synced_copy,
                locked,
                b.sync_running(),
            )
        };
        let resolving = !blocks.is_empty() && !c.as_text.get() && !locked;
        let stuck = unreadable && !c.as_text.get() && !locked;
        c.picker.set_visible(resolving);
        c.unreadable.set_visible(stuck);
        if stuck {
            c.unreadable_text.set_label(&if synced_copy {
                tr(
                    "\"Remove the markers\" keeps every line from both versions and drops only the marker lines, for you to read over. \"Use the last synced version\" goes back to the text last synced with iCloud. Before either, the note as it is now is copied to .icloud-notes/conflict-backups in your notes folder, so nothing is lost.",
                )
            } else {
                tr(
                    "\"Remove the markers\" keeps every line from both versions and drops only the marker lines, for you to read over. Before either, the note as it is now is copied to .icloud-notes/conflict-backups in your notes folder, so nothing is lost.",
                )
            });
            c.synced.set_visible(synced_copy);
        }
        if resolving {
            if c.choices.borrow().len() != blocks.len() {
                c.choices.replace(vec![String::new(); blocks.len()]);
            }
            self.fill_picker(&blocks);
        }
        // Both write the note: never under a sync. (A button's sensitivity
        // is its action's.)
        crate::actions::enable(self, "recover-conflict", stuck && !syncing && !self.editor.dirty());
        let chosen = c.choices.borrow().iter().all(|choice| !choice.is_empty());
        crate::actions::enable(self, "resolve-conflicts", resolving && !syncing && chosen);
        resolving
    }

    fn fill_picker(self: &Rc<Self>, blocks: &[Conflict]) {
        let c = &self.conflicts;
        omui::clear(&c.column);
        let n = blocks.len();
        let banner = card_box("warning-card");
        let text = omui::label(
            &if n == 1 {
                tr("It was edited on this computer and in iCloud before the two could sync. Pick the version to keep.")
            } else {
                trn!(
                    "It was edited on this computer and in iCloud before the two could sync. Pick the version to keep for each of the {n} changes.",
                    n
                )
            },
            "",
        );
        let heading = omui::label(&tr("This note changed in two places"), "");
        banner.append(&warning(&heading, &text));
        c.column.append(&banner);
        let choices = c.choices();
        let wide = self.editor.widget.width() >= SIDE_BY_SIDE + 64 || self.editor.widget.width() == 0;
        for (index, block) in blocks.iter().enumerate() {
            let hunk = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .build();
            if n > 1 {
                let (at, of) = (
                    omui::i18n::format_int(index as i64 + 1),
                    omui::i18n::format_int(n as i64),
                );
                hunk.append(&omui::label(&tr!("Change {} of {}", at, of), "caption"));
            }
            let context = |lines: &[String]| {
                let label = omui::label(&lines.join("\n"), "dim-label");
                label.set_wrap(false);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_visible(!lines.is_empty());
                label
            };
            hunk.append(&context(&block.before));
            let sides = gtk::Box::builder()
                .orientation(if wide {
                    gtk::Orientation::Horizontal
                } else {
                    gtk::Orientation::Vertical
                })
                .spacing(12)
                .homogeneous(wide)
                .build();
            let choice = choices.get(index).cloned().unwrap_or_default();
            sides.append(&self.version_card(
                index,
                "local",
                &tr("This computer"),
                name::COMPUTER,
                &block.local,
                &choice,
            ));
            // The brand, in every language.
            sides.append(&self.version_card(index, "remote", "iCloud", name::CLOUD, &block.remote, &choice));
            hunk.append(&sides);
            let both = gtk::CheckButton::builder()
                .label(tr("Keep both, this computer's version first"))
                .active(choice == "both")
                .build();
            // Unticked, the block has no version picked again.
            let weak = Rc::downgrade(self);
            both.connect_toggled(move |button| {
                if let Some(ui) = weak.upgrade() {
                    ui.choose_conflict(index, if button.is_active() { "both" } else { "" });
                }
            });
            hunk.append(&both);
            hunk.append(&context(&block.after));
            c.column.append(&hunk);
        }
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.append(
            &gtk::Button::builder()
                .label(tr("Edit as text"))
                .css_classes(["flat"])
                .hexpand(true)
                .halign(gtk::Align::Start)
                .action_name("win.conflict-as-text")
                .build(),
        );
        if n > 1 {
            for (label, side) in [
                (tr("All from this computer"), "local"),
                (tr("All from iCloud"), "remote"),
            ] {
                let button = gtk::Button::with_label(&label);
                let weak = Rc::downgrade(self);
                button.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.conflicts.choices.replace(vec![side.to_string(); n]);
                        ui.show_conflicts();
                    }
                });
                buttons.append(&button);
            }
        }
        let keep = gtk::Button::builder()
            .label(tr("Keep selected"))
            .css_classes(["suggested-action"])
            .action_name("win.resolve-conflicts")
            .build();
        buttons.append(&keep);
        c.column.append(&buttons);
    }

    /// One side of a block: its lines, those the other side lacks tinted,
    /// the whole card a click target.
    fn version_card(
        self: &Rc<Self>,
        index: usize,
        side: &'static str,
        label: &str,
        icon: &str,
        lines: &[ConflictLine],
        choice: &str,
    ) -> gtk::Button {
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let picked = choice == side;
        // A ticked circle for the version picked, an empty one (as a radio
        // button's) for the other.
        if picked {
            let mark = omui::icon(name::OK);
            mark.add_css_class("accent");
            head.append(&mark);
        } else {
            let mark = omui::icon(name::CIRCLE);
            mark.add_css_class("unpicked");
            head.append(&mark);
        }
        head.append(&omui::icon(icon));
        let title = omui::label(label, "heading");
        title.set_hexpand(true);
        head.append(&title);
        let changed = lines.iter().filter(|l| l.changed).count();
        let differs = match changed {
            0 => String::new(),
            n => trn!("{n} lines differ", n),
        };
        head.append(&omui::label(&differs, "caption"));
        column.append(&head);
        if lines.is_empty() {
            column.append(&omui::label(
                &tr("Empty. This version removed these lines."),
                "dim-label",
            ));
        }
        for line in lines {
            // The Markdown as written: rendered, a line from iCloud could load
            // remote images. Blank lines keep their height.
            let text = if line.text.trim().is_empty() { " " } else { &line.text };
            let label = omui::label(text, if line.changed { "changed-line" } else { "dim-label" });
            label.add_css_class("conflict-line");
            column.append(&label);
        }
        let card = gtk::Button::builder()
            .child(&column)
            .css_classes(["version-card"])
            .hexpand(true)
            .build();
        omui::set_class(&card, "picked", picked);
        card.update_property(&[gtk::accessible::Property::Label(&format!("{label}: {differs}"))]);
        let weak = Rc::downgrade(self);
        card.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.choose_conflict(index, side);
            }
        });
        card
    }

    fn choose_conflict(self: &Rc<Self>, index: usize, side: &str) {
        if let Some(choice) = self.conflicts.choices.borrow_mut().get_mut(index) {
            *choice = side.to_string();
        }
        self.show_conflicts();
    }
}
