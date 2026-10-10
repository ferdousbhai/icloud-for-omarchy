//! The one-time move of downloaded originals out of `<Pictures>/icloud-photos`,
//! the library's default folder in earlier versions, straight into `<Pictures>`
//! (`~/Pictures/<year>/<month>/`). [`Settings::load`] runs it, so the app and
//! the command line both see the library in its new place before using it.
//!
//! Only renames: each entry of the old folder is renamed up a level, on the
//! same filesystem, and nothing is copied or replaced. A folder whose name
//! is already taken (a year folder in both) is merged file by file; a file
//! whose name is taken stays where it is and is reported. The catalog's
//! paths move with the files, one transaction per rename, so a download is
//! never thought missing. The old folder is removed only if it ends up
//! empty. A library the user chose themselves is never moved.

use std::io;
use std::path::{Path, PathBuf};

use crate::catalog::Catalog;
use crate::config::{Dirs, DownloadMode, Settings};
use crate::thumbs::rename_noreplace;
use crate::transport::Error;

/// The old default library folder's name, inside the Pictures folder.
pub const OLD_FOLDER: &str = "icloud-photos";

/// Catalog `meta` key set once the move has run: from then on the old
/// folder is never looked into again, even if something was left in it.
pub const MOVED_KEY: &str = "library_moved_to_pictures";

/// What a move did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// Entries renamed into place (a whole folder counts once).
    pub renamed: usize,
    /// Entries left in the old folder (their name was taken, or the rename
    /// failed), each with the reason.
    pub left: Vec<(PathBuf, String)>,
}

/// The settings to use, after moving a library still in the old default
/// folder `<pictures>/icloud-photos` into `pictures` itself. A saved
/// `library_dir` other than the old default is the user's choice: it is
/// returned as it is and nothing moves. A saved old default is rewritten to
/// the new one.
pub fn run(dirs: &Dirs, pictures: &Path) -> Settings {
    let saved = Settings::saved(dirs);
    let old = pictures.join(OLD_FOLDER);
    if let Some(s) = saved.as_ref().filter(|s| s.library_dir != old) {
        return s.clone();
    }
    let download = saved.as_ref().map_or(DownloadMode::default(), |s| s.download);
    let settings = |library_dir: PathBuf| Settings { library_dir, download };
    let rewrite = |s: Settings| {
        if saved.is_some()
            && let Err(e) = s.save(dirs)
        {
            eprintln!("icloud-photos: cannot update {}: {e}", dirs.settings().display());
        }
        s
    };
    match std::fs::symlink_metadata(&old) {
        Ok(m) if m.is_dir() => {}
        // A link to a folder elsewhere (another disk, say) is the user's own
        // arrangement: keep using it, and say so in the settings.
        Ok(m) if m.is_symlink() => {
            let s = settings(old);
            if saved.is_none()
                && let Err(e) = s.save(dirs)
            {
                eprintln!("icloud-photos: cannot save {}: {e}", dirs.settings().display());
            }
            return s;
        }
        // Nothing to move.
        _ => return rewrite(settings(pictures.to_path_buf())),
    }
    let mut cat = match Catalog::open(&dirs.catalog()) {
        Ok(c) => c,
        Err(e) => {
            // Without the catalog the paths cannot follow the files: leave
            // everything where it is for now and try again next time.
            eprintln!(
                "icloud-photos: not moving {} yet: cannot open {}: {e}",
                old.display(),
                dirs.catalog().display()
            );
            return settings(old);
        }
    };
    if cat.meta(MOVED_KEY).ok().flatten().is_none() {
        let moved = move_library(&mut cat, &old, pictures);
        if let Err(e) = cat.set_meta(MOVED_KEY, Some("1")) {
            eprintln!("icloud-photos: cannot record the library move: {e}");
        }
        log(&old, pictures, &moved);
    }
    rewrite(settings(pictures.to_path_buf()))
}

/// Rename everything in `from` into `to` (see the module comment), then
/// remove `from` if it is empty.
pub fn move_library(cat: &mut Catalog, from: &Path, to: &Path) -> Moved {
    let mut moved = Moved::default();
    merge(cat, from, to, &mut moved);
    let _ = std::fs::remove_dir(from);
    moved
}

fn merge(cat: &mut Catalog, from: &Path, to: &Path, out: &mut Moved) {
    let mut names: Vec<_> = match std::fs::read_dir(from) {
        Ok(entries) => entries.filter_map(|e| e.ok().map(|e| e.file_name())).collect(),
        Err(e) => {
            out.left.push((from.to_path_buf(), e.to_string()));
            return;
        }
    };
    names.sort();
    for name in names {
        let (src, dst) = (from.join(&name), to.join(&name));
        match std::fs::symlink_metadata(&dst) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let renamed = cat.transaction(|c| {
                    c.move_paths(&src, &dst)?;
                    rename_noreplace(&src, &dst)?;
                    Ok(())
                });
                match renamed {
                    Ok(()) => out.renamed += 1,
                    // Gone meanwhile (another instance moved it first).
                    Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => out.left.push((src, e.to_string())),
                }
            }
            Ok(m) if m.is_dir() && is_dir(&src) => {
                merge(cat, &src, &dst, out);
                let _ = std::fs::remove_dir(&src);
            }
            Ok(_) => out.left.push((src, format!("{} already exists", dst.display()))),
            Err(e) => out.left.push((src, e.to_string())),
        }
    }
}

/// A folder itself, not a link to one.
fn is_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

fn log(from: &Path, to: &Path, moved: &Moved) {
    eprintln!(
        "icloud-photos: moved the photo library from {} into {} ({} item(s) renamed, {} left in place)",
        from.display(),
        to.display(),
        moved.renamed,
        moved.left.len()
    );
    for (p, why) in &moved.left {
        eprintln!("icloud-photos: left {} in place: {why}", p.display());
    }
}
