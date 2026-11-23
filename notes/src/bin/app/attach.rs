//! Attachments: the strip of what the open note links to, and files made
//! attachments of it from the file chooser (the desktop's portal, behind
//! `omui::FilePicker`), a drop or a paste. Each file is
//! copied into the folder's attachments/ and linked on a line of its own;
//! the next sync uploads it. A link deleted from the text removes its
//! attachment (Save anyway? asks first).

use std::path::PathBuf;
use std::rc::Rc;

use gtk::{gdk, gio, glib, prelude::*};
use omui::icons::name;
use omui::tr;

use crate::window::{Ui, as_is};

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
        let icon = omui::icon(name::ATTACH);
        icon.set_opacity(0.6);
        widget.append(&icon);
        let items = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let scroller = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&items)
            .build();
        widget.append(&scroller);
        let hint = omui::label("", "caption");
        omui::i18n::bind_label(&hint, "Delete a link to remove its attachment");
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
        // The library's refusals, in English.
        self.set_notice(as_is(errors.join(" ")));
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
        let (chosen, failed) = (Rc::downgrade(&ui), Rc::downgrade(&ui));
        ask_for_files(
            ui.picker.as_ref(),
            move |paths| {
                if let Some(ui) = chosen.upgrade() {
                    ui.attach_files(paths, None);
                }
            },
            move |why| {
                if let Some(detail) = why.detail() {
                    eprintln!("icloud-notes: the file chooser: {why} ({detail})");
                }
                if let Some(ui) = failed.upgrade() {
                    // Written again in the new language after a switch.
                    ui.set_notice(move || why.to_string());
                }
            },
        );
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

/// Asks `picker` for the files to attach, from the home folder: `chosen`
/// gets them, `failed` why no dialog could be shown (its text is for the
/// window, in the language of the moment each time it is written; its
/// detail for the log). Closing the dialog does neither.
pub fn ask_for_files(
    picker: &dyn omui::FilePicker,
    chosen: impl FnOnce(Vec<PathBuf>) + 'static,
    failed: impl FnOnce(omui::PickError) + 'static,
) {
    let options = omui::PickOptions {
        multiple: true,
        folder: Some(glib::home_dir()),
        accept_label: Some(tr("_Attach")),
        ..omui::PickOptions::new(&tr("Attach files"))
    };
    picker.open(
        options,
        Box::new(move |picked| match picked {
            Ok(paths) => chosen(paths),
            Err(omui::PickError::Cancelled) => {}
            Err(why) => failed(why),
        }),
    );
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    /// What a request answered: the files, or the failure.
    #[derive(Debug, Default, PartialEq)]
    struct Heard {
        chosen: Option<Vec<PathBuf>>,
        failed: Option<String>,
    }

    fn ask(picker: &omui::FakeFilePicker) -> Heard {
        let heard = Rc::new(RefCell::new(Heard::default()));
        let (chosen, failed) = (heard.clone(), heard.clone());
        ask_for_files(
            picker,
            move |paths| chosen.borrow_mut().chosen = Some(paths),
            move |why| failed.borrow_mut().failed = Some(why.to_string()),
        );
        Rc::try_unwrap(heard).unwrap().into_inner()
    }

    #[test]
    fn the_files_picked_are_attached() {
        let picker = omui::FakeFilePicker::new();
        let files = vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.pdf")];
        picker.answer(Ok(files.clone()));
        assert_eq!(ask(&picker).chosen, Some(files));
        let asked = picker.asked();
        assert_eq!(asked.len(), 1);
        let (saving, options) = &asked[0];
        assert!(!saving, "it opens files");
        assert!(options.multiple, "several at once");
        assert_eq!(options.title, "Attach files");
        assert_eq!(options.folder, Some(glib::home_dir()));
    }

    #[test]
    fn a_dialog_closed_attaches_nothing_and_says_nothing() {
        let picker = omui::FakeFilePicker::new();
        picker.answer(Err(omui::PickError::Cancelled));
        assert_eq!(ask(&picker), Heard::default());
    }

    /// The window is told omui's words for the failure, not what GIO said
    /// of it, which is for the log.
    #[test]
    fn a_picker_that_fails_says_why() {
        let _language = omui::i18n::lock_language();
        let picker = omui::FakeFilePicker::new();
        picker.answer(Err(omui::PickError::Unavailable(omui::PickDetail::new(
            "Cannot autolaunch D-Bus without X11 $DISPLAY",
        ))));
        assert_eq!(
            ask(&picker),
            Heard {
                chosen: None,
                failed: Some("No file chooser is available on this desktop.".into())
            }
        );
    }
}
