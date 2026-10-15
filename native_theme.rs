//! Native chrome follows Omarchy's palette, including theme switches.
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

thread_local! {
    static MONITORS: RefCell<Vec<gio::FileMonitor>> = const { RefCell::new(Vec::new()) };
}

fn current() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("omarchy/current")
}

fn palette() -> String {
    std::fs::read_to_string(current().join("theme/colors.toml")).unwrap_or_default()
}

fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (name, rest) = line.split_once('=')?;
        if name.trim() != key {
            return None;
        }
        let rest = rest.trim();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        rest.get(1..)?.split(quote).next()
    })
}

fn apply(provider: &gtk::CssProvider, base: &str) {
    let text = palette();
    let mut css = base.to_string();
    let mut properties = String::new();
    for (key, names) in [
        (
            "background",
            &[
                "window_bg_color",
                "view_bg_color",
                "headerbar_bg_color",
                "card_bg_color",
                "popover_bg_color",
                "sidebar_bg_color",
                "secondary_sidebar_bg_color",
                "dialog_bg_color",
            ][..],
        ),
        (
            "foreground",
            &[
                "window_fg_color",
                "view_fg_color",
                "headerbar_fg_color",
                "card_fg_color",
                "popover_fg_color",
                "sidebar_fg_color",
                "secondary_sidebar_fg_color",
                "dialog_fg_color",
            ][..],
        ),
        ("accent", &["accent_bg_color", "accent_color"][..]),
    ] {
        if let Some(color) = value(&text, key).filter(|v| v.starts_with('#') && gdk::RGBA::parse(*v).is_ok()) {
            for name in names {
                css.push_str(&format!("\n@define-color {name} {color};"));
                properties.push_str(&format!("--{}: {color};", name.replace('_', "-")));
            }
        }
    }
    css.push_str(&format!("\n.cloud-desktop {{ {properties} }}"));
    provider.load_from_string(&css);
    let scheme = match value(&text, "mode") {
        Some("dark") => adw::ColorScheme::PreferDark,
        Some("light") => adw::ColorScheme::PreferLight,
        _ => adw::ColorScheme::Default,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

pub fn install(base: &'static str) {
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER);
    apply(&provider, base);
    let last = Rc::new(RefCell::new(palette()));
    let mut monitors = Vec::new();
    for path in [current(), current().join("theme")] {
        let Ok(monitor) =
            gio::File::for_path(path).monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        else {
            continue;
        };
        let provider = provider.clone();
        let last = last.clone();
        monitor.connect_changed(move |_, _, _, _| {
            let provider = provider.clone();
            let last = last.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                let text = palette();
                if text != *last.borrow() {
                    *last.borrow_mut() = text;
                    apply(&provider, base);
                }
            });
        });
        monitors.push(monitor);
    }
    MONITORS.with(|m| *m.borrow_mut() = monitors);
}
