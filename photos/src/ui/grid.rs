//! The photo grid: a GtkListView of fixed-width rows of tiles, sectioned by
//! month. GtkGridView has no section headers (still true in GTK 4.22), so the
//! rows are what the list view virtualises and a section sorter over the row
//! model gives the month headers.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use icloud_photos::catalog::GridRow;
use icloud_photos::cloudkit::Kind;

/// Smallest tile edge; tiles grow to fill the width.
const TILE: i32 = 140;
const GAP: i32 = 3;
const PAD: i32 = 12;

/// One asset as the grid shows it. Built off the main loop with its month
/// already worked out, so re-chunking for a new width does no date maths.
#[derive(Debug, Clone)]
pub struct Tile {
    pub id: String,
    pub thumb: Option<PathBuf>,
    pub kind: Kind,
    pub is_live: bool,
    pub filename: String,
    /// year * 12 + month, local time.
    pub month: i64,
}

#[derive(Debug, Clone)]
pub struct RowItem {
    /// year * 12 + month, local time; sections sort newest first.
    pub month: i64,
    /// The month's header ("September 2026"), shared by its rows.
    pub label: Arc<str>,
    pub tiles: Vec<Arc<Tile>>,
}

/// Called when a tile is bound: (asset id, its thumb file if downloaded, the picture).
type BindTile = Box<dyn Fn(&str, Option<&PathBuf>, &gtk::Picture)>;
/// Called when a tile scrolls out of the list's reach: (asset id).
type UnbindTile = Box<dyn Fn(&str)>;

pub struct Grid {
    pub root: gtk::Stack,
    pub empty: adw::StatusPage,
    scrolled: gtk::ScrolledWindow,
    store: gio::ListStore,
    columns: Rc<Cell<usize>>,
    tile: Rc<Cell<i32>>,
    tiles: RefCell<Vec<Arc<Tile>>>,
    /// The picture each bound asset is shown in, so a decoded thumbnail goes
    /// straight to it.
    bound: Rc<RefCell<HashMap<String, gtk::Picture>>>,
    pub on_bind: Rc<RefCell<Option<BindTile>>>,
    pub on_unbind: Rc<RefCell<Option<UnbindTile>>>,
}

fn month_key(unix: i64) -> i64 {
    match glib::DateTime::from_unix_local(unix) {
        Ok(dt) => i64::from(dt.year()) * 12 + i64::from(dt.month()),
        Err(_) => 0,
    }
}

fn month_label(key: i64) -> Arc<str> {
    let (year, month) = ((key - 1).div_euclid(12), (key - 1).rem_euclid(12) + 1);
    glib::DateTime::from_local(year as i32, month as i32, 1, 0, 0, 0.0)
        .ok()
        .and_then(|dt| dt.format("%B %Y").ok())
        .map(|s| Arc::from(s.as_str()))
        .unwrap_or_else(|| Arc::from(""))
}

/// Catalog rows to tiles (newest first), each with its local month. Runs
/// off the main loop, once per reload.
pub fn tiles_of(assets: Vec<GridRow>) -> Vec<Arc<Tile>> {
    assets
        .into_iter()
        .map(|a| {
            Arc::new(Tile {
                month: month_key(a.created),
                id: a.id,
                thumb: a.thumb_path,
                kind: a.kind,
                is_live: a.is_live,
                filename: a.filename,
            })
        })
        .collect()
}

/// Chunk tiles (newest first) into rows of `columns`, never crossing a month.
pub fn rows_of(tiles: &[Arc<Tile>], columns: usize) -> Vec<RowItem> {
    let mut rows: Vec<RowItem> = Vec::new();
    for t in tiles {
        match rows.last_mut() {
            Some(r) if r.month == t.month && r.tiles.len() < columns => r.tiles.push(t.clone()),
            last => {
                let label = match last {
                    Some(r) if r.month == t.month => r.label.clone(),
                    _ => month_label(t.month),
                };
                rows.push(RowItem {
                    month: t.month,
                    label,
                    tiles: vec![t.clone()],
                });
            }
        }
    }
    rows
}

fn row_item(obj: &glib::Object) -> Option<std::cell::Ref<'_, RowItem>> {
    obj.downcast_ref::<glib::BoxedAnyObject>()
        .map(|b| b.borrow::<RowItem>())
}

impl Grid {
    pub fn new() -> Grid {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sorted = gtk::SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
        let by_month = gtk::CustomSorter::new(|a, b| {
            let (a, b) = (row_item(a).map_or(0, |r| r.month), row_item(b).map_or(0, |r| r.month));
            b.cmp(&a).into()
        });
        sorted.set_section_sorter(Some(&by_month));

        let columns = Rc::new(Cell::new(4usize));
        let tile = Rc::new(Cell::new(TILE));
        let on_bind: Rc<RefCell<Option<BindTile>>> = Rc::new(RefCell::new(None));
        let on_unbind: Rc<RefCell<Option<UnbindTile>>> = Rc::new(RefCell::new(None));
        let bound: Rc<RefCell<HashMap<String, gtk::Picture>>> = Rc::default();

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            item.set_activatable(false);
            item.set_focusable(false);
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(GAP)
                .halign(gtk::Align::Start)
                .margin_start(PAD)
                .build();
            row.set_margin_bottom(GAP);
            item.set_child(Some(&row));
        });
        let cols = columns.clone();
        let size = tile.clone();
        let bind_cb = on_bind.clone();
        let bind_map = bound.clone();
        factory.connect_bind(move |_, obj| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(row) = item.child().and_downcast::<gtk::Box>() else {
                return;
            };
            let Some(data) = item.item() else { return };
            let Some(data) = row_item(&data) else { return };
            // Make the row hold exactly `columns` tile slots.
            let want = cols.get();
            let mut have = 0;
            let mut child = row.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                have += 1;
                if have > want {
                    row.remove(&c);
                }
            }
            for _ in have..want {
                row.append(&tile_widget());
            }
            let mut slot = row.first_child();
            let mut i = 0;
            while let Some(w) = slot {
                slot = w.next_sibling();
                let Some(button) = w.downcast_ref::<gtk::Button>() else {
                    continue;
                };
                match data.tiles.get(i) {
                    Some(t) => {
                        button.set_visible(true);
                        button.set_action_target_value(Some(&t.id.to_variant()));
                        button.set_action_name(Some("win.open-asset"));
                        button.set_tooltip_text(Some(&t.filename));
                        let (picture, badge) = tile_parts(button);
                        picture.set_widget_name(&t.id);
                        bind_map.borrow_mut().insert(t.id.clone(), picture.clone());
                        if let Some(spacer) = picture.prev_sibling() {
                            spacer.set_size_request(size.get(), size.get());
                        }
                        picture.set_paintable(None::<&gtk::gdk::Paintable>);
                        badge.set_visible(t.kind == Kind::Video || t.is_live);
                        badge.set_icon_name(Some(if t.kind == Kind::Video {
                            "media-playback-start-symbolic"
                        } else {
                            "camera-photo-symbolic"
                        }));
                        if let Some(cb) = bind_cb.borrow().as_ref() {
                            cb(&t.id, t.thumb.as_ref(), &picture);
                        }
                    }
                    None => button.set_visible(false),
                }
                i += 1;
            }
        });
        let unbind_cb = on_unbind.clone();
        let unbind_map = bound.clone();
        factory.connect_unbind(move |_, obj| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(row) = item.child().and_downcast::<gtk::Box>() else {
                return;
            };
            let mut slot = row.first_child();
            while let Some(w) = slot {
                slot = w.next_sibling();
                if let Some(button) = w.downcast_ref::<gtk::Button>() {
                    let (picture, _) = tile_parts(button);
                    let id = picture.widget_name();
                    if !id.is_empty() {
                        let mut map = unbind_map.borrow_mut();
                        if map.get(id.as_str()) == Some(&picture) {
                            map.remove(id.as_str());
                        }
                        drop(map);
                        if let Some(cb) = unbind_cb.borrow().as_ref() {
                            cb(&id);
                        }
                    }
                    picture.set_widget_name("");
                    picture.set_paintable(None::<&gtk::gdk::Paintable>);
                }
            }
        });

        let headers = gtk::SignalListItemFactory::new();
        headers.connect_setup(|_, obj| {
            let Some(h) = obj.downcast_ref::<gtk::ListHeader>() else {
                return;
            };
            let label = gtk::Label::builder()
                .xalign(0.0)
                .css_classes(["title-4"])
                .margin_top(18)
                .margin_bottom(8)
                .margin_start(PAD)
                .build();
            h.set_child(Some(&label));
        });
        headers.connect_bind(|_, obj| {
            let Some(h) = obj.downcast_ref::<gtk::ListHeader>() else {
                return;
            };
            let (Some(label), Some(item)) = (h.child().and_downcast::<gtk::Label>(), h.item()) else {
                return;
            };
            if let Some(r) = row_item(&item) {
                label.set_label(&r.label);
            }
        });

        let list = gtk::ListView::new(Some(gtk::NoSelection::new(Some(sorted))), Some(factory));
        list.set_header_factory(Some(&headers));
        list.add_css_class("photo-grid");
        list.set_single_click_activate(false);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .vexpand(true)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("image-x-generic-symbolic")
            .title("No photos yet")
            .build();
        let root = gtk::Stack::new();
        root.add_named(&scrolled, Some("grid"));
        root.add_named(&empty, Some("empty"));

        let grid = Grid {
            root,
            empty,
            scrolled,
            store,
            columns,
            tile,
            tiles: RefCell::new(Vec::new()),
            bound,
            on_bind,
            on_unbind,
        };
        grid.watch_width();
        grid
    }

    /// Size tiles to fill the width: re-chunk the rows when the number of
    /// columns changes, otherwise just resize the tiles on screen. The list
    /// view sets its horizontal adjustment's page size to its width on every
    /// allocation, so that notification is the resize signal (no per-frame
    /// tick). The work runs on idle, outside the allocation that told us.
    fn watch_width(&self) {
        let (cols, tile) = (self.columns.clone(), self.tile.clone());
        let last = Rc::new(Cell::new(0));
        let queued = Rc::new(Cell::new(false));
        let sw = self.scrolled.clone();
        self.scrolled.hadjustment().connect_page_size_notify(move |adj| {
            let width = adj.page_size().round() as i32;
            if width <= 0 || width == last.get() || queued.replace(true) {
                return;
            }
            let (cols, tile, last, queued, sw) = (cols.clone(), tile.clone(), last.clone(), queued.clone(), sw.clone());
            let adj = adj.clone();
            glib::idle_add_local_once(move || {
                queued.set(false);
                let width = adj.page_size().round() as i32;
                if width <= 0 || width == last.replace(width) {
                    return;
                }
                let inner = width - 2 * PAD;
                let fit = ((inner + GAP) / (TILE + GAP)).max(1);
                let edge = ((inner - GAP * (fit - 1)) / fit).max(TILE / 2);
                let resized = edge != tile.replace(edge);
                if fit as usize != cols.get() {
                    cols.set(fit as usize);
                    sw.activate_action("win.regrid", None).ok();
                } else if resized {
                    walk(sw.upcast_ref(), &mut |w| {
                        if w.has_css_class("tile-spacer") {
                            w.set_size_request(edge, edge);
                        }
                    });
                }
            });
        });
    }

    pub fn columns(&self) -> usize {
        self.columns.get()
    }

    /// Show these tiles (newest first), already chunked into `rows` of
    /// `columns` (off the main loop), keeping the scroll position.
    pub fn set_rows(&self, tiles: Vec<Arc<Tile>>, rows: Vec<RowItem>, columns: usize) {
        *self.tiles.borrow_mut() = tiles;
        if columns == self.columns.get() {
            self.show_rows(rows);
        } else {
            self.regrid();
        }
    }

    /// Re-chunk for a new column count.
    pub fn regrid(&self) {
        let rows = rows_of(&self.tiles.borrow(), self.columns.get());
        self.show_rows(rows);
    }

    fn show_rows(&self, rows: Vec<RowItem>) {
        let adj = self.scrolled.vadjustment();
        let fraction = if adj.upper() > adj.page_size() {
            adj.value() / (adj.upper() - adj.page_size())
        } else {
            0.0
        };
        let objects: Vec<glib::BoxedAnyObject> = rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objects);
        self.root
            .set_visible_child_name(if objects.is_empty() { "empty" } else { "grid" });
        // The list view anchors on the first row, which would hide the first
        // month header; put the scroll position back explicitly.
        let adj = adj.clone();
        glib::idle_add_local_once(move || adj.set_value(fraction * (adj.upper() - adj.page_size()).max(0.0)));
    }

    pub fn ids(&self) -> Vec<String> {
        self.tiles.borrow().iter().map(|t| t.id.clone()).collect()
    }

    /// Put a freshly loaded thumbnail on the tile showing `id`, if any.
    pub fn show_texture(&self, id: &str, texture: &gtk::gdk::Texture) {
        if let Some(p) = self.bound.borrow().get(id)
            && p.widget_name() == id
        {
            p.set_paintable(Some(texture));
        }
    }
}

/// Every widget currently realised under `root` (only the visible rows and a
/// few either side exist, so this stays small).
fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        f(&w);
        let mut c = w.first_child();
        while let Some(child) = c {
            c = child.next_sibling();
            stack.push(child);
        }
    }
}

/// A tile: an empty spacer fixes the size (a GtkPicture would ask for its
/// thumbnail's natural size), the picture and badge overlay it.
fn tile_widget() -> gtk::Button {
    let spacer = gtk::Box::builder().width_request(TILE).height_request(TILE).build();
    spacer.add_css_class("tile-spacer");
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .build();
    picture.add_css_class("tile-picture");
    let badge = gtk::Image::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .margin_end(6)
        .margin_bottom(6)
        .build();
    badge.add_css_class("tile-badge");
    let overlay = gtk::Overlay::builder().child(&spacer).build();
    overlay.add_overlay(&picture);
    overlay.add_overlay(&badge);
    // The action is set on bind, together with its target.
    let button = gtk::Button::builder().child(&overlay).build();
    button.add_css_class("flat");
    button.add_css_class("tile");
    button
}

fn tile_parts(button: &gtk::Button) -> (gtk::Picture, gtk::Image) {
    let overlay = button.child().and_downcast::<gtk::Overlay>().expect("tile overlay");
    let spacer = overlay.child().expect("tile spacer");
    let picture = spacer
        .next_sibling()
        .and_downcast::<gtk::Picture>()
        .expect("tile picture");
    let badge = picture.next_sibling().and_downcast::<gtk::Image>().expect("tile badge");
    (picture, badge)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, created: i64) -> GridRow {
        GridRow {
            id: id.into(),
            created,
            thumb_path: None,
            kind: Kind::Photo,
            is_live: false,
            filename: format!("{id}.JPG"),
        }
    }

    #[test]
    fn rows_chunk_by_columns_and_never_cross_a_month() {
        // Mid-month noon UTC, so no time zone moves these across a month.
        let sep = 1_789_905_600; // 2026-09-20
        let aug = sep - 31 * 86_400; // 2026-08-20
        let assets: Vec<GridRow> = (0..5)
            .map(|i| row(&format!("s{i}"), sep - i))
            .chain((0..2).map(|i| row(&format!("a{i}"), aug - i)))
            .collect();
        let tiles = tiles_of(assets);
        let rows = rows_of(&tiles, 3);
        let shape: Vec<(usize, &str)> = rows.iter().map(|r| (r.tiles.len(), &*r.label)).collect();
        assert_eq!(
            shape,
            vec![(3, "September 2026"), (2, "September 2026"), (2, "August 2026")]
        );
        assert!(rows[0].month > rows[2].month);
        assert_eq!(rows[1].tiles[0].id, "s3");
        // Re-chunking shares the tiles; nothing is re-read or re-dated.
        let wide = rows_of(&tiles, 10);
        assert_eq!(wide.len(), 2);
        assert!(Arc::ptr_eq(&wide[0].tiles[0], &tiles[0]));
    }

    #[test]
    fn month_labels_round_trip_the_key() {
        assert_eq!(&*month_label(2026 * 12 + 1), "January 2026");
        assert_eq!(&*month_label(2026 * 12 + 12), "December 2026");
    }
}
