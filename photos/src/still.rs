//! A photo as an image for the desktop: the file the viewer shows, put on
//! the clipboard or made the Omarchy background. Shared by the viewer and
//! the command line.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::catalog::Row;
use crate::config::Dirs;

/// Omarchy's command that sets the background.
pub const WALLPAPER_COMMAND: &str = "omarchy-theme-bg-set";

/// The best local file to display: a JPEG/PNG original, else Apple's medium JPEG.
pub fn display_file(row: &Row) -> Option<PathBuf> {
    row.local_path
        .as_ref()
        .filter(|p| row.viewable_original() && p.exists())
        .or(row.medium_path.as_ref().filter(|p| p.exists()))
        .cloned()
}

/// Whether this desktop can set a wallpaper (Omarchy's command is on PATH).
pub fn has_wallpaper_command() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(WALLPAPER_COMMAND).is_file()))
}

/// Make `path` the Omarchy background with `omarchy-theme-bg-set` (or
/// `$ICLOUD_PHOTOS_WALLPAPER_SETTER`). A rendition from the cache (which is
/// pruned) is copied out of it first, into the data folder, which keeps only
/// the current wallpaper. Returns the file the background now points at.
pub fn set_wallpaper(path: &Path, dirs: &Dirs) -> Result<PathBuf, String> {
    let path = if path.starts_with(&dirs.cache) {
        keep_copy(path, &dirs.data.join("wallpaper"))?
    } else {
        path.to_path_buf()
    };
    let setter = std::env::var("ICLOUD_PHOTOS_WALLPAPER_SETTER").unwrap_or_else(|_| WALLPAPER_COMMAND.into());
    let out = Command::new(&setter)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {setter}: {e}"))?;
    if out.status.success() {
        Ok(path)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Put the image file on the Wayland clipboard with `wl-copy` (or
/// `$ICLOUD_PHOTOS_COPIER`), as its own type (JPEG or PNG). Returns that type.
pub fn copy_to_clipboard(path: &Path) -> Result<&'static str, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mime = if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else {
        "image/jpeg"
    };
    let copier = std::env::var("ICLOUD_PHOTOS_COPIER").unwrap_or_else(|_| "wl-copy".into());
    // wl-copy keeps serving the clipboard in the background after it exits,
    // so its output is not waited on (that would wait for the next copy).
    let mut child = Command::new(&copier)
        .args(["--type", mime])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {copier}: {e}"))?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(&bytes)
        .map_err(|e| e.to_string())?;
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(mime)
    } else {
        Err(format!("{copier} failed ({status})"))
    }
}

/// Copy `path` into `dir` as the only file there, so the wallpaper outlives
/// cache pruning without piling up.
fn keep_copy(path: &Path, dir: &Path) -> Result<PathBuf, String> {
    let dest = dir.join(path.file_name().unwrap_or_default());
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::copy(path, &dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        if entry.path() != dest {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::keep_copy;

    #[test]
    fn keep_copy_leaves_only_the_new_wallpaper() {
        let root = std::env::temp_dir().join(format!("icloud-photos-wallpaper-{}", std::process::id()));
        let (cache, keep) = (root.join("cache"), root.join("keep"));
        std::fs::create_dir_all(&cache).unwrap();
        for name in ["A.JPG", "B.JPG"] {
            std::fs::write(cache.join(name), name).unwrap();
        }
        assert_eq!(keep_copy(&cache.join("A.JPG"), &keep).unwrap(), keep.join("A.JPG"));
        let kept = keep_copy(&cache.join("B.JPG"), &keep).unwrap();
        // Setting the same one again copies over it in place.
        assert_eq!(keep_copy(&cache.join("B.JPG"), &keep).unwrap(), kept);
        let files: Vec<_> = std::fs::read_dir(&keep)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(files, ["B.JPG"]);
        assert_eq!(std::fs::read(&kept).unwrap(), b"B.JPG");
        // The cache's files are untouched.
        assert!(cache.join("A.JPG").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
