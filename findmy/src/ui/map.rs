//! The map: OpenStreetMap tiles through libshumate, a marker per located
//! device, and the selected device's history trail as a polyline.

use std::rc::Rc;

use adw::prelude::*;
use shumate::prelude::*;

use crate::history::Point;
use crate::models::{Device, Fix};

/// Zoom used when centering on a device.
const DEVICE_ZOOM: f64 = 16.0;

pub struct DeviceMap {
    pub widget: shumate::SimpleMap,
    markers: shumate::MarkerLayer,
    trail: shumate::PathLayer,
    on_marker: Rc<dyn Fn(&str)>,
}

impl DeviceMap {
    /// `on_marker` runs with the device id when its marker is clicked.
    pub fn new(on_marker: impl Fn(&str) + 'static) -> Rc<Self> {
        let widget = shumate::SimpleMap::new();
        widget.set_vexpand(true);
        widget.set_hexpand(true);
        let registry = shumate::MapSourceRegistry::with_defaults();
        let source = registry.by_id(shumate::MAP_SOURCE_OSM_MAPNIK);
        widget.set_map_source(source.as_ref());
        let viewport = widget.viewport().expect("a SimpleMap always has a viewport");
        viewport.set_min_zoom_level(2);

        let trail = shumate::PathLayer::new(&viewport);
        trail.set_stroke_color(Some(&gtk::gdk::RGBA::new(0.21, 0.52, 0.89, 0.85)));
        trail.set_stroke_width(5.0);
        trail.set_outline_color(Some(&gtk::gdk::RGBA::new(1.0, 1.0, 1.0, 0.9)));
        trail.set_outline_width(1.5);
        let markers = shumate::MarkerLayer::new(&viewport);
        widget.add_overlay_layer(&trail);
        widget.add_overlay_layer(&markers);

        Rc::new(Self {
            widget,
            markers,
            trail,
            on_marker: Rc::new(on_marker),
        })
    }

    fn map(&self) -> Option<shumate::Map> {
        self.widget.map()
    }

    /// Redraws every device marker; the selected one is highlighted.
    pub fn set_devices(&self, devices: &[Device], selected: Option<&str>) {
        self.markers.remove_all();
        for d in devices {
            let Some(fix) = d.location else { continue };
            let marker = shumate::Marker::new();
            marker.set_location(fix.lat, fix.lon);
            marker.set_child(Some(&marker_content(d, selected == Some(d.id.as_str()))));
            let click = gtk::GestureClick::new();
            let (on_marker, id) = (self.on_marker.clone(), d.id.clone());
            click.connect_released(move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                on_marker(&id);
            });
            marker.add_controller(click);
            self.markers.add_marker(&marker);
        }
    }

    /// Animates to `fix` at street zoom.
    pub fn center_on(&self, fix: &Fix) {
        if let Some(map) = self.map() {
            map.go_to_full(fix.lat, fix.lon, DEVICE_ZOOM);
        }
    }

    /// Jumps (no animation) to the first located device, for first load.
    pub fn show_initial(&self, fix: &Fix) {
        if let Some(map) = self.map() {
            map.center_on(fix.lat, fix.lon);
            if let Some(viewport) = self.widget.viewport() {
                viewport.set_zoom_level(DEVICE_ZOOM - 2.0);
            }
        }
    }

    /// Draws the history trail, oldest to newest, ending at the device's
    /// current position when it is newer than the last stored point.
    pub fn set_trail(&self, points: &[Point], current: Option<&Fix>) {
        self.trail.remove_all();
        for p in points {
            self.trail.add_node(&shumate::Coordinate::new_full(p.lat, p.lon));
        }
        if let (Some(fix), Some(last)) = (current, points.last())
            && fix.ts_ms / 1000 > last.ts
        {
            self.trail.add_node(&shumate::Coordinate::new_full(fix.lat, fix.lon));
        }
    }

    pub fn clear_trail(&self) {
        self.trail.remove_all();
    }
}

/// A round badge with the model icon, centred on the position (so the trail
/// ends under it); the selected device's badge is accent-coloured. The name
/// is in the tooltip and the window subtitle.
fn marker_content(d: &Device, selected: bool) -> gtk::Image {
    let badge = gtk::Image::from_icon_name(d.class.icon_name());
    badge.add_css_class("device-marker");
    if selected {
        badge.add_css_class("selected");
    }
    if d.location.is_some_and(|f| f.is_old) || !d.online {
        badge.add_css_class("stale");
    }
    badge.set_tooltip_text(Some(&format!("{}\n{}", d.name, d.summary(crate::models::now_ms()))));
    badge.set_cursor_from_name(Some("pointer"));
    badge
}

/// Styles for the map badges, loaded once at startup.
pub const CSS: &str = "
.device-marker {
  padding: 7px;
  border-radius: 999px;
  background-color: alpha(black, 0.75);
  color: white;
  box-shadow: 0 1px 4px alpha(black, 0.45);
}
.device-marker.selected {
  padding: 9px;
  background-color: var(--accent-bg-color);
  color: var(--accent-fg-color);
}
.device-marker.stale { opacity: 0.6; }
";
