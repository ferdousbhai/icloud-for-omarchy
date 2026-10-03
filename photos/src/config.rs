//! Where things live, and the two preferences.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

/// The XDG Pictures directory, read from `user-dirs.dirs` as Qt's
/// PicturesLocation reads it (Notes' vault sits in DocumentsLocation the
/// same way); `~/Pictures` without one.
fn pictures() -> PathBuf {
    let file = xdg("XDG_CONFIG_HOME", ".config").join("user-dirs.dirs");
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let value = line.trim().strip_prefix("XDG_PICTURES_DIR=")?.trim().trim_matches('"');
                match value.strip_prefix("$HOME") {
                    Some(rest) => Some(home().join(rest.trim_start_matches('/'))),
                    None => Some(PathBuf::from(value)).filter(|p| p.is_absolute()),
                }
            })
        })
        .filter(|p| *p != home())
        .unwrap_or_else(|| home().join("Pictures"))
}

#[derive(Debug, Clone)]
pub struct Dirs {
    /// `~/.local/share/icloud-photos` (catalog.db).
    pub data: PathBuf,
    /// `~/.cache/icloud-photos` (thumbs/, medium/).
    pub cache: PathBuf,
    /// `~/.config/icloud-photos` (settings.json).
    pub config: PathBuf,
}

impl Dirs {
    pub fn from_env() -> Dirs {
        Dirs {
            data: xdg("XDG_DATA_HOME", ".local/share").join("icloud-photos"),
            cache: xdg("XDG_CACHE_HOME", ".cache").join("icloud-photos"),
            config: xdg("XDG_CONFIG_HOME", ".config").join("icloud-photos"),
        }
    }

    /// Everything under one directory: `--data-dir` (data/, cache/,
    /// config/ beneath it), which the tests use too.
    pub fn under(root: &Path) -> Dirs {
        Dirs {
            data: root.join("data"),
            cache: root.join("cache"),
            config: root.join("config"),
        }
    }

    pub fn catalog(&self) -> PathBuf {
        self.data.join("catalog.db")
    }

    pub fn thumbs(&self) -> PathBuf {
        self.cache.join("thumbs")
    }

    pub fn medium(&self) -> PathBuf {
        self.cache.join("medium")
    }

    pub fn settings(&self) -> PathBuf {
        self.config.join("settings.json")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DownloadMode {
    /// Originals download when you open, download or export one.
    #[default]
    OnDemand,
    /// Every original downloads in the background after each sync.
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub library_dir: PathBuf,
    #[serde(default)]
    pub download: DownloadMode,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            library_dir: pictures().join("icloud-photos"),
            download: DownloadMode::OnDemand,
        }
    }
}

impl Settings {
    pub fn load(dirs: &Dirs) -> Settings {
        Settings::load_or(dirs, Settings::default())
    }

    /// The saved settings, or `fallback` when there are none (the CLI's
    /// `--data-dir` keeps the library under that directory too).
    pub fn load_or(dirs: &Dirs, fallback: Settings) -> Settings {
        std::fs::read(dirs.settings())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(fallback)
    }

    pub fn save(&self, dirs: &Dirs) -> std::io::Result<()> {
        std::fs::create_dir_all(&dirs.config)?;
        let tmp = dirs.settings().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, dirs.settings())
    }
}
