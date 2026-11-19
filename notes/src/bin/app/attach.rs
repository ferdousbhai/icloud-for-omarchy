//! Attachments: the strip of what the open note links to, and files made
//! attachments of it from the file chooser, a drop or a paste. Each file is
//! copied into the folder's attachments/ and linked on a line of its own;
//! the next sync uploads it. A link deleted from the text removes its
//! attachment (Save anyway? asks first).

use std::path::PathBuf;
use std::rc::Rc;

use gtk::{gdk, gio, glib, prelude::*};
use omui::icons::name;

use crate::window::Ui;

/// How large a picture's thumbnail is in the strip.
const THUMB: (i32, i32) = (46, 36);

pub struct Strip {
    pub widget: gtk::Box,
    items: gtk::Box,
    hint: gtk::Label,
}

impl Strip {
    pub fn new() -> Strip {
        let widget = gtk::Box::builder()
            .spacing(10)
            .css_classes(["attachments"])
            .margin_start(32)
            .margin_end(32)
            .margin_top(14)
            .visible(false)
            .build();
        let icon = crate::icons::image(crate::icons::ATTACH);
        icon.set_opacity(0.6);
        widget.append(&icon);
        let items = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let scroller = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&items)
            .build();
        widget.append(&scroller);
        let hint = omui::label("Delete a link to remove its attachment", "caption");
        hint.add_css_class("dim-label");
        hint.set_wrap(false);
        widget.append(&hint);
        Strip { widget, items, hint }
    }
}

impl Ui {
    /// The strip, for the open note as it is on disk.
    pub fn show_attachments(&self) {
        let strip = &self.strip;
        omui::clear(&strip.items);
        let attachments = self.backend.borrow().note_attachments().to_vec();
        strip.widget.set_visible(!attachments.is_empty());
        strip.hint.set_visible(self.editor.view.is_editable());
        for attachment in attachments {
            let item = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .build();
            if attachment.image {
                let picture = gtk::Picture::for_filename(&attachment.path);
                picture.set_size_request(THUMB.0, THUMB.1);
                picture.set_content_fit(gtk::ContentFit::Contain);
                item.append(&picture);
            } else {
                let icon = omui::icon(name::DOCUMENT);
                icon.set_pixel_size(22);
                item.append(&icon);
            }
            item.append(
                &gtk::Label::builder()
                    .label(&attachment.name)
                    .max_width_chars(12)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .css_classes(["caption", "dim-label"])
                    .build(),
            );
            item.set_tooltip_text(Some(&attachment.name));
            strip.items.append(&item);
        }
    }

    /// Files made attachments of the open note, linked at `at` (a drop's
    /// place: the end of its line), else at the caret, or at the end of a
    /// note never clicked into.
    pub fn attach_files(&self, paths: Vec<PathBuf>, at: Option<gtk::TextIter>) {
        if paths.is_empty() || !self.window.lookup_action("attach").is_some_and(|a| a.is_enabled()) {
            return;
        }
        let (mut links, mut errors) = (Vec::new(), Vec::new());
        for path in paths {
            match self.backend.borrow_mut().add_attachment(&path.to_string_lossy()) {
                Ok(added) => links.push(added.link),
                Err(refusal) => errors.push(refusal.message),
            }
        }
        let buffer = &self.editor.buffer;
        if !links.is_empty() {
            let mut iter = match at {
                Some(mut at) => {
                    if !at.ends_line() {
                        at.forward_to_line_end();
                    }
                    at
                }
                None if self.editor.view.has_focus() || buffer.cursor_position() > 0 => {
                    buffer.iter_at_offset(buffer.cursor_position())
                }
                None => buffer.end_iter(),
            };
            let before = if iter.starts_line() { "" } else { "\n" };
            let after = if iter.ends_line() && !iter.is_end() { "" } else { "\n" };
            buffer.insert(&mut iter, &format!("{before}{}{after}", links.join("\n")));
            buffer.place_cursor(&iter);
        }
        self.set_notice(&errors.join(" "));
        self.editor.view.grab_focus();
    }
}

/// The file chooser, a drop of files on the text, and a paste of copied
/// files or a copied picture.
pub fn connect(ui: &Rc<Ui>) {
    let action = gio::SimpleAction::new("attach", None);
    let weak = Rc::downgrade(ui);
    action.connect_activate(move |_, _| {
        let Some(ui) = weak.upgrade() else { return };
        let dialog = gtk::FileDialog::builder()
            .title("Attach files")
            .modal(true)
            .initial_folder(&gio::File::for_path(glib::home_dir()))
            .build();
        let weak = Rc::downgrade(&ui);
        dialog.open_multiple(Some(&ui.window), gio::Cancellable::NONE, move |chosen| {
            let (Some(ui), Ok(files)) = (weak.upgrade(), chosen) else {
                return;
            };
            let paths = (0..files.n_items())
                .filter_map(|i| files.item(i).and_downcast::<gio::File>())
                .filter_map(|file| file.path())
                .collect();
            ui.attach_files(paths, None);
        });
    });
    ui.window.add_action(&action);

    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let weak = Rc::downgrade(ui);
    drop.connect_drop(move |_, value, x, y| {
        let (Some(ui), Ok(files)) = (weak.upgrade(), value.get::<gdk::FileList>()) else {
            return false;
        };
        let paths: Vec<PathBuf> = files.files().iter().filter_map(|file| file.path()).collect();
        if paths.is_empty() {
            return false;
        }
        let view = &ui.editor.view;
        let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        ui.attach_files(paths, view.iter_at_location(bx, by));
        true
    });
    ui.editor.view.add_controller(drop);

    // Ahead of the text's own paste, which would paste a file's path.
    let keys = gtk::EventControllerKey::builder()
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let weak = Rc::downgrade(ui);
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(ui) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let paste = modifiers.contains(gdk::ModifierType::CONTROL_MASK) && matches!(key, gdk::Key::v | gdk::Key::V);
        let clipboard = ui.editor.view.clipboard();
        let formats = clipboard.formats();
        let files = formats.contains_type(gdk::FileList::static_type());
        let picture = formats.contains_type(gdk::Texture::static_type()) && !formats.contain_mime_type("text/plain");
        if !paste || !(files || picture) || !ui.window.lookup_action("attach").is_some_and(|a| a.is_enabled()) {
            return glib::Propagation::Proceed;
        }
        let weak = Rc::downgrade(&ui);
        if files {
            clipboard.read_value_async(
                gdk::FileList::static_type(),
                glib::Priority::DEFAULT,
                gio::Cancellable::NONE,
                move |value| {
                    if let (Some(ui), Ok(value)) = (weak.upgrade(), value)
                        && let Ok(files) = value.get::<gdk::FileList>()
                    {
                        ui.attach_files(files.files().iter().filter_map(|f| f.path()).collect(), None);
                    }
                },
            );
        } else {
            clipboard.read_texture_async(gio::Cancellable::NONE, move |texture| {
                let (Some(ui), Ok(Some(texture))) = (weak.upgrade(), texture) else {
                    return;
                };
                // A copied picture (a screenshot) is attached as a PNG file.
                let dir = glib::user_cache_dir().join("icloud-notes").join("pasted");
                let path = dir.join("Pasted image.png");
                if std::fs::create_dir_all(&dir).is_ok() && texture.save_to_png(&path).is_ok() {
                    ui.attach_files(vec![path], None);
                }
            });
        }
        glib::Propagation::Stop
    });
    ui.editor.view.add_controller(keys);
}
