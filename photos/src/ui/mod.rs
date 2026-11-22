pub mod albums;
pub mod banner;
pub mod decode;
pub mod grid;
pub mod prefs;
pub mod upload;
pub mod viewer;
pub mod window;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// Over omui's stylesheet: the photo grid's tiles and their Live Photo and
/// video badges, and the viewer's ground around a photo. The tiles' corners
/// are the theme's small radius, which follows Hyprland's rounding. A badge
/// sits on a photo, whose colours could be anything, so it is drawn on a
/// chip of the theme's background rather than straight on the picture.
pub const CSS: &str = "
.photo-grid, .photo-grid > row { background: none; padding: 0; }
.photo-grid > row:hover { background: none; }
button.tile { padding: 0; border: 1px solid transparent; border-radius: var(--om-radius-small); min-width: 0; min-height: 0; }
button.tile:focus-visible { border-color: @om_accent; outline: none; }
.tile-picture { border-radius: var(--om-radius-small); background-color: alpha(currentColor, 0.08); }
.tile-badge { color: @om_text; background-color: alpha(@om_bg, 0.75); border-radius: 999px; padding: 3px; }
.viewer { background-color: @om_bg; }
";

/// Text that is built again after each switch of language: what a widget
/// shows that is not one fixed string (a status line, a count, a summary).
/// [`Relabel::set`] takes the function that builds the text, shows its text,
/// and keeps it for the next switch. The widget is held weakly, so the text
/// goes with it.
pub struct Relabel<W: IsA<gtk::Widget>> {
    widget: glib::WeakRef<W>,
    apply: fn(&W, &str),
    make: Rc<RefCell<Box<dyn Fn() -> String>>>,
}

impl<W: IsA<gtk::Widget>> Relabel<W> {
    /// Builds `widget`'s text with what [`Relabel::set`] was last given,
    /// through `apply`, now and after each switch for as long as it lives.
    pub fn new(widget: &W, apply: fn(&W, &str)) -> Self {
        let make: Rc<RefCell<Box<dyn Fn() -> String>>> = Rc::new(RefCell::new(Box::new(String::new)));
        let (weak, again) = (widget.downgrade(), make.clone());
        omui::i18n::on_change(widget, move || {
            if let Some(widget) = weak.upgrade() {
                apply(&widget, &(again.borrow())());
            }
        });
        Self {
            widget: widget.downgrade(),
            apply,
            make,
        }
    }

    pub fn set(&self, make: impl Fn() -> String + 'static) {
        if let Some(widget) = self.widget.upgrade() {
            (self.apply)(&widget, &make());
        }
        *self.make.borrow_mut() = Box::new(make);
    }
}

/// [`Relabel`] for a label.
pub fn relabel(label: &gtk::Label) -> Relabel<gtk::Label> {
    Relabel::new(label, |label, text| label.set_label(text))
}

/// A size in bytes as the language writes the number, with SI units (as
/// GLib's `format_size`, whose number follows the process's locale rather
/// than the language picked in Omarchy). Units are not translated.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return omui::trn("{n} bytes", bytes as i64);
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{} {}", omui::i18n::format_number(value, 1), UNITS[unit])
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes_read_as_glib_writes_them_in_english() {
        // The plural forms are the catalog's.
        omui::i18n::register("en", crate::ENGLISH);
        assert_eq!(super::format_size(1), "1 byte");
        assert_eq!(super::format_size(999), "999 bytes");
        assert_eq!(super::format_size(1_500), "1.5 kB");
        assert_eq!(super::format_size(2_345_678), "2.3 MB");
        assert_eq!(super::format_size(999_999), "1.0 MB");
        assert_eq!(super::format_size(3_000_000_000), "3.0 GB");
    }
}
