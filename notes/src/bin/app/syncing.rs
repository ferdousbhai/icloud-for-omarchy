//! Syncing from the window, as Notes does: changes reach iCloud on their
//! own, and changes made elsewhere come in, through one step, the engine's
//! `background` (the same decision the timer makes while the window is
//! closed). It syncs when local changes wait, once none changed for a
//! while, and when the last sync is `pull_every` old. iCloud does not tell a
//! web client that something changed, so a phone's edit is fetched by the
//! step: every minute while the window is active, every 15 minutes while it
//! is not, and slower after each failed pull. The engine runs on a worker
//! thread; what it did comes back to the main loop.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::{glib, prelude::*};
use icloud_notes::backend::Event;
use icloud_notes::lock::Taken;
use icloud_notes::sync::{Kind, Run, Started};
use icloud_notes::{session, syncmodel};

use crate::dialogs;
use crate::window::Ui;

/// How often the window asks the engine whether a sync is due.
const TICK: Duration = Duration::from_secs(60);
/// A sync that could not start (a dialog open, unsaved edits) is asked
/// about again after this long, as is a change seen here.
const SOON: Duration = Duration::from_secs(12);
/// How often a run waiting for another process's lock looks again.
const LOCK_RETRY: Duration = Duration::from_millis(500);
/// The longest the step waits between pulls.
const SLOWEST_SECS: u32 = 15 * 60;

const PAUSED: &str = "Sync paused. Sign in to iCloud to resume.";
const OFFLINE: &str = "Offline. Notes syncs again once iCloud can be reached.";

impl Ui {
    /// The first sync, once icloud-session has said whether anyone is
    /// signed in (without it, sync anyway: the sign-in is unknown, not
    /// missing). Without a vault, an account already signed in on this
    /// computer is cloned quietly; only one with no sign-in sees the
    /// onboarding dialog.
    pub fn start_syncing(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        omui::background(session::read, move |status| {
            let Some(ui) = weak.upgrade() else { return };
            ui.backend.borrow_mut().adopt_sign_in(status.ok().flatten());
            ui.show_banners();
            ui.first_sync();
            ui.watch_sign_in();
        });
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(TICK, move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            ui.sync_if_due(ui.pull_every());
            glib::ControlFlow::Continue
        });
        // Changes from other devices come in the moment the window comes to
        // the front, once the last sync is a minute old.
        let weak = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |window| {
            if let Some(ui) = weak.upgrade()
                && window.is_active()
            {
                ui.refresh_sign_in();
                ui.sync_if_due(60);
            }
        });
    }

    fn first_sync(self: &Rc<Self>) {
        if self.started.replace(true) {
            return;
        }
        let (left, cloned, engine, signed_in, expired, apple_id) = {
            let b = self.backend.borrow();
            (
                b.vault().left_icloud(),
                b.vault().cloned(),
                b.engine().is_some(),
                b.signed_in(),
                b.auth_expired(),
                b.apple_id().to_string(),
            )
        };
        if left {
            // Omacloud syncs the folder: no sync, no clone, no sign-in.
        } else if cloned {
            if engine && !expired {
                self.sync_if_due(60); // edits made while the window was closed go up first
            }
        } else if engine && signed_in {
            self.set_notice(&format!("Downloading your notes as {apple_id}…"));
            self.run(Kind::Clone);
        } else {
            dialogs::onboarding(self);
        }
    }

    /// Hears icloud-session's sign-in change, on a thread of its own.
    fn watch_sign_in(self: &Rc<Self>) {
        let (sender, changes) = async_channel::unbounded();
        std::thread::spawn(move || icloud_session::watch_forever(|status| sender.send_blocking(status).is_ok()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(status) = changes.recv().await {
                let Some(ui) = weak.upgrade() else { break };
                ui.sign_in_changed(Some(status));
            }
        });
    }

    /// Reads the sign-in again (the days left move on), off the main loop.
    pub fn refresh_sign_in(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        omui::background(session::read, move |status| {
            if let (Some(ui), Ok(Some(status))) = (weak.upgrade(), status) {
                ui.sign_in_changed(Some(status));
            }
        });
    }

    fn sign_in_changed(self: &Rc<Self>, status: Option<icloud_session::Status>) {
        let was_expired = self.backend.borrow().auth_expired();
        self.backend.borrow_mut().adopt_sign_in(status);
        self.signing_in.set(false);
        self.show_banners();
        if self.clone_wanted.get() && self.backend.borrow().signed_in() {
            self.clone_wanted.set(false);
            self.run(Kind::Clone);
        } else if !self.started.get() {
            self.first_sync();
        } else if was_expired && !self.backend.borrow().auth_expired() {
            self.set_status("Signed in.");
            self.sync_if_due(60);
        }
    }

    /// How old the last sync may be before the step pulls: a minute while
    /// the window is active, doubling after each failed pull, and 15 minutes
    /// while it is not.
    fn pull_every(&self) -> u32 {
        if !self.window.is_active() {
            return SLOWEST_SECS;
        }
        (60 * 2u32.pow(self.poll_failures.get())).min(SLOWEST_SECS)
    }

    /// What every sync that starts on its own waits for: a running sync,
    /// unsaved edits, an open dialog.
    fn sync_blocked(&self) -> bool {
        self.backend.borrow().sync_running() || self.editor.dirty() || dialogs::open(self)
    }

    /// The engine's step, when nothing blocks it; else again soon.
    pub fn sync_if_due(self: &Rc<Self>, every: u32) {
        {
            let b = self.backend.borrow();
            if !b.vault().cloned() || b.auth_expired() {
                return;
            }
        }
        if self.sync_blocked() {
            self.sync_soon();
            return;
        }
        self.run(Kind::Background { every_secs: every });
    }

    /// Asks about a sync once a change seen here has settled.
    pub fn sync_soon(self: &Rc<Self>) {
        if let Some(source) = self.soon.take() {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(SOON, move || {
            if let Some(ui) = weak.upgrade() {
                ui.soon.take();
                ui.sync_if_due(ui.pull_every());
            }
        });
        self.soon.replace(Some(source));
    }

    /// Edits the latest copy: opening a note, focusing the editor or typing
    /// into an untouched note pulls first when the last pull is over a
    /// minute old. The editor waits, read-only, until the pull is in.
    pub fn pull_if_stale(self: &Rc<Self>) -> bool {
        {
            let b = self.backend.borrow();
            if !b.vault().cloned() || b.auth_expired() {
                return false;
            }
        }
        let fresh = self
            .last_sync
            .get()
            .is_some_and(|at| at.elapsed() < Duration::from_secs(60));
        if fresh || self.sync_blocked() {
            return false;
        }
        self.freshening.set(true);
        self.show_note_header();
        self.run(Kind::Background { every_secs: 60 });
        true
    }

    /// A sync asked for (Sync now): both directions, whatever is due.
    pub fn sync_now(self: &Rc<Self>, delete_missing: bool) {
        if self.flush_edits() {
            self.run(Kind::Sync { delete_missing });
        }
    }

    /// Starts `kind`, unless a run is under way. While another process holds
    /// the vault's lock it waits for it, and saves wait with it.
    pub fn run(self: &Rc<Self>, kind: Kind) {
        let started = {
            let mut b = self.backend.borrow_mut();
            if b.sync_running() {
                return;
            }
            b.begin(kind)
        };
        self.started(kind, started);
    }

    fn started(self: &Rc<Self>, kind: Kind, started: Started) {
        self.last_sync.set(Some(Instant::now()));
        match started {
            Started::Nothing => self.freshening.set(false),
            Started::Ended(runs) => self.ran(vec![runs]),
            Started::Waiting => {
                self.waiting_run.set(Some(kind));
                let holder = self.backend.borrow().lock_holder();
                self.set_status(&format!("Waiting for {holder} to finish…"));
                self.spinner.set_visible(true);
                self.show_banners();
                self.show_vault();
                self.wait_for_lock();
            }
            Started::Running(running) => {
                self.run_stopper.replace(Some(running.stopper()));
                self.spinner.set_visible(true);
                self.show_banners(); // their buttons wait for the run
                if !matches!(kind, Kind::Background { .. }) {
                    self.set_status(&format!("{}…", label(kind)));
                }
                self.show_vault();
                let weak = Rc::downgrade(self);
                omui::background(
                    move || running.wait(),
                    move |finished| {
                        let Some(ui) = weak.upgrade() else { return };
                        ui.run_stopper.take();
                        let runs = ui.backend.borrow_mut().finish(finished.unwrap_or_default());
                        ui.reports(runs);
                    },
                );
            }
        }
    }

    /// Another process holds the vault's lock (a background sync, a command
    /// line change): looks again until the window holds it, then starts the
    /// run that waited for it, or reads what that process changed.
    pub fn wait_for_lock(self: &Rc<Self>) {
        if self.lock_retry.get() {
            return;
        }
        self.lock_retry.set(true);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(LOCK_RETRY, move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let run = ui.waiting_run.get();
            let started = {
                let mut b = ui.backend.borrow_mut();
                match run {
                    Some(_) => b.go_on(),
                    None if b.lock_vault() == Taken::Busy => Started::Waiting,
                    None => Started::Nothing,
                }
            };
            match (run, started) {
                (_, Started::Waiting) => return glib::ControlFlow::Continue,
                (Some(kind), started) => {
                    ui.waiting_run.set(None);
                    ui.started(kind, started);
                }
                (None, _) => {
                    ui.backend.borrow_mut().refresh();
                    ui.after_reload();
                }
            }
            ui.lock_retry.set(false);
            glib::ControlFlow::Break
        });
    }

    /// A run's refused sign-ins, told to icloud-session off the main loop;
    /// when it vouches for the sign-in, one more sync.
    fn reports(self: &Rc<Self>, runs: Vec<Run>) {
        let reports = self.backend.borrow_mut().take_reports();
        if reports == 0 {
            self.ran(vec![runs]);
            return;
        }
        let weak = Rc::downgrade(self);
        omui::background(
            move || {
                (0..reports)
                    .map(|_| session::report_sign_in_required())
                    .collect::<Vec<_>>()
            },
            move |answers| {
                let Some(ui) = weak.upgrade() else { return };
                let mut next = None;
                for answer in answers.unwrap_or_default() {
                    next = ui.backend.borrow_mut().reported(answer).or(next);
                }
                ui.ran(vec![runs]);
                if let Some(kind) = next {
                    ui.run(kind);
                }
            },
        );
    }

    /// What a chain of runs did, on screen.
    fn ran(self: &Rc<Self>, chains: Vec<Vec<Run>>) {
        self.spinner.set_visible(false);
        self.freshening.set(false);
        self.last_sync.set(Some(Instant::now()));
        let runs: Vec<Run> = chains.into_iter().flatten().collect();
        let mut message = None;
        for run in &runs {
            let code = run.error.get("code").and_then(|c| c.as_str()).unwrap_or_default();
            let error_message = run.error.get("message").and_then(|m| m.as_str()).unwrap_or_default();
            match run.command {
                "pull" => {
                    let failures = if run.ok {
                        0
                    } else {
                        (self.poll_failures.get() + 1).min(4)
                    };
                    self.poll_failures.set(failures);
                }
                "push" if run.ok => self.deletions_refused.borrow_mut().clear(),
                "push" if code == "too_many_deletions" => {
                    self.deletions_refused.replace(error_message.to_string());
                }
                "clone" => {
                    self.set_notice("");
                    if !run.ok && !self.backend.borrow().vault().cloned() {
                        dialogs::onboarding(self);
                    }
                }
                _ => {}
            }
            let name = match run.command {
                "push" => "Push",
                "pull" => "Pull",
                _ => "Clone",
            };
            message = Some(match code {
                "sign_in_required" | "session_rejected" => PAUSED.to_string(),
                "offline" | "network" => OFFLINE.to_string(),
                "too_many_deletions" => "Nothing pushed: most of the notes' files are gone.".to_string(),
                _ if run.ok => format!("{name} done."),
                "" => format!("{name} failed (exit {}). See log.", run.exit_code),
                _ => format!("{name} failed. See log."),
            });
        }
        // A step that found nothing due leaves the status as it was.
        if let Some(message) = message {
            self.set_status(&message);
        } else if self.status.label().ends_with('…') {
            self.set_status("Ready.");
        }
        self.show_banners();
        self.after_reload();
    }

    /// The vault as a run (or another program) left it: the lists, and the
    /// open note, whose unsaved edits are merged with what changed under
    /// them rather than written over it.
    pub fn after_reload(self: &Rc<Self>) {
        self.handle_events();
        self.refresh_search();
        let on_disk = syncmodel::editor_form(self.backend.borrow().note_body());
        if on_disk != self.editor.saved() {
            if !self.editor.dirty() {
                self.load_editor();
            } else if !self.backend.borrow().sync_running() {
                self.merge_edits(&self.editor.saved());
            }
        }
        self.show_vault();
    }

    /// The open note changed under unsaved edits (a pull, another program):
    /// edits to different lines land in the note as it is, and only lines
    /// both sides changed open as versions to pick from. A note that went
    /// away keeps the edits as a new note.
    pub fn merge_edits(self: &Rc<Self>, base: &str) {
        let mine = self.editor.text();
        let merged = self.backend.borrow_mut().keep_edits_as_conflict(base, &mine);
        let mut kept = String::new();
        for event in self.backend.borrow_mut().take_events() {
            match event {
                Event::EditsKeptAsNote(message) => kept = message,
                Event::VaultChanged => self.sync_soon(),
                _ => {}
            }
        }
        if merged {
            self.load_editor();
            if !kept.is_empty() {
                self.set_notice(&kept);
            } else if self.backend.borrow().note_conflicts().is_empty() {
                self.set_notice("Merged your edits with a change from another device.");
            }
        }
    }
}

/// What the status line calls a run while it goes.
fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::Clone => "Clone",
        Kind::Pull => "Pull",
        Kind::Push { .. } => "Push",
        Kind::Sync { .. } | Kind::Background { .. } => "Sync",
    }
}
