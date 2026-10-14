//! `icloud-photos <command>`: every feature of the app from a terminal (or an
//! agent), without GTK or a display. The commands run the same library code
//! as the window: `sync::run`, `thumbs::fetch_detailed` and the download pool,
//! `upload::upload_batch`, `CloudKit::delete_assets`, `thumbs::prune_cache`.
//!
//! Output follows the shared product contract ([`icloud_session::output`]):
//! text on a terminal, the JSON envelope `{"ok":true,"data","summary"}` when
//! piped, and `--json`/`--styled`/`--quiet`/`--ids-only`/`--count` to choose.
//! A failure in a machine mode is `{"ok":false,"error":{"code","message","hint"?}}`
//! on stdout; in text mode `error:`/`hint:` lines on stderr. Progress (sync
//! on a terminal, upload steps) goes to stderr.
//!
//! Exit codes, as in every iCloud tool (docs/CLI.md): 0 ok, 1 error (also
//! some items of a `download`/`upload`/`delete` failed: the data still lists
//! every item, each failed one with its `error`), 2 usage (or `delete`
//! without `--yes` when nobody can be asked), 3 not signed in (or iCloud
//! Photos not enabled for the account), 4 not found, 5 a service unavailable
//! (offline, network, icloud-sessiond, keyring, Apple 5xx).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use icloud_photos::catalog::{Catalog, LAST_SYNC_KEY, Row, SYNC_TOKEN_KEY};
use icloud_photos::cloudkit::{CloudKit, Kind, Modified, sanitize};
use icloud_photos::config::{Dirs, DownloadMode, Settings};
use icloud_photos::session;
use icloud_photos::sync::{self, Mode, Progress, Report};
use icloud_photos::thumbs::{self, Downloader, Job, Priority, Targets};
use icloud_photos::transport::{Error, Transport};
use icloud_photos::upload::{self, BatchEvent, Step};
use icloud_session::cli::{self, EXIT_ERROR, EXIT_NOT_FOUND, EXIT_SIGN_IN, EXIT_UNAVAILABLE, EXIT_USAGE};
use icloud_session::output::{
    self, CliError, KEYRING_HINT, Mode as OutMode, NETWORK_HINT, OutputFlags, Response, SERVICE_TROUBLE_HINT,
    SESSION_SERVICE_HINT, SIGN_IN_HINT,
};
use icloud_session::time::{days_from_civil, rfc3339};
use serde_json::{Value, json};

const AFTER_HELP: &str = "\
Output: human-readable text on a terminal, a JSON envelope
{\"ok\":true,\"data\",\"summary\"} when piped. --json forces the envelope,
--styled text, --quiet prints only the data, --ids-only one id per line
(albums: album ids; list, info, thumb, open: item ids; download, delete: the
items that succeeded; upload: the new asset ids), --count the number of
results (list, albums; the ids above; prune-cache: files removed). Flags
combine: ids-only > count > quiet > json > styled. A failure is
{\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout, or
error:/hint: lines on stderr in text mode. Progress goes to stderr.

Exit codes:
  0 ok
  1 error (cancelled, error, io, http 4xx), or some items of download, upload or
    delete failed (the data lists each item with its error)
  2 usage, or delete without --yes when nobody can be asked
  3 sign_in_required (icloud-session sign-in; status also exits 3 when
    signed out), photos_unavailable (iCloud Photos is not on for this
    Apple ID), session_rejected (iCloud keeps refusing the session although
    icloud-sessiond still holds it: sign in again)
  4 not_found
  5 offline, network, session_service (icloud-sessiond unreachable),
    keyring_unavailable, bad_response, http 5xx

ICLOUD_SESSION_MOCK=1 with ICLOUD_SESSION_MOCK_URL talks to the fake CloudKit server instead of Apple.";

#[derive(Parser)]
#[command(
    name = "icloud-photos",
    version,
    about = "iCloud Photos from the command line. With no command, opens the app.",
    after_help = AFTER_HELP
)]
struct Cli {
    #[command(flatten)]
    out: OutputFlags,
    /// Keep the catalog, cache, settings and library all under DIR instead
    /// of the XDG directories and ~/Pictures.
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign-in state, where things live, catalog counts and the last sync.
    #[command(after_help = AFTER_HELP)]
    Status,
    /// Bring the catalog up to date (incremental, falling back to a full listing).
    #[command(after_help = AFTER_HELP)]
    Sync {
        /// List the whole library even when an incremental sync is possible.
        #[arg(long)]
        full: bool,
    },
    /// Albums in the catalog: id, name, photo count.
    #[command(after_help = AFTER_HELP)]
    Albums,
    /// Photos and videos in the catalog, newest first.
    #[command(after_help = AFTER_HELP)]
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
    #[command(after_help = AFTER_HELP)]
    Info { id: String },
    /// Fetch iCloud's thumbnail into the cache and print its path.
    #[command(after_help = AFTER_HELP)]
    Thumb {
        id: String,
        /// Also copy it to PATH.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
    },
    /// Download originals (and a Live Photo's video) into the library folder.
    ///
    /// In text mode prints each saved path as it finishes (failures on
    /// stderr). The data is one entry per id asked for, in that order:
    /// {id, path, live_path, live_error} or {id, error}. Exits 1 if any
    /// item failed.
    #[command(after_help = AFTER_HELP)]
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
    /// Download the original if needed and open it in the default app (the viewer's Open).
    #[command(after_help = AFTER_HELP)]
    Open {
        id: String,
        /// Open iCloud's large JPEG preview instead of the original.
        #[arg(long)]
        medium: bool,
    },
    /// Upload photos and videos, then sync so they appear in the catalog.
    ///
    /// Progress on stderr. The data is {files: [{file, asset_id,
    /// master_id, duplicate} | {file, error}], uploaded, duplicates,
    /// failed, album, album_error, sync, not_in_catalog}. Exits 1 if any
    /// file failed.
    #[command(after_help = AFTER_HELP)]
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
    ///
    /// Asks in text mode on a terminal (stdin and stdout); anywhere else
    /// --yes is required (a usage error, exit 2, otherwise). The data is
    /// [{id, deleted, error?}]. Exits 1 if any item failed.
    #[command(after_help = AFTER_HELP)]
    Delete {
        #[arg(required = true)]
        ids: Vec<String>,
        /// Do not ask; required when nobody can be asked (not a terminal, or a machine output mode).
        #[arg(long, short)]
        yes: bool,
    },
    /// Remove cached previews of deleted items and trim the preview cache.
    #[command(after_help = AFTER_HELP)]
    PruneCache,
    /// Open the iCloud sign-in window (in mock mode, sign the fake server back in).
    #[command(after_help = AFTER_HELP)]
    SignIn,
    /// Show the preferences, or change them.
    #[command(after_help = AFTER_HELP)]
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

/// A failure: either already the contract's error object (usage, not
/// found, cancelled), or a library error that [`classify`] maps.
enum Fail {
    Cli(CliError),
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

impl From<CliError> for Fail {
    fn from(e: CliError) -> Self {
        Fail::Cli(e)
    }
}

type Res<T> = std::result::Result<T, Fail>;

fn other(msg: impl Into<String>) -> Fail {
    Fail::Err(Error::Other(msg.into()))
}

fn usage(msg: impl Into<String>) -> CliError {
    CliError::new("usage", EXIT_USAGE, msg)
}

fn not_found(msg: impl Into<String>) -> CliError {
    CliError::new("not_found", EXIT_NOT_FOUND, msg)
}

fn cancelled(msg: impl Into<String>) -> CliError {
    CliError::new("cancelled", EXIT_ERROR, msg)
}

/// Run the CLI on the process arguments.
pub fn main() -> ExitCode {
    let cli = match output::parse::<Cli>() {
        Ok(c) => c,
        Err(code) => return ExitCode::from(code),
    };
    let mode = cli.out.mode();
    let code = match run(cli, mode) {
        Ok(resp) => resp.print(mode),
        Err(fail) => classify(fail).report(mode),
    };
    ExitCode::from(code)
}

/// A failure as the contract's error: exit code, JSON `code`, message and
/// remediation hint, per the shared mapping (docs/CLI.md).
fn classify(fail: Fail) -> CliError {
    let e = match fail {
        Fail::Cli(e) => return e,
        Fail::Err(e) => e,
    };
    let msg = e.to_string();
    let with_photos_hint = |e: CliError| {
        if e.hint.as_deref() == Some(SIGN_IN_HINT) {
            e.hint(sign_in_hint())
        } else {
            e
        }
    };
    match e {
        Error::SignInRequired => output::sign_in_required().hint(sign_in_hint()),
        Error::NotOffered(_) => CliError::new("photos_unavailable", EXIT_SIGN_IN, msg)
            .hint("turn on iCloud Photos for this Apple ID (on an Apple device or icloud.com), then try again"),
        Error::Offline(_) => CliError::new("offline", EXIT_UNAVAILABLE, msg).hint(NETWORK_HINT),
        Error::Network(_) => CliError::new("network", EXIT_UNAVAILABLE, msg).hint(NETWORK_HINT),
        Error::BadResponse(_) => CliError::new("bad_response", EXIT_UNAVAILABLE, msg).hint(SERVICE_TROUBLE_HINT),
        Error::Service(_) => CliError::new("session_service", EXIT_UNAVAILABLE, msg).hint(SESSION_SERVICE_HINT),
        Error::KeyringUnavailable(_) => CliError::new("keyring_unavailable", EXIT_UNAVAILABLE, msg).hint(KEYRING_HINT),
        // 401/421 here persisted although the daemon says signed in:
        // `session_rejected`; 5xx exit 5; other 4xx exit 1.
        Error::Http { status, .. } => with_photos_hint(output::http_failure(status, msg)),
        Error::Io(_) => CliError::new("io", EXIT_ERROR, msg),
        _ => CliError::new("error", EXIT_ERROR, msg),
    }
}

/// The shared sign-in hint, plus Photos' own ways to sign in.
fn sign_in_hint() -> String {
    format!("{SIGN_IN_HINT}, or `icloud-photos sign-in`, or sign in from any iCloud app")
}

struct Ctx {
    mode: OutMode,
    dirs: Dirs,
    settings: Settings,
}

impl Ctx {
    fn catalog(&self) -> Res<Catalog> {
        Ok(Catalog::open(&self.dirs.catalog())?)
    }

    fn transport(&self) -> Res<Arc<dyn Transport>> {
        Ok(session::connect()?)
    }

    fn targets(&self, library: Option<&Path>) -> Targets {
        Targets {
            dirs: self.dirs.clone(),
            library: library.map_or_else(|| self.settings.library_dir.clone(), Path::to_path_buf),
        }
    }

    /// Text mode: per-item lines are streamed as they happen.
    fn text(&self) -> bool {
        self.mode == OutMode::Human
    }
}

/// A line on stdout, safe for a terminal (file names come from iCloud).
fn say(line: &str) {
    println!("{}", output::terminal_safe(line));
}

/// A line on stderr, safe for a terminal.
fn warn(line: &str) {
    eprintln!("{}", output::terminal_safe(line));
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn run(cli: Cli, mode: OutMode) -> Res<Response> {
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
    let ctx = Ctx { mode, dirs, settings };
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
        Command::Open { id, medium } => open(&ctx, &id, medium),
        Command::Upload { files, album, no_sync } => upload_cmd(&ctx, &files, album.as_deref(), no_sync),
        Command::Delete { ids, yes } => delete(&ctx, &ids, yes),
        Command::PruneCache => prune(&ctx),
        Command::SignIn => sign_in(),
        Command::Config { library_dir, download } => config(ctx, library_dir, download),
    }
}

// ---- status, sync, sign-in, config --------------------------------------

fn status(ctx: &Ctx) -> Res<Response> {
    let (signed_in, sign_in_error) = match session::sign_in_state() {
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
        "mock": session::is_mock(),
        "catalog": catalog,
        "catalog_exists": catalog.exists(),
        "assets": assets,
        "albums": albums,
        "downloaded": downloaded,
        "last_sync": last_sync.map(rfc3339),
        "last_sync_unix": last_sync,
        "incremental_sync_ready": token,
        "library_dir": ctx.settings.library_dir,
        "cache_dir": ctx.dirs.cache,
        "settings": ctx.dirs.settings(),
        "download_mode": mode,
    });
    let signed = match (signed_in, &sign_in_error) {
        (Some(true), _) => "yes".to_owned(),
        (Some(false), _) => "no (run `icloud-photos sign-in`)".to_owned(),
        (None, e) => format!("unknown ({})", e.as_deref().unwrap_or("")),
    };
    let human = [
        format!(
            "Signed in:     {signed}{}",
            if session::is_mock() { " [mock]" } else { "" }
        ),
        format!(
            "Catalog:       {}{}",
            catalog.display(),
            if catalog.exists() { "" } else { " (not created yet)" }
        ),
        format!("Items:         {assets} ({downloaded} downloaded)"),
        format!("Albums:        {albums}"),
        format!(
            "Last sync:     {}",
            last_sync.map_or_else(|| "never".to_owned(), rfc3339)
        ),
        format!("Library:       {}", ctx.settings.library_dir.display()),
        format!("Cache:         {}", ctx.dirs.cache.display()),
        format!("Download mode: {mode}"),
    ]
    .join("\n");
    let state = match signed_in {
        Some(true) => "Signed in",
        Some(false) => "Not signed in",
        None => "Sign-in state unknown",
    };
    let summary = format!(
        "{state}; {}, {}",
        plural(assets, "item", "items"),
        plural(albums, "album", "albums")
    );
    let resp = Response::new(v, summary).human(human);
    Ok(if signed_in == Some(false) {
        resp.exit(EXIT_SIGN_IN)
    } else {
        resp
    })
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

fn report_summary(r: &Report) -> String {
    let mode = match r.mode {
        Mode::Full => "Full",
        Mode::Incremental => "Incremental",
    };
    format!(
        "{mode} sync: {} updated, {} removed, {} albums",
        r.assets, r.removed, r.albums
    )
}

fn report_text(r: &Report) -> String {
    let mut s = report_summary(r);
    if let Some(why) = &r.fell_back {
        s.push_str(&format!("\n(incremental sync failed: {why}; did a full listing)"));
    }
    s
}

/// Run the app's sync, with progress on stderr in text mode when stderr is a
/// terminal.
fn run_sync(ctx: &Ctx, t: &dyn Transport, full: bool) -> Res<Report> {
    let tty = ctx.text() && std::io::stderr().is_terminal();
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

fn sync_cmd(ctx: &Ctx, full: bool) -> Res<Response> {
    let t = ctx.transport()?;
    let report = run_sync(ctx, &*t, full)?;
    Ok(Response::new(report_json(&report), report_summary(&report)).human(report_text(&report)))
}

fn sign_in() -> Res<Response> {
    session::start_sign_in(&|_| {})?;
    let mock = session::is_mock();
    let summary = if mock {
        "Signed the fake server back in."
    } else {
        "Opened the iCloud sign-in window; `icloud-photos status` shows when it is done."
    };
    Ok(Response::new(json!({ "started": true, "mock": mock }), summary))
}

fn config(mut ctx: Ctx, library_dir: Option<PathBuf>, download: Option<DownloadArg>) -> Res<Response> {
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
    let summary = if changed {
        "Saved the preferences"
    } else {
        "The preferences"
    };
    let human = format!(
        "library_dir = {}\ndownload    = {mode}",
        ctx.settings.library_dir.display()
    );
    Ok(Response::new(v, summary).human(human))
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
        "created": rfc3339(r.created),
        "created_unix": r.created,
        "kind": kind_of(r),
        "size": r.size,
        "width": r.w,
        "height": r.h,
        "local_path": existing(&r.local_path),
        "live_path": existing(&r.live_path),
    })
}

fn albums(ctx: &Ctx) -> Res<Response> {
    let rows = ctx.catalog()?.albums()?;
    let v: Vec<Value> = rows
        .iter()
        .map(|a| json!({ "id": a.id, "name": a.name, "count": a.count }))
        .collect();
    let human = rows
        .iter()
        .map(|a| format!("{}\t{}\t{}", a.id, a.count, a.name))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Response::new(v, plural(rows.len(), "album", "albums"))
        .human(human)
        .ids(rows.iter().map(|a| a.id.clone()).collect()))
}

fn check_album(cat: &Catalog, album: &str) -> Res<()> {
    if cat.albums()?.iter().any(|x| x.id == album) {
        Ok(())
    } else {
        Err(not_found(format!("no album {album} in the catalog"))
            .hint("see `icloud-photos albums` (or run `icloud-photos sync` first)")
            .into())
    }
}

fn list(
    ctx: &Ctx,
    album: Option<&str>,
    since: Option<i64>,
    limit: Option<usize>,
    kind: Option<KindFilter>,
) -> Res<Response> {
    let cat = ctx.catalog()?;
    if let Some(a) = album {
        check_album(&cat, a)?;
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
    let human = rows
        .iter()
        .map(|r| {
            let local = existing(&r.local_path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".into());
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                r.id,
                rfc3339(r.created),
                kind_of(r),
                r.size,
                r.filename,
                local
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let data: Vec<Value> = rows.iter().map(row_json).collect();
    Ok(Response::new(data, plural(rows.len(), "item", "items"))
        .human(human)
        .ids(rows.iter().map(|r| r.id.clone()).collect()))
}

fn row_or_fail(cat: &Catalog, id: &str) -> Res<Row> {
    cat.asset(id)?.ok_or_else(|| {
        not_found(format!("no item {id} in the catalog"))
            .hint("run `icloud-photos sync`, then `icloud-photos list`")
            .into()
    })
}

fn info(ctx: &Ctx, id: &str) -> Res<Response> {
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
    let path = |p: &Option<PathBuf>| existing(p).map_or_else(|| "-".to_owned(), |p| p.display().to_string());
    let human = [
        format!("ID:        {}", r.id),
        format!("File:      {}", r.filename),
        format!("Taken:     {}", rfc3339(r.created)),
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
    .join("\n");
    Ok(
        Response::new(v, format!("{} ({}, {})", r.filename, kind_of(&r), rfc3339(r.created)))
            .human(human)
            .ids(vec![r.id.clone()]),
    )
}

// ---- downloads ----------------------------------------------------------

fn copy_to(src: &Path, dest: &Path) -> Res<PathBuf> {
    if let Some(dir) = dest.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(src, dest)?;
    Ok(dest.to_path_buf())
}

fn thumb(ctx: &Ctx, id: &str, out: Option<&Path>) -> Res<Response> {
    let cat = ctx.catalog()?;
    row_or_fail(&cat, id)?;
    let t = ctx.transport()?;
    let cached = thumbs::fetch(&*t, &cat, &ctx.targets(None), id, Job::Thumb)?;
    let path = match out {
        Some(p) => copy_to(&cached, p)?,
        None => cached.clone(),
    };
    let text = path.display().to_string();
    Ok(Response::new(json!({ "id": id, "path": path, "cache_path": cached }), text).ids(vec![id.to_owned()]))
}

/// The viewer's Open: the original (downloaded first when it is not yet),
/// or with `medium` iCloud's preview, handed to the desktop's default app
/// (`xdg-open`, or `$ICLOUD_PHOTOS_OPENER`).
fn open(ctx: &Ctx, id: &str, medium: bool) -> Res<Response> {
    let cat = ctx.catalog()?;
    let row = row_or_fail(&cat, id)?;
    let path = match existing(&row.local_path) {
        Some(p) if !medium => p.clone(),
        _ => {
            let t = ctx.transport()?;
            let job = if medium { Job::Medium } else { Job::Original };
            thumbs::fetch_detailed(&*t, &cat, &ctx.targets(None), id, job)?.path
        }
    };
    let opener = std::env::var("ICLOUD_PHOTOS_OPENER").unwrap_or_else(|_| "xdg-open".into());
    let status = std::process::Command::new(&opener)
        .arg(&path)
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| other(format!("could not run {opener}: {e}")))?;
    if !status.success() {
        return Err(other(format!("{opener} {} failed ({status})", path.display())));
    }
    let text = path.display().to_string();
    Ok(Response::new(json!({ "id": id, "path": path, "opened": true }), text).ids(vec![id.to_owned()]))
}

/// Parallel downloads, as in the app.
const DOWNLOAD_WORKERS: usize = 4;

fn download(ctx: &Ctx, ids: Vec<String>, all: bool, medium: bool, out: Option<&Path>) -> Res<Response> {
    let cat = ctx.catalog()?;
    let ids = if all { cat.missing_originals()? } else { ids };
    for id in &ids {
        row_or_fail(&cat, id)?;
    }
    let t = ctx.transport()?;
    let targets = ctx.targets(if medium { None } else { out });
    let job = if medium { Job::Medium } else { Job::Original };

    // The app's download pool: a few threads, each with its own catalog
    // connection; an id named twice is fetched once.
    let mut unique: Vec<&str> = Vec::new();
    let mut seen = HashSet::new();
    for id in &ids {
        if seen.insert(id.as_str()) {
            unique.push(id);
        }
    }
    let (tx, rx) = std::sync::mpsc::channel::<thumbs::Event>();
    let pool = (!unique.is_empty()).then(|| {
        Downloader::start(
            t.clone(),
            targets.dirs.clone(),
            targets.library.clone(),
            DOWNLOAD_WORKERS.min(unique.len()),
            Box::new(move |e| {
                let _ = tx.send(e);
            }),
        )
    });
    for id in &unique {
        if let Some(p) = &pool {
            p.enqueue(id, job, Priority::Background);
        }
    }

    let mut done: HashMap<String, Res<Value>> = HashMap::new();
    let mut failed = 0;
    while done.len() < unique.len() {
        let e = rx.recv().map_err(|_| other("the download threads stopped"))?;
        let id = e.id.clone();
        let result: Res<Value> = match e.result {
            // A lapsed sign-in fails every other item too: stop.
            Err(err) if err.is_sign_in() => {
                if let Some(p) = &pool {
                    p.clear_background();
                }
                return Err(Fail::Err(err));
            }
            Err(err) => Err(Fail::Err(err)),
            Ok(cached) if medium => (|| {
                let path = match out {
                    Some(dir) => copy_to(&cached, &dir.join(format!("{}.jpg", sanitize(&id))))?,
                    None => cached,
                };
                Ok(json!({ "id": id, "path": path }))
            })(),
            Ok(path) => row_or_fail(&cat, &id).map(|row| {
                json!({
                    "id": id,
                    "path": path,
                    "live_path": existing(&row.live_path),
                    "live_error": e.live_error.map(|e| e.to_string()),
                })
            }),
        };
        let result = match result {
            Err(f @ Fail::Cli(_)) => return Err(f),
            r => r,
        };
        // Text mode: lines as each finishes; the data keeps the order asked for.
        match &result {
            Ok(v) => {
                if !v["live_error"].is_null() {
                    failed += 1;
                }
                if ctx.text() {
                    say(v["path"].as_str().unwrap_or_default());
                    if let Some(p) = v["live_path"].as_str() {
                        say(p);
                    }
                    if let Some(err) = v["live_error"].as_str() {
                        warn(&format!("error: {id}: Live Photo video: {err}"));
                    }
                }
            }
            Err(Fail::Err(err)) => {
                failed += 1;
                if ctx.text() {
                    warn(&format!("error: {id}: {err}"));
                }
            }
            Err(Fail::Cli(_)) => {}
        }
        done.insert(id, result);
    }
    let code = if failed > 0 { EXIT_ERROR } else { 0 };
    if ctx.text() && !unique.is_empty() {
        // Every line is already out.
        return Ok(Response::silent().exit(code));
    }
    let results: Vec<Value> = ids
        .iter()
        .map(|id| match done.get(id) {
            Some(Ok(v)) => v.clone(),
            Some(Err(Fail::Err(e))) => json!({ "id": id, "error": e.to_string() }),
            _ => json!({ "id": id, "error": "not downloaded" }),
        })
        .collect();
    let ok: Vec<String> = unique
        .iter()
        .filter(|id| matches!(done.get(**id), Some(Ok(v)) if v["live_error"].is_null()))
        .map(|id| id.to_string())
        .collect();
    let summary = match (unique.len(), failed) {
        (0, _) => "Nothing to download".to_owned(),
        (n, 0) => format!("Downloaded {}", plural(n, "item", "items")),
        (n, f) => format!("Downloaded {} of {}; {f} failed", n - f, plural(n, "item", "items")),
    };
    Ok(Response::new(results, summary).ids(ok).exit(code))
}

// ---- upload -------------------------------------------------------------

fn upload_cmd(ctx: &Ctx, files: &[PathBuf], album: Option<&str>, no_sync: bool) -> Res<Response> {
    let (paths, skipped): (Vec<PathBuf>, Vec<PathBuf>) = files
        .iter()
        .cloned()
        .partition(|p| p.is_file() && upload::is_supported(p));
    if let Some(a) = album {
        check_album(&ctx.catalog()?, a)?;
    }
    let mut results: Vec<Value> = skipped
        .iter()
        .map(|p| {
            let why = if p.is_file() {
                "not a photo or video iCloud Photos takes"
            } else {
                "no such file"
            };
            if ctx.text() {
                warn(&format!("error: skipping {}: {why}", p.display()));
            }
            json!({ "file": p, "error": why })
        })
        .collect();
    let mut failed = skipped.len();
    let mut uploaded = 0;
    let mut duplicates = 0;
    let mut sign_in_lapsed = false;
    let mut new_ids: Vec<String> = Vec::new();
    let mut album_error = None;
    let mut sync_report = None;
    let mut missing: Vec<String> = Vec::new();

    if !paths.is_empty() {
        let t = ctx.transport()?;
        let done: RefCell<Vec<(usize, String, icloud_photos::transport::Result<upload::Uploaded>)>> =
            RefCell::new(Vec::new());
        // Progress goes to stderr in every mode: it is not the result.
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
                warn(&format!("[{}/{total}] {text}", (index + 1).min(total)));
            }
            BatchEvent::FileDone { index, name, result } => done.borrow_mut().push((index, name, result)),
        });
        failed += summary.failed;
        uploaded = summary.uploaded;
        duplicates = summary.duplicates;

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
                    if ctx.text() {
                        warn(&format!(
                            "error: {}: {e}",
                            file.map_or_else(|| "upload".into(), |p| p.display().to_string())
                        ));
                    }
                    results.push(json!({ "file": file, "error": e.to_string() }));
                }
            }
        }
        if sign_in_lapsed && new_ids.is_empty() {
            return Err(Fail::Err(Error::SignInRequired));
        }

        if let (Some(album), false) = (album, new_ids.is_empty()) {
            let ids: Vec<&str> = new_ids.iter().map(String::as_str).collect();
            if let Err(e) = CloudKit::connect(&*t).and_then(|ck| ck.add_to_album(album, &ids)) {
                if ctx.text() {
                    warn(&format!("error: adding to album {album}: {e}"));
                }
                album_error = Some(e.to_string());
                failed += 1;
            }
        }

        // CloudKit can take ~15-20 s to make new assets queryable: sync until
        // they are in the catalog, for up to a minute.
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
            if !missing.is_empty() && ctx.text() {
                warn(&format!(
                    "warning: not in the catalog yet (a later sync will bring them): {}",
                    missing.join(", ")
                ));
            }
        }
    }

    let v = json!({
        "files": results,
        "uploaded": uploaded,
        "duplicates": duplicates,
        "failed": failed,
        "album": album,
        "album_error": album_error,
        "sync": sync_report.as_ref().map(report_json),
        "not_in_catalog": missing,
    });
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
    let mut summary = format!("Uploaded {}", plural(uploaded, "file", "files"));
    if duplicates > 0 {
        summary.push_str(&format!(", {duplicates} already in iCloud"));
    }
    if failed > 0 {
        summary.push_str(&format!(", {failed} failed"));
    }
    let code = if sign_in_lapsed {
        EXIT_SIGN_IN
    } else if failed > 0 {
        EXIT_ERROR
    } else {
        0
    };
    // With nothing uploaded the text is the summary (the failures are on stderr).
    Ok(Response::new(v, summary)
        .human(lines.join("\n"))
        .ids(new_ids)
        .exit(code))
}

// ---- delete, prune ------------------------------------------------------

fn delete(ctx: &Ctx, ids: &[String], yes: bool) -> Res<Response> {
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
        // Ask only when someone can answer: text mode, stdin and stdout a terminal.
        if !output::interactive(ctx.mode) {
            return Err(usage("delete asks before it acts, and nobody can be asked here")
                .hint("pass --yes to delete without asking")
                .into());
        }
        for r in &rows {
            warn(&format!("  {}\t{}", r.id, r.filename));
        }
        if !cli::confirm(&format!(
            "Move {} item(s) to Recently Deleted in iCloud, on all your devices? \
             They can be recovered there for about 30 days.",
            rows.len()
        )) {
            return Err(cancelled("nothing deleted").into());
        }
    }
    let t = ctx.transport()?;
    let ck = CloudKit::connect(&*t)?;
    // Up to 200 deletions per request, each answered on its own.
    let first: Vec<(&str, Option<&str>)> = rows.iter().map(|r| (r.id.as_str(), r.change_tag.as_deref())).collect();
    let mut outcome: Vec<icloud_photos::transport::Result<Modified>> = ck.delete_assets(&first);
    let code_of = |r: &icloud_photos::transport::Result<Modified>| match r {
        Err(Error::CloudKit { code, .. }) => Some(code.clone()),
        _ => None,
    };
    // Changed on another device since the last sync (as the app does: sync,
    // then try again with the fresh change tag). ATOMIC_ERROR: failed only
    // because another record did; send it again as it was.
    let conflicts = outcome.iter().any(|r| code_of(r).as_deref() == Some("CONFLICT"));
    let mut retry: Vec<(usize, String, Option<String>)> = Vec::new();
    if conflicts {
        sync::sync(&ck, &mut cat, &|_| {})?;
    }
    for (i, r) in outcome.iter_mut().enumerate() {
        match code_of(r).as_deref() {
            Some("CONFLICT") => {
                let fresh = row_or_fail(&cat, &rows[i].id)?;
                if fresh.deleted {
                    *r = Err(Error::Other("already deleted on another device".into()));
                } else {
                    retry.push((i, fresh.id, fresh.change_tag));
                }
            }
            Some("ATOMIC_ERROR") => retry.push((i, rows[i].id.clone(), rows[i].change_tag.clone())),
            _ => {}
        }
    }
    if !retry.is_empty() {
        let again: Vec<(&str, Option<&str>)> = retry.iter().map(|(_, id, tag)| (id.as_str(), tag.as_deref())).collect();
        for ((i, ..), r) in retry.iter().zip(ck.delete_assets(&again)) {
            outcome[*i] = r;
        }
    }
    cat.transaction(|cat| {
        for (row, r) in rows.iter().zip(&outcome) {
            if let Ok(m) = r {
                cat.mark_deleted(&row.id, m.change_tag.as_deref())?;
            }
        }
        Ok(())
    })?;
    let mut results = Vec::new();
    let mut deleted = Vec::new();
    let mut failed = 0;
    for (row, result) in rows.iter().zip(outcome) {
        match result {
            Ok(_) => {
                if ctx.text() {
                    say(&format!("{}\tmoved to Recently Deleted", row.id));
                }
                deleted.push(row.id.clone());
                results.push(json!({ "id": row.id, "deleted": true }));
            }
            Err(e) if e.is_sign_in() => return Err(e.into()),
            Err(e) => {
                failed += 1;
                if ctx.text() {
                    warn(&format!("error: {}: {e}", row.id));
                }
                results.push(json!({ "id": row.id, "deleted": false, "error": e.to_string() }));
            }
        }
    }
    let code = if failed > 0 { EXIT_ERROR } else { 0 };
    if ctx.text() {
        // Every line is already out.
        return Ok(Response::silent().exit(code));
    }
    let mut summary = format!("Moved {} to Recently Deleted", plural(deleted.len(), "item", "items"));
    if failed > 0 {
        summary.push_str(&format!("; {failed} failed"));
    }
    Ok(Response::new(results, summary).ids(deleted).exit(code))
}

fn prune(ctx: &Ctx) -> Res<Response> {
    let cat = ctx.catalog()?;
    let removed = thumbs::prune_cache(&cat, &ctx.dirs, thumbs::MEDIUM_CACHE_CAP)?;
    Ok(Response::new(
        json!({ "removed": removed }),
        format!("Removed {removed} cached file(s)"),
    )
    .count(removed))
}

// ---- dates --------------------------------------------------------------

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
    Ok(days_from_civil(y, m as u32, d as u32) * 86_400 + secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        assert_eq!(parse_date("1970-01-01"), Ok(0));
        assert_eq!(parse_date("2024-02-29T12:30:05Z"), Ok(1_709_209_805));
        assert_eq!(rfc3339(1_709_209_805), "2024-02-29T12:30:05Z");
        assert_eq!(parse_date("1709209805"), Ok(1_709_209_805));
        assert_eq!(rfc3339(-1), "1969-12-31T23:59:59Z");
        assert!(parse_date("2024-13-01").is_err());
        assert!(parse_date("yesterday").is_err());
    }

    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}

#[cfg(test)]
mod exit_tests {
    use super::*;

    fn exit_and_code(e: Error) -> (u8, String, bool) {
        let e = classify(Fail::Err(e));
        (e.exit, e.code, e.hint.is_some())
    }

    #[test]
    fn failures_follow_the_shared_exit_mapping() {
        let s = || "x".to_owned();
        let http = |status| Error::Http { status, body: s() };
        let t = |exit, code: &str, hint| (exit, code.to_owned(), hint);
        assert_eq!(exit_and_code(Error::Offline(s())), t(5, "offline", true));
        assert_eq!(exit_and_code(Error::Network(s())), t(5, "network", true));
        assert_eq!(exit_and_code(Error::BadResponse(s())), t(5, "bad_response", true));
        assert_eq!(exit_and_code(Error::Service(s())), t(5, "session_service", true));
        assert_eq!(
            exit_and_code(Error::KeyringUnavailable(s())),
            t(5, "keyring_unavailable", true)
        );
        assert_eq!(exit_and_code(http(503)), t(5, "http", true));
        assert_eq!(exit_and_code(http(421)), t(3, "session_rejected", true));
        assert_eq!(exit_and_code(http(401)), t(3, "session_rejected", true));
        assert_eq!(exit_and_code(http(404)), t(1, "http", false));
        assert_eq!(
            exit_and_code(Error::NotOffered("ckdatabasews".into())),
            t(3, "photos_unavailable", true)
        );
        let rejected = classify(Fail::Err(http(421)));
        assert_eq!(rejected.hint.as_deref(), Some(sign_in_hint().as_str()));
        assert!(rejected.message.contains("HTTP 421"), "{}", rejected.message);
        assert_eq!(exit_and_code(Error::SignInRequired), t(3, "sign_in_required", true));
        assert_eq!(exit_and_code(Error::Other(s())), t(1, "error", false));
        assert_eq!(exit_and_code(Error::Io(std::io::Error::other("x"))), t(1, "io", false));
        assert_eq!(classify(not_found(s()).into()).exit, 4);
        assert_eq!(classify(usage(s()).into()).exit, 2);
        assert_eq!(classify(cancelled(s()).into()).exit, 1);
    }
}
