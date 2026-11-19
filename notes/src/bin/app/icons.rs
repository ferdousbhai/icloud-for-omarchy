//! The toolbar's icons omui does not bundle: Lucide's (ISC, as omui's are)
//! bold, italic, link, paperclip, folder-plus and square-pen. omui's own are
//! outlined into fills so GTK recolours them; these stay strokes, so each is
//! drawn here in the theme's text colour, and again when the theme changes.

use gtk::{gdk, glib, prelude::*};

pub const BOLD: &str = r#"<path d="M6 12h9a4 4 0 0 1 0 8H7a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h7a4 4 0 0 1 0 8"/>"#;
pub const ITALIC: &str = r#"<line x1="19" x2="10" y1="4" y2="4"/><line x1="14" x2="5" y1="20" y2="20"/><line x1="15" x2="9" y1="4" y2="20"/>"#;
pub const LINK: &str = r#"<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>"#;
pub const ATTACH: &str = r#"<path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48"/>"#;
pub const NEW_FOLDER: &str = r#"<path d="M12 10v6"/><path d="M9 13h6"/><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>"#;
pub const NEW_NOTE: &str = r#"<path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z"/>"#;

/// The pixels an icon is drawn at: twice the 16 it is shown at, for a scaled screen.
const DRAWN: i32 = 32;

/// A header button with the icon `geometry` (one of the above), as omui's `icon_button` makes one.
pub fn button(geometry: &'static str, tooltip: &str) -> gtk::Button {
    let image = gtk::Image::builder()
        .pixel_size(16)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    let button = gtk::Button::builder()
        .child(&image)
        .css_classes(["header-action"])
        .build();
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    if let Some(theme) = omui::Theme::get() {
        paint(&image, geometry, &theme.tokens().text);
        let weak = image.downgrade();
        theme.on_change(&image, move |tokens| {
            if let Some(image) = weak.upgrade() {
                paint(&image, geometry, &tokens.text);
            }
        });
    }
    button
}

fn paint(image: &gtk::Image, geometry: &str, colour: &str) {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{DRAWN}" height="{DRAWN}" viewBox="0 0 24 24" fill="none" stroke="{colour}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{geometry}</svg>"#
    );
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(svg.into_bytes()));
    image.set_paintable(texture.ok().as_ref());
}
