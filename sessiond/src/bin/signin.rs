//! `icloud-session-signin`: Apple's own sign-in page in a GTK4 + WebKitGTK 6
//! window. When the page has signed in, prints one JSON object to stdout
//! and exits 0; closing the window exits 1. icloud-sessiond runs it for
//! `SignIn()`; run it by hand to try a sign-in (it stores nothing but its
//! WebKit profile).
//!
//! ```json
//! {"cookies":[{"name":"X-APPLE-WEBAUTH-TOKEN","value":"…","domain":".icloud.com","path":"/","expires":1790000000}],
//!  "clientId":"…","clientBuildNumber":"…","clientMasteringNumber":"…"}
//! ```
//!
//! Signed-in detection mirrors icloud-md's Playwright login
//! (`auth/browserLogin.js`): the page's own `setup.icloud.com/setup/ws/1/
//! accountLogin` or `/validate` answers 2xx with `dsInfo` and no pending
//! `hsaChallengeRequired`. The client params come from that request's
//! query; the cookies from WebKit's cookie manager a second later.
//!
//! Environment:
//! - `ICLOUD_SESSION_SIGNIN_UA`: `safari` for a macOS Safari user agent,
//!   any other value is used verbatim; unset keeps WebKitGTK's default.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};
use webkit6::prelude::*;
use webkit6::{gio, glib, gtk, soup};

const HOME: &str = "https://www.icloud.com/";
/// Find My's page: where Apple asks for the password again before Find My
/// answers (HTTP 450 until then), as icloud.com does for Find Devices.
const FIND: &str = "https://www.icloud.com/find";
const VALIDATE: &str = "https://setup.icloud.com/setup/ws/1/validate";
/// icloud-md's defaults, which icloud-sessiond also falls back to.
const CLIENT_BUILD_NUMBER: &str = "2624Build27";
const CLIENT_MASTERING_NUMBER: &str = "2624Build27";
const TOKEN: &str = "X-APPLE-WEBAUTH-TOKEN";
/// How often the jar is checked for a sign-in.
const POLL: Duration = Duration::from_secs(2);
/// How often Find My is asked while waiting for its password (`--find`).
const FIND_POLL: Duration = Duration::from_secs(5);
const SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15";

/// Ticks "Keep me signed in" in Apple's sign-in frame, so the session is
/// the long (extended) one and a later sign-in can skip 2FA, as icloud-md's
/// `keepMeSignedInWatcher` does. Best effort.
const KEEP_SIGNED_IN_JS: &str = r#"
(() => {
  const tick = () => {
    for (const label of document.querySelectorAll('label')) {
      if (!/keep me signed in/i.test(label.textContent || '')) continue;
      const box = label.control || label.querySelector('input[type=checkbox]') ||
        (label.htmlFor && document.getElementById(label.htmlFor));
      if (box && box.type === 'checkbox' && !box.checked) box.click();
    }
  };
  setInterval(tick, 1000);
})();
"#;

/// The script message handler the page scripts below report to.
const TRACE_HANDLER: &str = "icloudSessionTrace";

/// `ICLOUD_SESSION_SIGNIN_TRACE=1`: reports each frame's form (field ids,
/// types and names, button ids and labels; never values) and the page's
/// own requests (method, host and path; no query), to see Apple's sign-in
/// without screen access.
const TRACE_JS: &str = r##"
(() => {
  const post = (m) => { try { window.webkit.messageHandlers.icloudSessionTrace.postMessage(location.host + location.pathname + ": " + m); } catch (_) {} };
  const where = (u) => { try { const x = new URL(u, location.href); return x.host + x.pathname; } catch (_) { return "?"; } };
  const f = window.fetch;
  if (f) window.fetch = async (...a) => {
    const url = typeof a[0] === "string" ? a[0] : (a[0] && a[0].url) || "";
    const r = await f(...a);
    post("fetch " + ((a[1] && a[1].method) || "GET") + " " + where(url) + " -> " + r.status);
    return r;
  };
  const open = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (m, u, ...rest) {
    this.addEventListener("loadend", () => post("xhr " + m + " " + where(u) + " -> " + this.status));
    return open.call(this, m, u, ...rest);
  };
  let last = "";
  setInterval(() => {
    const shown = (e) => e.offsetParent !== null;
    const inputs = [...document.querySelectorAll("input")].filter(shown)
      .map((e) => `${e.id || "-"}/${e.type}/${e.name || "-"}/${e.autocomplete || "-"}`);
    const buttons = [...document.querySelectorAll("button, [role=button]")].filter(shown)
      .map((e) => `${e.id || "-"}:${(e.textContent || "").trim().slice(0, 24)}`);
    const alerts = [...document.querySelectorAll("[role=alert], [aria-live], .form-message, .error, .si-error-message, h1, h2")]
      .filter(shown).map((e) => (e.textContent || "").trim().replace(/\s+/g, " ").slice(0, 80)).filter(Boolean);
    const filled = [...document.querySelectorAll("input")].filter(shown).map((e) => `${e.id || e.type}=${e.value ? "set" : "empty"}`);
    const now = `inputs [${inputs.join(" ")}] buttons [${buttons.join(" | ")}] text [${alerts.join(" / ")}] values [${filled.join(" ")}]`;
    if (now !== last && (inputs.length || buttons.length)) { last = now; post(now); }
  }, 1000);
})();
"##;

/// Fills Apple's sign-in form, as a password manager would: the Apple ID
/// if asked, then the password, submitting each; at most twice, so a
/// refused password can't be retried into a lockout. `__ID__` and `__PW__`
/// are replaced with JSON strings. Reports steps (never values).
const AUTOFILL_JS: &str = r##"
(() => {
  const ID = __ID__, PW = __PW__;
  const post = (m) => { try { window.webkit.messageHandlers.icloudSessionTrace.postMessage("autofill: " + m); } catch (_) {} };
  const shown = (e) => e && e.offsetParent !== null && !e.disabled;
  const set = (el, v) => {
    el.focus();
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(el, v);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  };
  const submit = (el) => {
    for (const sel of ["#sign-in", "#continue", "button[type=submit]"]) {
      const b = document.querySelector(sel);
      if (shown(b)) { b.click(); return sel; }
    }
    const k = { key: "Enter", code: "Enter", keyCode: 13, which: 13, bubbles: true };
    el.dispatchEvent(new KeyboardEvent("keydown", k));
    el.dispatchEvent(new KeyboardEvent("keyup", k));
    return "Enter";
  };
  let accountTries = 0, passwordTries = 0, signInClicks = 0, lastClick = 0;
  setInterval(() => {
    const account = document.querySelector("#account_name_text_field, input[autocomplete=username], input[type=email]");
    const password = document.querySelector("#password_text_field, input[type=password]");
    const said = [...document.querySelectorAll("[role=alert], [aria-live], .form-message, .si-error-message")]
      .map((e) => e.textContent || "").join(" ");
    if (passwordTries > 0 && /incorrect|not correct|wrong|locked/i.test(said)) {
      post("password rejected");
      return;
    }
    if (shown(password) && !password.value && passwordTries < 2) {
      passwordTries++;
      if (shown(account) && !account.value) set(account, ID);
      set(password, PW);
      post("password filled, submitted with " + submit(password));
    } else if (shown(password) && password.value && signInClicks < 2 && Date.now() - lastClick > 3000) {
      // The password step kept the value filled before "Continue": press
      // its "Sign In" (the Continue click above already counted once).
      const button = document.querySelector("#sign-in");
      if (shown(button) && /sign in/i.test(button.textContent || "")) {
        signInClicks++;
        lastClick = Date.now();
        button.click();
        post("pressed Sign In");
      }
    } else if (shown(account) && !account.value && !shown(password) && accountTries < 2) {
      accountTries++;
      set(account, ID);
      post("Apple ID filled, submitted with " + submit(account));
    }
  }, 700);
})();
"##;

/// With `--autofill`, on www.icloud.com: clicks the landing page's
/// "Sign In", which is what loads Apple's sign-in frame. At most twice.
const OPEN_SIGN_IN_JS: &str = r##"
(() => {
  const post = (m) => { try { window.webkit.messageHandlers.icloudSessionTrace.postMessage("autofill: " + m); } catch (_) {} };
  let clicks = 0;
  const timer = setInterval(() => {
    if (clicks >= 2 || document.querySelector("iframe[src*='idmsa.apple.com']")) { clearInterval(timer); return; }
    const button = [...document.querySelectorAll("button, [role=button]")]
      .find((b) => b.offsetParent !== null && /^sign in$/i.test((b.textContent || "").trim()));
    if (button) { clicks++; button.click(); post("clicked Sign In"); }
  }, 1000);
})();
"##;

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
            home.join(fallback)
        })
        .join("icloud-session")
        .join("webkit")
}

/// A completed sign-in: account info present and no 2FA still pending
/// (icloud-md's `isFullySignedInBody`).
fn is_fully_signed_in(body: &Value) -> bool {
    let challenge = |v: &Value| v.get("hsaChallengeRequired") == Some(&Value::Bool(true));
    body.get("dsInfo").is_some_and(Value::is_object) && !challenge(body) && !challenge(&body["dsInfo"])
}

/// The client params this window identifies as, to Apple and then to the daemon.
fn client_params(client_id: &str) -> Value {
    json!({
        "clientId": client_id,
        "clientBuildNumber": CLIENT_BUILD_NUMBER,
        "clientMasteringNumber": CLIENT_MASTERING_NUMBER,
    })
}

/// A /validate call made from the page itself, so it carries the browser's
/// own cookies (and rotates the token in the jar, as the page's heartbeat
/// does). Answers `{"status": n, "body": "..."}` as a JSON string.
fn validate_js(client_id: &str) -> String {
    let mut url = url::Url::parse(VALIDATE).expect("VALIDATE is a URL");
    url.query_pairs_mut()
        .append_pair("clientBuildNumber", CLIENT_BUILD_NUMBER)
        .append_pair("clientMasteringNumber", CLIENT_MASTERING_NUMBER)
        .append_pair("clientId", client_id);
    let url = Value::String(url.into());
    format!(
        r#"const r = await fetch({url}, {{method: "POST", credentials: "include",
  headers: {{"Content-Type": "text/plain;charset=UTF-8"}}, body: ""}});
return JSON.stringify({{status: r.status, body: r.ok ? await r.text() : ""}});"#
    )
}

/// Like [`validate_js`], then Find My's own first call (initClient) with the
/// same cookies: 450 until the password has been entered for Find My.
/// Answers the validate body only once Find My answers 2xx.
fn find_js(client_id: &str) -> String {
    let query = format!(
        "clientBuildNumber={CLIENT_BUILD_NUMBER}&clientMasteringNumber={CLIENT_MASTERING_NUMBER}&clientId={client_id}"
    );
    let query = Value::String(query);
    let validate = Value::String(VALIDATE.to_owned());
    format!(
        r#"const q = {query};
const post = (url, body) => fetch(url, {{method: "POST", credentials: "include",
  headers: {{"Content-Type": "text/plain;charset=UTF-8"}}, body}});
// Validate once per page (each /validate rotates the session token; many in
// a row alongside another copy of the session get it ended), then only ask
// Find My, which rotates nothing.
let text = globalThis.__icloudSessionAccount;
if (!text) {{
  const v = await post({validate} + "?" + q, "");
  if (!v.ok) return JSON.stringify({{status: v.status, body: ""}});
  text = await v.text();
  globalThis.__icloudSessionAccount = text;
}}
const account = JSON.parse(text);
const findme = account.webservices && account.webservices.findme && account.webservices.findme.url;
if (!findme || !account.dsInfo) return JSON.stringify({{status: 0, body: ""}});
const f = await post(findme + "/fmipservice/client/web/initClient?" + q + "&dsid=" + account.dsInfo.dsid,
  JSON.stringify({{clientContext: {{appName: "iCloud Find (Web)", appVersion: "2.0", apiVersion: "3.0",
    deviceListVersion: 1, fmly: true, timezone: "UTC", inactiveTime: 0}}}}));
return JSON.stringify({{status: f.status, body: f.ok ? text : ""}});"#
    )
}

/// Domains the window loads pages and frames from: Apple's sign-in
/// (idmsa.apple.com, inside www.icloud.com) and what it embeds.
const APPLE_DOMAINS: [&str; 4] = ["apple.com", "icloud.com", "cdn-apple.com", "apple-cloudkit.com"];

/// Whether a navigation (of the page or any frame) stays in the window:
/// https on an Apple domain, or a local `about:`/`data:`/`blob:` document.
fn stays_in_window(uri: &str) -> bool {
    let Ok(url) = url::Url::parse(uri) else {
        return false;
    };
    match url.scheme() {
        "about" | "data" | "blob" => true,
        "https" => url.host_str().is_some_and(|host| {
            let host = host.to_ascii_lowercase();
            APPLE_DOMAINS
                .iter()
                .any(|d| host == *d || host.strip_suffix(d).is_some_and(|rest| rest.ends_with('.')))
        }),
        _ => false,
    }
}

fn is_icloud_domain(domain: &str) -> bool {
    let host = domain.trim_start_matches('.');
    host == "icloud.com" || host.ends_with(".icloud.com")
}

fn cookies_json(cookies: Vec<soup::Cookie>) -> Vec<Value> {
    cookies
        .into_iter()
        .filter_map(|mut c| {
            let domain = c.domain()?.to_string();
            if !is_icloud_domain(&domain) {
                return None;
            }
            let expires = c.expires().map(|t| t.to_unix()).filter(|&t| t > 0);
            Some(json!({
                "name": c.name()?.as_str(),
                "value": c.value().map(|v| v.to_string()).unwrap_or_default(),
                "domain": domain,
                "path": c.path().map_or_else(|| "/".to_string(), |p| p.to_string()),
                "expires": expires,
            }))
        })
        .collect()
}

struct Capture {
    done: Cell<bool>,
    exit: Cell<u8>,
    main_loop: glib::MainLoop,
    cookies: webkit6::CookieManager,
    client_id: String,
    /// A check is running.
    checking: Cell<bool>,
    /// The token value /validate last refused: not tried again until the
    /// jar holds a different one (a fresh sign-in or a rotation).
    refused: RefCell<Option<String>>,
    /// Authorizing Find My (`--find`) rather than signing in: done when
    /// Find My answers, re-checked on every poll since entering the
    /// password need not change the token.
    find: bool,
    /// When Find My was last asked.
    last_find: Cell<Option<std::time::Instant>>,
    /// The dsid the page's /validate named (`--find`), printed with the jar
    /// so the daemon can check it without validating the jar itself.
    dsid: RefCell<Option<String>>,
}

impl Capture {
    /// Waits a moment for the validate response's own Set-Cookies to land
    /// in the jar, then prints the jar and the params and quits.
    fn finish(self: &Rc<Self>) {
        if self.done.replace(true) {
            return;
        }
        eprintln!("icloud-session-signin: signed in, capturing the session");
        let me = self.clone();
        glib::timeout_add_local_once(Duration::from_secs(1), move || {
            let me2 = me.clone();
            me.cookies.all_cookies(None::<&gio::Cancellable>, move |result| {
                match result {
                    Ok(cookies) => {
                        let mut out = client_params(&me2.client_id);
                        out["cookies"] = Value::Array(cookies_json(cookies));
                        if let Some(dsid) = me2.dsid.borrow().as_ref() {
                            out["dsid"] = Value::String(dsid.clone());
                        }
                        println!("{out}");
                        me2.exit.set(0);
                    }
                    Err(e) => eprintln!("icloud-session-signin: reading cookies: {e}"),
                }
                me2.main_loop.quit();
            });
        });
    }

    /// Once the jar holds a session token, asks Apple (from the page, with
    /// the page's cookies) whether it is a complete sign-in: account info
    /// and no 2FA still pending. The page's own calls to Apple can't be
    /// observed from here, so this does not depend on them; it also covers
    /// a profile that is already signed in.
    fn check(self: &Rc<Self>, view: &webkit6::WebView) {
        if self.done.get() {
            return;
        }
        if self.find && self.last_find.get().is_some_and(|t| t.elapsed() < FIND_POLL) {
            return;
        }
        if self.checking.replace(true) {
            return;
        }
        let me = self.clone();
        let view = view.clone();
        self.cookies.all_cookies(None::<&gio::Cancellable>, move |result| {
            let token = result.ok().and_then(|cookies| {
                cookies.into_iter().find_map(|mut c| {
                    let is_token =
                        c.name().is_some_and(|n| n == TOKEN) && c.domain().is_some_and(|d| is_icloud_domain(&d));
                    is_token.then(|| c.value().map(|v| v.to_string()).unwrap_or_default())
                })
            });
            let Some(token) = token.filter(|t| !t.is_empty()) else {
                me.checking.set(false);
                return;
            };
            if !me.find && me.refused.borrow().as_deref() == Some(token.as_str()) {
                me.checking.set(false);
                return;
            }
            if me.find {
                me.last_find.set(Some(std::time::Instant::now()));
            }
            let me2 = me.clone();
            view.call_async_javascript_function(
                &if me.find {
                    find_js(&me.client_id)
                } else {
                    validate_js(&me.client_id)
                },
                None,
                None,
                None,
                None::<&gio::Cancellable>,
                move |result| {
                    let answer: Value = match result {
                        Ok(v) => serde_json::from_str(&v.to_str()).unwrap_or(Value::Null),
                        Err(e) => {
                            eprintln!("icloud-session-signin: checking the sign-in: {e}");
                            Value::Null
                        }
                    };
                    let status = answer["status"].as_u64().unwrap_or(0);
                    let body: Value = answer["body"]
                        .as_str()
                        .and_then(|b| serde_json::from_str(b).ok())
                        .unwrap_or(Value::Null);
                    if (200..300).contains(&status) && is_fully_signed_in(&body) {
                        if me2.find {
                            *me2.dsid.borrow_mut() = match &body["dsInfo"]["dsid"] {
                                Value::String(d) => Some(d.clone()),
                                Value::Number(d) => Some(d.to_string()),
                                _ => None,
                            };
                        }
                        me2.finish();
                    } else {
                        if status != 0 {
                            let what = if me2.find { "Find My" } else { "/validate" };
                            eprintln!("icloud-session-signin: not signed in yet ({what} answered {status})");
                        }
                        *me2.refused.borrow_mut() = Some(token);
                    }
                    me2.checking.set(false);
                },
            );
        });
    }
}

/// The window's Wayland app_id and desktop entry name.
const APP_ID: &str = "io.github.ferdousbhai.ICloudSession";

fn main() -> ExitCode {
    let (mut find, mut autofill) = (false, false);
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--find" => find = true,
            "--autofill" => autofill = true,
            "-V" | "--version" => {
                println!("icloud-session-signin {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!(
                    "usage: icloud-session-signin [--find] [--autofill]   (prints the captured session as JSON)\n\
                     --autofill reads the Apple ID and the password, one per line, from stdin"
                );
                return ExitCode::from(64);
            }
        }
    }
    // Read before any window opens; kept only in the page script below.
    let credentials = if autofill {
        let mut lines = std::io::stdin().lines();
        match (lines.next(), lines.next()) {
            (Some(Ok(id)), Some(Ok(pw))) if !id.is_empty() && !pw.is_empty() => Some((id, pw)),
            _ => {
                eprintln!("icloud-session-signin: --autofill needs the Apple ID and the password on stdin");
                return ExitCode::from(64);
            }
        }
    } else {
        None
    };
    // No GtkApplication here, so the Wayland app_id is the program name: make
    // it ours, matching the desktop entry, not "GTK Application".
    glib::set_prgname(Some(APP_ID));
    glib::set_application_name("Sign in to iCloud");
    if let Err(e) = gtk::init() {
        eprintln!("icloud-session-signin: cannot open a window: {e}");
        return ExitCode::FAILURE;
    }

    let data_dir = xdg_dir("XDG_DATA_HOME", ".local/share");
    let cache_dir = xdg_dir("XDG_CACHE_HOME", ".cache");
    for dir in [&data_dir, &cache_dir] {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
    }
    let session = webkit6::NetworkSession::new(data_dir.to_str(), cache_dir.to_str());
    let Some(cookies) = session.cookie_manager() else {
        eprintln!("icloud-session-signin: WebKit has no cookie manager");
        return ExitCode::FAILURE;
    };
    cookies.set_persistent_storage(
        data_dir.join("cookies.sqlite").to_str().unwrap_or_default(),
        webkit6::CookiePersistentStorage::Sqlite,
    );
    // Apple's sign-in form is an idmsa.apple.com frame inside
    // www.icloud.com, so its cookies are third-party ones, which WebKitGTK 6
    // refuses by default. The window only ever loads Apple (see below).
    cookies.set_accept_policy(webkit6::CookieAcceptPolicy::Always);

    let content = webkit6::UserContentManager::new();
    content.register_script_message_handler(TRACE_HANDLER, None);
    // Set once the Capture exists: Apple saying the password is wrong ends
    // the window with exit code 3, so the daemon stops trying it.
    let rejected: Rc<RefCell<Option<Rc<Capture>>>> = Rc::new(RefCell::new(None));
    {
        let rejected = rejected.clone();
        content.connect_script_message_received(Some(TRACE_HANDLER), move |_, value| {
            let text = value.to_str();
            eprintln!("icloud-session-signin: {text}");
            if text.as_str() == "autofill: password rejected"
                && let Some(capture) = rejected.borrow().as_ref()
                && !capture.done.replace(true)
            {
                capture.exit.set(3);
                capture.main_loop.quit();
            }
        });
    }
    let apple_frames = ["https://*.apple.com/*", "https://*.icloud.com/*"];
    if std::env::var("ICLOUD_SESSION_SIGNIN_TRACE").is_ok_and(|v| !v.is_empty() && v != "0") {
        content.add_script(&webkit6::UserScript::new(
            TRACE_JS,
            webkit6::UserContentInjectedFrames::AllFrames,
            webkit6::UserScriptInjectionTime::Start,
            &apple_frames,
            &[],
        ));
    }
    if let Some((id, pw)) = &credentials {
        let script = AUTOFILL_JS
            .replace("__ID__", &Value::String(id.clone()).to_string())
            .replace("__PW__", &Value::String(pw.clone()).to_string());
        content.add_script(&webkit6::UserScript::new(
            &script,
            webkit6::UserContentInjectedFrames::AllFrames,
            webkit6::UserScriptInjectionTime::End,
            &["https://idmsa.apple.com/*"],
            &[],
        ));
        content.add_script(&webkit6::UserScript::new(
            OPEN_SIGN_IN_JS,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::End,
            &["https://www.icloud.com/*"],
            &[],
        ));
    }
    content.add_script(&webkit6::UserScript::new(
        KEEP_SIGNED_IN_JS,
        webkit6::UserContentInjectedFrames::AllFrames,
        webkit6::UserScriptInjectionTime::End,
        &["https://idmsa.apple.com/*"],
        &[],
    ));
    let view = webkit6::WebView::builder()
        .network_session(&session)
        .user_content_manager(&content)
        .build();
    if let Some(settings) = WebViewExt::settings(&view) {
        match std::env::var("ICLOUD_SESSION_SIGNIN_UA").ok().filter(|v| !v.is_empty()) {
            Some(ua) if ua == "safari" => settings.set_user_agent(Some(SAFARI_UA)),
            Some(ua) => settings.set_user_agent(Some(&ua)),
            None => {}
        }
    }

    let capture = Rc::new(Capture {
        done: Cell::new(false),
        exit: Cell::new(1),
        main_loop: glib::MainLoop::new(None, false),
        cookies,
        client_id: uuid::Uuid::new_v4().to_string(),
        checking: Cell::new(false),
        refused: RefCell::new(None),
        find,
        last_find: Cell::new(None),
        dsid: RefCell::new(None),
    });
    *rejected.borrow_mut() = Some(capture.clone());

    // Signed in yet? Checked on a timer and after each page load.
    {
        let capture = capture.clone();
        let view = view.clone();
        glib::timeout_add_local(POLL, move || {
            capture.check(&view);
            glib::ControlFlow::Continue
        });
    }
    // Keep the window on Apple: a link elsewhere the user clicked opens in
    // their browser, anything else off Apple is not loaded.
    view.connect_decide_policy(|_, decision, kind| {
        if !matches!(
            kind,
            webkit6::PolicyDecisionType::NavigationAction | webkit6::PolicyDecisionType::NewWindowAction
        ) {
            return false;
        }
        let Some(action) = decision
            .downcast_ref::<webkit6::NavigationPolicyDecision>()
            .and_then(|d| d.navigation_action())
        else {
            return false;
        };
        let uri = action
            .request()
            .and_then(|r| r.uri())
            .map(|u| u.to_string())
            .unwrap_or_default();
        if stays_in_window(&uri) {
            return false;
        }
        if action.is_user_gesture() && (uri.starts_with("https://") || uri.starts_with("http://")) {
            eprintln!("icloud-session-signin: opening {uri} in the browser");
            if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
                eprintln!("icloud-session-signin: opening {uri}: {e}");
            }
        } else {
            eprintln!("icloud-session-signin: not loading {uri} (not Apple)");
        }
        decision.ignore();
        true
    });
    view.connect_web_process_terminated(|_, reason| {
        eprintln!("icloud-session-signin: the web process ended ({reason:?})");
    });
    {
        let capture = capture.clone();
        view.connect_load_changed(move |view, event| {
            if event == webkit6::LoadEvent::Finished {
                eprintln!("icloud-session-signin: loaded {}", view.uri().unwrap_or_default());
                capture.check(view);
            }
        });
    }
    view.connect_load_failed(|_, _, uri, error| {
        eprintln!("icloud-session-signin: loading {uri} failed: {error}");
        false
    });

    let window = gtk::Window::builder()
        .title("Sign in to iCloud")
        .default_width(1000)
        .default_height(800)
        .child(&view)
        .build();
    {
        let capture = capture.clone();
        window.connect_close_request(move |_| {
            if !capture.done.get() {
                eprintln!("icloud-session-signin: window closed before sign-in completed");
                capture.main_loop.quit();
            }
            glib::Propagation::Proceed
        });
    }
    view.load_uri(if find { FIND } else { HOME });
    if autofill {
        // Filled in unseen; shown only if Apple wants more than the password
        // (a 2FA code, a new-terms page) and it hasn't finished by then.
        let window = window.clone();
        let capture = capture.clone();
        glib::timeout_add_local_once(Duration::from_secs(40), move || {
            if !capture.done.get() {
                eprintln!("icloud-session-signin: not finished by itself; showing the window");
                window.present();
            }
        });
    } else {
        window.present();
    }
    capture.main_loop.run();
    window.destroy();
    ExitCode::from(capture.exit.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_apple_stays_in_the_window() {
        assert!(stays_in_window("https://www.icloud.com/"));
        assert!(stays_in_window("https://idmsa.apple.com/appleauth/auth/signin"));
        assert!(stays_in_window("https://icloud.com"));
        assert!(stays_in_window("https://www.cdn-apple.com/x.js"));
        assert!(stays_in_window("about:blank"));
        assert!(!stays_in_window("http://www.icloud.com/"));
        assert!(!stays_in_window("https://evilicloud.com/"));
        assert!(!stays_in_window("https://icloud.com.evil.example/"));
        assert!(!stays_in_window("https://example.com/?u=https://www.icloud.com/"));
        assert!(!stays_in_window("file:///etc/passwd"));
        assert!(!stays_in_window("not a url"));
    }

    #[test]
    fn signed_in_bodies() {
        assert!(is_fully_signed_in(&json!({"dsInfo": {"dsid": "1"}})));
        assert!(!is_fully_signed_in(
            &json!({"dsInfo": {"dsid": "1"}, "hsaChallengeRequired": true})
        ));
        assert!(!is_fully_signed_in(&json!({"dsInfo": {"hsaChallengeRequired": true}})));
        assert!(!is_fully_signed_in(&json!({"error": 1})));
    }

    #[test]
    fn validate_call_carries_the_params() {
        let js = validate_js("abc");
        assert!(js.contains(r#""https://setup.icloud.com/setup/ws/1/validate?clientBuildNumber=2624Build27&clientMasteringNumber=2624Build27&clientId=abc""#));
        assert!(js.contains(r#"credentials: "include""#));
        assert_eq!(client_params("abc")["clientId"], "abc");
    }

    #[test]
    fn find_call_validates_once_then_asks_find_my() {
        let js = find_js("abc");
        assert!(js.contains(r#""clientBuildNumber=2624Build27&clientMasteringNumber=2624Build27&clientId=abc""#));
        assert!(js.contains(r#""https://setup.icloud.com/setup/ws/1/validate""#));
        assert!(js.contains("/fmipservice/client/web/initClient?"));
        assert!(js.contains(r#"credentials: "include""#));
        assert!(js.contains("globalThis.__icloudSessionAccount"));
        // A client id cannot break out of its string.
        let js = find_js(r#"a"; alert(1); ""#);
        assert!(js.contains(
            r#""clientBuildNumber=2624Build27&clientMasteringNumber=2624Build27&clientId=a\"; alert(1); \"""#
        ));
    }

    /// Runs `find_js` three times in node against a fake `fetch`, if node
    /// is installed: one /validate in all, Find My asked every time, and
    /// the validate body handed back only once Find My answers 2xx.
    #[test]
    fn find_call_behaves_in_a_js_engine() {
        if std::process::Command::new("node").arg("--version").output().is_err() {
            eprintln!("node not found: skipping");
            return;
        }
        let body = find_js("abc");
        let script = format!(
            r#"const calls = [];
let findStatus = 450;
globalThis.fetch = async (url, opts) => {{
  calls.push(url);
  if (url.startsWith("https://setup.icloud.com/")) {{
    return {{ok: true, status: 200, text: async () => JSON.stringify({{dsInfo: {{dsid: "42"}},
      webservices: {{findme: {{url: "https://p1-fmipweb.icloud.com:443"}}}}}})}};
  }}
  return {{ok: findStatus < 300, status: findStatus, text: async () => ""}};
}};
const run = async () => {{ {body} }};
(async () => {{
  const a = JSON.parse(await run());
  const b = JSON.parse(await run());
  findStatus = 200;
  const c = JSON.parse(await run());
  console.log(JSON.stringify({{a, b, c, calls}}));
}})();"#
        );
        let out = std::process::Command::new("node")
            .args(["-e", &script])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["a"], json!({"status": 450, "body": ""}));
        assert_eq!(v["b"], json!({"status": 450, "body": ""}));
        assert_eq!(v["c"]["status"], 200);
        assert_eq!(
            serde_json::from_str::<Value>(v["c"]["body"].as_str().unwrap()).unwrap()["dsInfo"]["dsid"],
            "42"
        );
        let calls: Vec<&str> = v["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert_eq!(calls.iter().filter(|c| c.contains("/validate")).count(), 1, "{calls:?}");
        assert_eq!(calls.iter().filter(|c| c.contains("/initClient")).count(), 3);
        assert!(calls[1].starts_with("https://p1-fmipweb.icloud.com:443/fmipservice/client/web/initClient?"));
        assert!(calls[1].ends_with("&dsid=42"));
    }
}
