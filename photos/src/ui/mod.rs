pub mod albums;
pub mod banner;
pub mod decode;
pub mod grid;
pub mod prefs;
pub mod upload;
pub mod viewer;
pub mod window;

/// Over omui's stylesheet: the photo grid's tiles and their Live Photo and
/// video badges, and the viewer's black around a photo.
pub const CSS: &str = "
.photo-grid, .photo-grid > row { background: none; padding: 0; }
.photo-grid > row:hover { background: none; }
button.tile { padding: 0; border: 1px solid transparent; border-radius: 6px; min-width: 0; min-height: 0; }
button.tile:focus-visible { border-color: @om_accent; outline: none; }
.tile-picture { border-radius: 6px; background-color: alpha(currentColor, 0.08); }
.tile-badge { color: white; -gtk-icon-shadow: 0 1px 3px rgba(0,0,0,0.6); }
.viewer { background-color: black; }
";
