//! A note laid out on A4 pages as a PDF, styled as the editor shows it:
//! what `export-pdf` writes, from the window and from the command line.
//! Cairo and Pango need no display, so neither does this.

use std::io;
use std::path::Path;

use gtk::pango;

use crate::highlight;

/// A4 in points, and the margin around the text.
const PAGE: (f64, f64) = (595.0, 842.0);
const MARGIN: f64 = 56.0;

fn failed(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// Writes `markdown` to `path` as a PDF.
pub fn render(markdown: &str, path: &Path) -> io::Result<()> {
    let surface = cairo::PdfSurface::new(PAGE.0, PAGE.1, path).map_err(failed)?;
    let cr = cairo::Context::new(&surface).map_err(failed)?;
    let layout = pangocairo::functions::create_layout(&cr);
    let scale = f64::from(pango::SCALE);
    layout.set_width(((PAGE.0 - 2.0 * MARGIN) * scale) as i32);
    layout.set_wrap(pango::WrapMode::WordChar);
    layout.set_font_description(Some(&pango::FontDescription::from_string("Sans 11")));
    let markup: Vec<String> = markdown.lines().map(highlight::markup).collect();
    layout.set_markup(&markup.join("\n"));

    // Line by line, onto a new page when the next would go past the bottom.
    let usable = PAGE.1 - 2.0 * MARGIN;
    let mut top = 0.0;
    let mut lines = layout.iter();
    loop {
        let (_, logical) = lines.line_extents();
        let (y, height) = (f64::from(logical.y()) / scale, f64::from(logical.height()) / scale);
        if y + height - top > usable && y > top {
            cr.show_page().map_err(failed)?;
            top = y;
        }
        if let Some(line) = lines.line_readonly() {
            let baseline = f64::from(lines.baseline()) / scale;
            cr.move_to(MARGIN + f64::from(logical.x()) / scale, MARGIN + baseline - top);
            pangocairo::functions::show_layout_line(&cr, &line);
        }
        if !lines.next_line() {
            break;
        }
    }
    drop(cr);
    surface.finish();
    surface.status().map_err(failed)
}
