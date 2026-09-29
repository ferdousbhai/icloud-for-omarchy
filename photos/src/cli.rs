//! `icloud-photos <command>`: every feature of the app from a terminal (or an
//! agent), without GTK or a display. The commands run the same library code
//! as the window: `sync::run`, `thumbs::fetch_detailed`,
//! `upload::upload_batch`, `CloudKit::delete_asset`, `thumbs::prune_cache`.
//!
//! Output is plain text, or JSON on stdout with `--json`. Exit codes: 0 ok,
//! 1 error, 2 sign-in required, 64 usage.

use std::cell::RefCell;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use icloud_photos::catalog::{Catalog, LAST_SYNC_KEY, Row, SYNC_TOKEN_KEY};
use icloud_photos::cloudkit::{CloudKit, Kind, sanitize};
use icloud_photos::config::{Dirs, DownloadMode, Settings};
use icloud_photos::sync::{self, Mode, Progress, Report};
use icloud_photos::thumbs::{self, Job, Targets};
use icloud_photos::transport::{self, Error, MockTransport, Transport};
use icloud_photos::upload::{self, BatchEvent, Step};
use serde_json::{Value, json};

const EXIT_ERROR: u8 = 1;
const EXIT_SIGN_IN: u8 = 2;
const EXIT_USAGE: u8 = 64;

#[derive(Parser)]
#[command(
    name = "icloud-photos",
    version,
    about = "iCloud Photos from the command line. With no command, opens the app.",
    after_help = "Exit codes: 0 ok, 1 error, 2 sign-in required, 64 usage.\n\
                  ICLOUD_SESSION_MOCK=1 with ICLOUD_SESSION_MOCK_URL talks to the fake CloudKit server instead of Apple."
)]
struct Cli {
    /// Machine-readable JSON on stdout (errors as JSON on stderr).
    #[arg(long, global = true)]
    json: bool,
    /// Keep the catalog, cache, settings and library all under DIR instead
    /// of the XDG directories and ~/Pictures/iCloud.
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign-in state, where things live, catalog counts and the last sync.
    Status,
    /// Bring the catalog up to date (incremental, falling back to a full listing).
    Sync {
        /// List the whole library even when an incremental sync is possible.
        #[arg(long)]
        full: bool,
    },
    /// Albums in the catalog: id, name, photo count.
    Albums,
    /// Photos and videos in the catalog, newest first.
    List {
        /// Only this album (an id from `albums`).
        #[arg(long, value_name = "ID")]
        album: Option<String>,
        /// Only items taken at or after DATE (YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS[Z], UTC, or Unix seconds).
        #[arg(long, value_name = "DATE", value_parser = parse_date)]
        since: Option<i64>,
        /// At most N items.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Only this kind.
        #[arg(long, value_enum)]
        kind: Option<KindFilter>,
    },
    /// Everything the catalog knows about one item.
    Info { id: String },
    /// Fetch iCloud's thumbnail into the cache and print its path.
    Thumb {
        id: String,
        /// Also copy it to PATH.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
    },
    /// Download originals (and a Live Photo's video) into the library folder.
    Download {
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        ids: Vec<String>,
        /// Every original not downloaded yet (what "download all" does in the app).
        #[arg(long)]
        all: bool,
        /// iCloud's large JPEG preview (the viewer's image) instead of the original.
        #[arg(long)]
        medium: bool,
        /// Save originals under DIR/<year>/<month>/ instead of the library
        /// folder; with --medium, copy the preview to DIR.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
    /// Upload photos and videos, then sync so they appear in the catalog.
    Upload {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Also add the uploaded items to this album.
        #[arg(long, value_name = "ID")]
        album: Option<String>,
        /// Skip the sync afterwards.
        #[arg(long)]
        no_sync: bool,
    },
    /// Move items to Recently Deleted in iCloud (on every device).
    Delete {
        #[arg(required = true)]
        ids: Vec<String>,
        /// Do not ask; required when stdin is not a terminal.
        #[arg(long, short)]
        yes: bool,
    },
    /// Remove cached previews of deleted items and trim the preview cache.
    PruneCache,
    /// Open the iCloud sign-in window (in mock mode, sign the fake server back in).
    SignIn,
    /// Show the preferences, or change them.
    Config {
        /// Folder for downloaded originals.
        #[arg(long, value_name = "DIR")]
        library_dir: Option<PathBuf>,
        /// Download originals on demand, or all of them in the background (the app).
        #[arg(long, value_enum)]
        download: Option<DownloadArg>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum KindFilter {
    Photo,
    Video,
    Live,
}

#[derive(Clone, Copy, ValueEnum)]
enum DownloadArg {
    OnDemand,
    All,
}

enum Fail {
    Usage(String),
    Err(Error),
}

impl From<Error> for Fail {
    fn from(e: Error) -> Self {
        Fail::Err(e)
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail::Err(e.into())
    }
}

type Res<T> = std::result::Result<T, Fail>;

fn other(msg: impl Into<String>) -> Fail {
    Fail::Err(Error::Other(msg.into()))
}

/// Run the CLI on the process arguments.
pub fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let code = if e.use_stderr() { EXIT_USAGE } else { 0 };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
    let json = cli.json;
    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(fail) => {
            let (code, kind, msg) = match fail {
                Fail::Usage(m) => (EXIT_USAGE, "usage", m),
                Fail::Err(e) if e.is_sign_in() => (
                    EXIT_SIGN_IN,
                    "sign_in_required",
                    format!("{e}; run `icloud-photos sign-in` (or sign in from any iCloud app)"),
                ),
                Fail::Err(e) => (EXIT_ERROR, "error", e.to_string()),
            };
            if json {
                eprintln!("{}", json!({ "error": msg, "kind": kind, "exit_code": code }));
            } else {
                eprintln!("icloud-photos: {msg}");
            }
            ExitCode::from(code)
        }
    }
}

struct Ctx {
    json: bool,
    dirs: Dirs,
    settings: Settings,
}

impl Ctx {
    fn catalog(&self) -> Res<Catalog> {
        Ok(Catalog::open(&self.dirs.catalog())?)
    }

    fn transport(&self) -> Res<Arc<dyn Transport>> {
        Ok(transport::from_env()?)
    }

    fn targets(&self, library: Option<&Path>) -> Targets {
        Targets {
            dirs: self.dirs.clone(),
            library: library.map_or_else(|| self.settings.library_dir.clone(), Path::to_path_buf),
        }
    }

    /// Print `human` or, with --json, `value`.
    fn out(&self, value: &Value, human: impl FnOnce() -> String) {
        if self.json {
            println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
        } else {
            let text = human();
            if !text.is_empty() {
                println!("{text}");
            }
        }
    }
}

fn run(cli: Cli) -> Res<u8> {
    let (dirs, settings) = match &cli.data_dir {
        Some(dir) => {
            let root = std::path::absolute(dir)?;
            let dirs = Dirs::under(&root);
            let fallback = Settings {
                library_dir: root.join("library"),
                download: DownloadMode::OnDemand,
            };
            let settings = Settings::load_or(&dirs, fallback);
            (dirs, settings)
        }
        None => {
            let dirs = Dirs::from_env();
            let settings = Settings::load(&dirs);
            (dirs, settings)
        }
    };
    let ctx = Ctx {
        json: cli.json,
        dirs,
        settings,
    };
    match cli.command {
        Command::Status => status(&ctx),
        Command::Sync { full } => sync_cmd(&ctx, full),
        Command::Albums => albums(&ctx),
        Command::List {
            album,
            since,
            limit,
            kind,
        } => list(&ctx, album.as_deref(), since, limit, kind),
        Command::Info { id } => info(&ctx, &id),
        Command::Thumb { id, out } => thumb(&ctx, &id, out.as_deref()),
        Command::Download { ids, all, medium, out } => download(&ctx, ids, all, medium, out.as_deref()),
        Command::Upload { files, album, no_sync } => upload_cmd(&ctx, &files, album.as_deref(), no_sync),
        Command::Delete { ids, yes } => delete(&ctx, &ids, yes),
        Command::PruneCache => prune(&ctx),
        Command::SignIn => sign_in(&ctx),
        Command::Config { library_dir, download } => config(ctx, library_dir, download),
    }
}

// ---- status, sync, sign-in, config --------------------------------------

fn status(ctx: &Ctx) -> Res<u8> {
    let (signed_in, sign_in_error) = match transport::sign_in_state() {
        Ok(s) => (Some(s.signed_in), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let catalog = ctx.dirs.catalog();
    // Do not create a catalog just to report that there is none.
    let (assets, albums, downloaded, last_sync, token) = if catalog.exists() {
        let cat = ctx.catalog()?;
        let rows = cat.assets(None)?;
        let downloaded = rows
            .iter()
            .filter(|r| r.local_path.as_ref().is_some_and(|p| p.exists()))
            .count();
        let last = cat.meta(LAST_SYNC_KEY)?.and_then(|v| v.parse::<i64>().ok());
        (
            rows.len(),
            cat.albums()?.len(),
            downloaded,
            last,
            cat.meta(SYNC_TOKEN_KEY)?.is_some(),
        )
    } else {
        (0, 0, 0, None, false)
    };
    let mode = match ctx.settings.download {
        DownloadMode::OnDemand => "on-demand",
        DownloadMode::All => "all",
    };
    let v = json!({
        "signed_in": signed_in,
        "sign_in_error": sign_in_error,
        "mock": MockTransport::active(),
        "catalog": catalog,
        "catalog_exists": catalog.exists(),
        "assets": assets,
        "albums": albums,
        "downloaded": downloaded,
        "last_sync": last_sync.map(iso),
        "last_sync_unix": last_sync,
        "incremental_sync_ready": token,
        "library_dir": ctx.settings.library_dir,
        "cache_dir": ctx.dirs.cache,
        "settings": ctx.dirs.settings(),
        "download_mode": mode,
    });
    ctx.out(&v, || {
        let signed = match (signed_in, &sign_in_error) {
            (Some(true), _) => "yes".to_owned(),
            (Some(false), _) => "no (run `icloud-photos sign-in`)".to_owned(),
            (None, e) => format!("unknown ({})", e.as_deref().unwrap_or("")),
        };
        [
            format!(
                "Signed in:     {signed}{}",
                if MockTransport::active() { " [mock]" } else { "" }
            ),
            format!(
                "Catalog:       {}{}",
                catalog.display(),
                if catalog.exists() { "" } else { " (not created yet)" }
            ),
            format!("Items:         {assets} ({downloaded} downloaded)"),
            format!("Albums:        {albums}"),
            format!("Last sync:     {}", last_sync.map_or_else(|| "never".to_owned(), iso)),
            format!("Library:       {}", ctx.settings.library_dir.display()),
            format!("Cache:         {}", ctx.dirs.cache.display()),
            format!("Download mode: {mode}"),
        ]
        .join("\n")
    });
    Ok(if signed_in == Some(false) { EXIT_SIGN_IN } else { 0 })
}

fn report_json(r: &Report) -> Value {
    json!({
        "mode": match r.mode { Mode::Full => "full", Mode::Incremental => "incremental" },
        "assets": r.assets,
        "removed": r.removed,
        "albums": r.albums,
        "fell_back": r.fell_back,
    })
}

fn report_text(r: &Report) -> String {
    let mode = match r.mode {
        Mode::Full => "Full",
        Mode::Incremental => "Incremental",
    };
    let mut s = format!(
        "{mode} sync: {} updated, {} removed, {} albums",
        r.assets, r.removed, r.albums
    );
    if let Some(why) = &r.fell_back {
        s.push_str(&format!("\n(incremental sync failed: {why}; did a full listing)"));
    }
    s
}

/// Run the app's sync, with progress on stderr when it is a terminal.
fn run_sync(ctx: &Ctx, t: &dyn Transport, full: bool) -> Res<Report> {
    let tty = std::io::stderr().is_terminal() && !ctx.json;
    let progress = move |p: Progress| {
        if !tty {
            return;
        }
        let text = match p {
            Progress::Albums => "listing albums".to_owned(),
            Progress::Assets(n) => format!("listed {n} items"),
            Progress::AlbumMembers { done, total } => format!("album {}/{total}", done + 1),
            Progress::Changes(n) => format!("{n} changes"),
        };
        eprint!("\r\x1b[2Ksyncing: {text}");
    };
    let result = sync::run(t, &ctx.dirs, full, &progress);
    if tty {
        eprint!("\r\x1b[2K");
    }
    Ok(result?)
}

fn sync_cmd(ctx: &Ctx, full: bool) -> Res<u8> {
    let t = ctx.transport()?;
    let report = run_sync(ctx, &*t, full)?;
    ctx.out(&report_json(&report), || report_text(&report));
    Ok(0)
}

fn sign_in(ctx: &Ctx) -> Res<u8> {
    transport::start_sign_in(&|_| {})?;
    let mock = MockTransport::active();
    ctx.out(&json!({ "started": true, "mock": mock }), || {
        if mock {
            "Signed the fake server back in.".to_owned()
        } else {
            "Opened the iCloud sign-in window; `icloud-photos status` shows when it is done.".to_owned()
        }
    });
    Ok(0)
}

fn config(mut ctx: Ctx, library_dir: Option<PathBuf>, download: Option<DownloadArg>) -> Res<u8> {
    let changed = library_dir.is_some() || download.is_some();
    if let Some(dir) = library_dir {
        ctx.settings.library_dir = std::path::absolute(dir)?;
    }
    if let Some(d) = download {
        ctx.settings.download = match d {
            DownloadArg::OnDemand => DownloadMode::OnDemand,
            DownloadArg::All => DownloadMode::All,
        };
    }
    if changed {
        ctx.settings.save(&ctx.dirs)?;
    }
    let mode = match ctx.settings.download {
        DownloadMode::OnDemand => "on-demand",
        DownloadMode::All => "all",
    };
    let v = json!({ "library_dir": ctx.settings.library_dir, "download": mode, "file": ctx.dirs.settings(), "saved": changed });
    ctx.out(&v, || {
        format!(
            "library_dir = {}\ndownload    = {mode}",
            ctx.settings.library_dir.display()
        )
    });
    Ok(0)
}

// ---- reading the catalog ------------------------------------------------

fn kind_of(r: &Row) -> &'static str {
    match (r.kind, r.is_live) {
        (Kind::Video, _) => "video",
        (Kind::Photo, true) => "live",
        (Kind::Photo, false) => "photo",
    }
}

fn existing(p: &Option<PathBuf>) -> Option<&PathBuf> {
    p.as_ref().filter(|p| p.exists())
}

fn row_json(r: &Row) -> Value {
    json!({
        "id": r.id,
        "filename": r.filename,
        "created": iso(r.created),
        "created_unix": r.created,
        "kind": kind_of(r),
        "size": r.size,
        "width": r.w,
        "height": r.h,
        "local_path": existing(&r.local_path),
        "live_path": existing(&r.live_path),
    })
}

fn albums(ctx: &Ctx) -> Res<u8> {
    let rows = ctx.catalog()?.albums()?;
    let v: Vec<Value> = rows
        .iter()
        .map(|a| json!({ "id": a.id, "name": a.name, "count": a.count }))
        .collect();
    ctx.out(&Value::Array(v), || {
        rows.iter()
            .map(|a| format!("{}\t{}\t{}", a.id, a.count, a.name))
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(0)
}

fn list(ctx: &Ctx, album: Option<&str>, since: Option<i64>, limit: Option<usize>, kind: Option<KindFilter>) -> Res<u8> {
    let cat = ctx.catalog()?;
    if let Some(a) = album
        && !cat.albums()?.iter().any(|x| x.id == a)
    {
        return Err(other(format!(
            "no album {a} in the catalog (see `icloud-photos albums`)"
        )));
    }
    let want = kind.map(|k| match k {
        KindFilter::Photo => "photo",
        KindFilter::Video => "video",
        KindFilter::Live => "live",
    });
    let rows: Vec<Row> = cat
        .assets(album)?
        .into_iter()
        .filter(|r| since.is_none_or(|s| r.created >= s))
        .filter(|r| want.is_none_or(|k| kind_of(r) == k))
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    ctx.out(&Value::Array(rows.iter().map(row_json).collect()), || {
        rows.iter()
            .map(|r| {
                let local = existing(&r.local_path)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "-".into());
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    r.id,
                    iso(r.created),
                    kind_of(r),
                    r.size,
                    r.filename,
                    local
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(0)
}

fn row_or_fail(cat: &Catalog, id: &str) -> Res<Row> {
    cat.asset(id)?.ok_or_else(|| {
        other(format!(
            "no item {id} in the catalog (run `icloud-photos sync`, then `list`)"
        ))
    })
}

fn info(ctx: &Ctx, id: &str) -> Res<u8> {
    let cat = ctx.catalog()?;
    let r = row_or_fail(&cat, id)?;
    let albums = cat.albums_of(id)?;
    let mut v = row_json(&r);
    v["master_id"] = json!(r.master_id);
    v["deleted"] = json!(r.deleted);
    v["change_tag"] = json!(r.change_tag);
    v["original_type"] = json!(r.orig_type);
    v["live_type"] = json!(r.live_type);
    v["thumb_path"] = json!(existing(&r.thumb_path));
    v["medium_path"] = json!(existing(&r.medium_path));
    v["albums"] = albums
        .iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    ctx.out(&v, || {
        let path = |p: &Option<PathBuf>| existing(p).map_or_else(|| "-".to_owned(), |p| p.display().to_string());
        [
            format!("ID:        {}", r.id),
            format!("File:      {}", r.filename),
            format!("Taken:     {}", iso(r.created)),
            format!("Kind:      {}", kind_of(&r)),
            format!("Size:      {} bytes, {}x{}", r.size, r.w, r.h),
            format!(
                "Albums:    {}",
                if albums.is_empty() {
                    "-".into()
                } else {
                    albums.iter().map(|a| a.1.as_str()).collect::<Vec<_>>().join(", ")
                }
            ),
            format!("Original:  {}", path(&r.local_path)),
            format!("Live:      {}", path(&r.live_path)),
            format!("Thumbnail: {}", path(&r.thumb_path)),
            format!("Preview:   {}", path(&r.medium_path)),
            format!(
                "Deleted:   {}",
                if r.deleted {
                    "yes (in Recently Deleted or gone)"
                } else {
                    "no"
                }
            ),
        ]
        .join("\n")
    });
    Ok(0)
}

// ---- downloads ----------------------------------------------------------

fn copy_to(src: &Path, dest: &Path) -> Res<PathBuf> {
    if let Some(dir) = dest.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(src, dest)?;
    Ok(dest.to_path_buf())
}

fn thumb(ctx: &Ctx, id: &str, out: Option<&Path>) -> Res<u8> {
    let cat = ctx.catalog()?;
    row_or_fail(&cat, id)?;
    let t = ctx.transport()?;
    let cached = thumbs::fetch(&*t, &cat, &ctx.targets(None), id, Job::Thumb)?;
    let path = match out {
        Some(p) => copy_to(&cached, p)?,
        None => cached.clone(),
    };
    ctx.out(&json!({ "id": id, "path": path, "cache_path": cached }), || {
        path.display().to_string()
    });
    Ok(0)
}

fn download(ctx: &Ctx, ids: Vec<String>, all: bool, medium: bool, out: Option<&Path>) -> Res<u8> {
    let cat = ctx.catalog()?;
    let ids = if all { cat.missing_originals()? } else { ids };
    for id in &ids {
        row_or_fail(&cat, id)?;
    }
    let t = ctx.transport()?;
    let targets = ctx.targets(if medium { None } else { out });
    let mut results = Vec::new();
    let mut failed = 0;
    for id in &ids {
        let result: Res<Value> = (|| {
            if medium {
                let cached = thumbs::fetch(&*t, &cat, &targets, id, Job::Medium)?;
                let path = match out {
                    Some(dir) => copy_to(&cached, &dir.join(format!("{}.jpg", sanitize(id))))?,
                    None => cached,
                };
                return Ok(json!({ "id": id, "path": path }));
            }
            let fetched = thumbs::fetch_detailed(&*t, &cat, &targets, id, Job::Original)?;
            let row = row_or_fail(&cat, id)?;
            Ok(json!({
                "id": id,
                "path": fetched.path,
                "live_path": existing(&row.live_path),
                "live_error": fetched.live_error.map(|e| e.to_string()),
            }))
        })();
        match result {
            Ok(v) => {
                if !v["live_error"].is_null() {
                    failed += 1;
                }
                if !ctx.json {
                    println!("{}", v["path"].as_str().unwrap_or_default());
                    if let Some(p) = v["live_path"].as_str() {
                        println!("{p}");
                    }
                    if let Some(e) = v["live_error"].as_str() {
                        eprintln!("icloud-photos: {id}: Live Photo video: {e}");
                    }
                }
                results.push(v);
            }
            // A lapsed sign-in fails every other item too: stop.
            Err(Fail::Err(e)) if e.is_sign_in() => return Err(Fail::Err(e)),
            Err(Fail::Err(e)) => {
                failed += 1;
                if !ctx.json {
                    eprintln!("icloud-photos: {id}: {e}");
                }
                results.push(json!({ "id": id, "error": e.to_string() }));
            }
            Err(f) => return Err(f),
        }
    }
    if ctx.json {
        ctx.out(&Value::Array(results), String::new);
    }
    Ok(if failed > 0 { EXIT_ERROR } else { 0 })
}

// ---- upload -------------------------------------------------------------

fn upload_cmd(ctx: &Ctx, files: &[PathBuf], album: Option<&str>, no_sync: bool) -> Res<u8> {
    let (paths, skipped): (Vec<PathBuf>, Vec<PathBuf>) = files
        .iter()
        .cloned()
        .partition(|p| p.is_file() && upload::is_supported(p));
    let mut results: Vec<Value> = skipped
        .iter()
        .map(|p| {
            let why = if p.is_file() {
                "not a photo or video iCloud Photos takes"
            } else {
                "no such file"
            };
            eprintln!("icloud-photos: skipping {}: {why}", p.display());
            json!({ "file": p, "error": why })
        })
        .collect();
    if let Some(a) = album
        && !ctx.catalog()?.albums()?.iter().any(|x| x.id == a)
    {
        return Err(other(format!(
            "no album {a} in the catalog (see `icloud-photos albums`)"
        )));
    }
    let mut failed = skipped.len();
    if paths.is_empty() {
        ctx.out(&json!({ "files": results }), String::new);
        return Ok(EXIT_ERROR);
    }
    let t = ctx.transport()?;

    let done: RefCell<Vec<(usize, String, icloud_photos::transport::Result<upload::Uploaded>)>> =
        RefCell::new(Vec::new());
    let summary = upload::upload_batch(&*t, &paths, Duration::from_secs(60), &|| false, &|event| match event {
        BatchEvent::Step {
            index,
            total,
            name,
            step,
        } => {
            let text = match step {
                Step::Reserving => format!("preparing {name}"),
                Step::Sending { bytes } => format!("sending {name} ({bytes} bytes)"),
                Step::Registering => format!("adding {name} to the library"),
                Step::Ingesting { progress } => format!("iCloud is processing the upload ({progress}%)"),
            };
            eprintln!("[{}/{total}] {text}", (index + 1).min(total));
        }
        BatchEvent::FileDone { index, name, result } => done.borrow_mut().push((index, name, result)),
    });
    failed += summary.failed;

    let mut sign_in_lapsed = false;
    let mut new_ids: Vec<String> = Vec::new();
    for (index, name, result) in done.into_inner() {
        match result {
            Ok(u) => {
                new_ids.push(u.asset_id.clone());
                results.push(json!({ "file": paths[index], "asset_id": u.asset_id, "master_id": u.master_id, "duplicate": u.duplicate }));
            }
            Err(e) => {
                sign_in_lapsed |= e.is_sign_in();
                // An error before any file (the service unreachable) has no name.
                let file = paths.get(index).filter(|_| !name.is_empty());
                eprintln!(
                    "icloud-photos: {}: {e}",
                    file.map_or_else(|| "upload".into(), |p| p.display().to_string())
                );
                results.push(json!({ "file": file, "error": e.to_string() }));
            }
        }
    }
    if sign_in_lapsed && new_ids.is_empty() {
        return Err(Fail::Err(Error::SignInRequired));
    }

    let mut album_error = None;
    if let (Some(album), false) = (album, new_ids.is_empty()) {
        let ids: Vec<&str> = new_ids.iter().map(String::as_str).collect();
        match CloudKit::connect(&*t).and_then(|ck| ck.add_to_album(album, &ids)) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("icloud-photos: adding to album {album}: {e}");
                album_error = Some(e.to_string());
                failed += 1;
            }
        }
    }

    // CloudKit can take ~15-20 s to make new assets queryable: sync until
    // they are in the catalog, for up to a minute.
    let mut sync_report = None;
    let mut missing: Vec<String> = Vec::new();
    if !no_sync && !new_ids.is_empty() && !sign_in_lapsed {
        for attempt in 0..4 {
            if attempt > 0 {
                eprintln!("waiting for iCloud to index {} new item(s)…", missing.len());
                std::thread::sleep(Duration::from_secs(15));
            }
            sync_report = Some(run_sync(ctx, &*t, false)?);
            let cat = ctx.catalog()?;
            missing = new_ids
                .iter()
                .filter(|id| !cat.asset(id).ok().flatten().is_some_and(|r| !r.deleted))
                .cloned()
                .collect();
            if missing.is_empty() {
                break;
            }
        }
        if !missing.is_empty() {
            eprintln!(
                "icloud-photos: not in the catalog yet (a later sync will bring them): {}",
                missing.join(", ")
            );
        }
    }

    let v = json!({
        "files": results,
        "uploaded": summary.uploaded,
        "duplicates": summary.duplicates,
        "failed": failed,
        "album": album,
        "album_error": album_error,
        "sync": sync_report.as_ref().map(report_json),
        "not_in_catalog": missing,
    });
    ctx.out(&v, || {
        let mut lines: Vec<String> = results
            .iter()
            .filter_map(|r| {
                let id = r["asset_id"].as_str()?;
                Some(if r["duplicate"] == true {
                    format!("{id}\t(already in iCloud)")
                } else {
                    id.to_owned()
                })
            })
            .collect();
        if let Some(r) = &sync_report {
            lines.push(report_text(r));
        }
        lines.join("\n")
    });
    Ok(if sign_in_lapsed {
        EXIT_SIGN_IN
    } else if failed > 0 {
        EXIT_ERROR
    } else {
        0
    })
}

// ---- delete, prune ------------------------------------------------------

fn confirm(n: usize) -> Res<bool> {
    eprint!(
        "Move {n} item(s) to Recently Deleted in iCloud, on all your devices? They can be recovered there for about 30 days. [y/N] "
    );
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

fn delete(ctx: &Ctx, ids: &[String], yes: bool) -> Res<u8> {
    let mut cat = ctx.catalog()?;
    let mut rows = Vec::new();
    for id in ids {
        let r = row_or_fail(&cat, id)?;
        if r.deleted {
            return Err(other(format!("{id} is already deleted")));
        }
        rows.push(r);
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(Fail::Usage(
                "refusing to delete without --yes when stdin is not a terminal".into(),
            ));
        }
        if !ctx.json {
            for r in &rows {
                eprintln!("  {}\t{}", r.id, r.filename);
            }
        }
        if !confirm(rows.len())? {
            eprintln!("Nothing deleted.");
            return Ok(EXIT_ERROR);
        }
    }
    let t = ctx.transport()?;
    let ck = CloudKit::connect(&*t)?;
    let mut results = Vec::new();
    let mut failed = 0;
    for row in rows {
        let mut result = ck.delete_asset(&row.id, row.change_tag.as_deref());
        // Changed on another device since the last sync (as the app does:
        // sync, then try again with the fresh change tag).
        if matches!(&result, Err(Error::CloudKit { code, .. }) if code == "CONFLICT") {
            sync::sync(&ck, &mut cat, &|_| {})?;
            let fresh = row_or_fail(&cat, &row.id)?;
            result = if fresh.deleted {
                Err(Error::Other("already deleted on another device".into()))
            } else {
                ck.delete_asset(&row.id, fresh.change_tag.as_deref())
            };
        }
        match result {
            Ok(m) => {
                cat.mark_deleted(&row.id, m.change_tag.as_deref())?;
                if !ctx.json {
                    println!("{}\tmoved to Recently Deleted", row.id);
                }
                results.push(json!({ "id": row.id, "deleted": true }));
            }
            Err(e) if e.is_sign_in() => return Err(e.into()),
            Err(e) => {
                failed += 1;
                if !ctx.json {
                    eprintln!("icloud-photos: {}: {e}", row.id);
                }
                results.push(json!({ "id": row.id, "deleted": false, "error": e.to_string() }));
            }
        }
    }
    if ctx.json {
        ctx.out(&Value::Array(results), String::new);
    }
    Ok(if failed > 0 { EXIT_ERROR } else { 0 })
}

fn prune(ctx: &Ctx) -> Res<u8> {
    let cat = ctx.catalog()?;
    let removed = thumbs::prune_cache(&cat, &ctx.dirs, thumbs::MEDIUM_CACHE_CAP)?;
    ctx.out(&json!({ "removed": removed }), || {
        format!("Removed {removed} cached file(s)")
    });
    Ok(0)
}

// ---- dates --------------------------------------------------------------

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
fn iso(unix: i64) -> String {
    let (y, m, d) = civil_from_days(unix.div_euclid(86_400));
    let s = unix.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s / 60 % 60, s % 60)
}

/// `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM[:SS][Z]` (UTC), or Unix seconds.
fn parse_date(s: &str) -> std::result::Result<i64, String> {
    let bad = || format!("{s:?} is not YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS or Unix seconds");
    if let Ok(n) = s.parse::<i64>() {
        return Ok(n);
    }
    let (date, time) = s.split_once(['T', ' ']).unwrap_or((s, ""));
    let num = |p: &str| p.parse::<i64>().map_err(|_| bad());
    let parts: Vec<&str> = date.split('-').collect();
    let [y, m, d] = parts.as_slice() else { return Err(bad()) };
    let (y, m, d) = (num(y)?, num(m)?, num(d)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    let mut secs = 0;
    let time = time.trim_end_matches('Z');
    if !time.is_empty() {
        let t: Vec<&str> = time.split(':').collect();
        if !(2..=3).contains(&t.len()) {
            return Err(bad());
        }
        let (h, mi, se) = (num(t[0])?, num(t[1])?, t.get(2).map_or(Ok(0), |x| num(x))?);
        if h > 23 || mi > 59 || se > 60 {
            return Err(bad());
        }
        secs = h * 3600 + mi * 60 + se;
    }
    Ok(days_from_civil(y, m, d) * 86_400 + secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        assert_eq!(parse_date("1970-01-01"), Ok(0));
        assert_eq!(parse_date("2024-02-29T12:30:05Z"), Ok(1_709_209_805));
        assert_eq!(iso(1_709_209_805), "2024-02-29T12:30:05Z");
        assert_eq!(parse_date("1709209805"), Ok(1_709_209_805));
        assert_eq!(iso(-1), "1969-12-31T23:59:59Z");
        assert!(parse_date("2024-13-01").is_err());
        assert!(parse_date("yesterday").is_err());
        for day in [-800_000, -1, 0, 59, 11_016, 19_782, 2_000_000] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
    }

    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
