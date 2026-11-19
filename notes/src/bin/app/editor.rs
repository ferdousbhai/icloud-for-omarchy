//! The editor pane: the open note's date, its title in a vault that titles
//! notes by file name, what keeps it from being edited, and its Markdown as
//! plain text. Notes has no Save button: what is typed is written once
//! typing pauses (the window's autosave).

use std::cell::RefCell;

use gtk::prelude::*;
use omui::icons::name;

pub struct Editor {
    pub widget: gtk::Stack,
    pub date: gtk::Label,
    /// The title of a note in a vault titled by file name: renaming it
    /// renames the file. In-body vaults title a note by its first line.
    pub title: gtk::Entry,
    pub conflict_copy: gtk::Label,
    pub read_only: gtk::Label,
    pub view: gtk::TextView,
    pub buffer: gtk::TextBuffer,
    /// What the editor was last loaded from or saved as; edits diverge from it.
    saved: RefCell<String>,
}

/// A centred line of the note's header.
fn header_line(class: &str) -> gtk::Label {
    gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["caption", class])
        .margin_start(32)
        .margin_end(32)
        .visible(false)
        .build()
}

impl Editor {
    pub fn new() -> Editor {
        let empty = omui::EmptyState::new(Some(name::DOCUMENT), "", Some("Select a note, or create a new one."));
        let date = gtk::Label::builder()
            .css_classes(["caption", "dim-label"])
            .margin_top(16)
            .build();
        let title = gtk::Entry::builder()
            .css_classes(["note-title"])
            .margin_start(28)
            .margin_end(28)
            .margin_top(12)
            .visible(false)
            .build();
        let conflict_copy = header_line("warning");
        conflict_copy.set_label(
            "This is a Syncthing conflict copy, so it is not synced. Merge what you want from it into the note and delete it.",
        );
        let read_only = header_line("warning");
        let buffer = gtk::TextBuffer::new(None);
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(32)
            .right_margin(32)
            .top_margin(4)
            .bottom_margin(32)
            .css_classes(["note-text"])
            .build();
        view.update_property(&[gtk::accessible::Property::Label("Note")]);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .margin_top(8)
            .child(&view)
            .build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for widget in [
            date.upcast_ref::<gtk::Widget>(),
            title.upcast_ref(),
            conflict_copy.upcast_ref(),
            read_only.upcast_ref(),
            scroller.upcast_ref(),
        ] {
            page.append(widget);
        }
        let widget = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        widget.add_named(&empty.widget, Some("empty"));
        widget.add_named(&page, Some("note"));
        Editor {
            widget,
            date,
            title,
            conflict_copy,
            read_only,
            view,
            buffer,
            saved: RefCell::default(),
        }
    }

    pub fn text(&self) -> String {
        self.buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false)
            .into()
    }

    /// The text differs from what was loaded or saved.
    pub fn dirty(&self) -> bool {
        self.text() != *self.saved.borrow()
    }

    pub fn saved(&self) -> String {
        self.saved.borrow().clone()
    }

    pub fn set_saved(&self, text: &str) {
        self.saved.replace(text.to_string());
    }

    /// Shows `body`, the note as the editor holds it. A reload of the open
    /// note (a pull) keeps the cursor where it was.
    pub fn load(&self, body: &str) {
        if self.text() != body {
            let at = self.buffer.cursor_position();
            self.buffer.set_text(body);
            let at = at.min(self.buffer.char_count());
            self.buffer.place_cursor(&self.buffer.iter_at_offset(at));
        }
        self.set_saved(body);
    }

    /// No note: the empty page, and nothing in the editor.
    pub fn close(&self) {
        self.buffer.set_text("");
        self.set_saved("");
        self.widget.set_visible_child_name("empty");
    }
}
