pub mod albums;
pub mod banner;
pub mod grid;
pub mod prefs;
pub mod upload;
pub mod viewer;
pub mod window;

pub const CSS: &str = "
.photo-grid, .photo-grid > row { background: none; padding: 0; }
.photo-grid > row:hover { background: none; }
button.tile { padding: 0; border-radius: 6px; min-width: 0; min-height: 0; }
.tile-picture { border-radius: 6px; background-color: alpha(currentColor, 0.08); }
.tile-badge { color: white; -gtk-icon-shadow: 0 1px 3px rgba(0,0,0,0.6); }
.viewer { background-color: black; }
";
