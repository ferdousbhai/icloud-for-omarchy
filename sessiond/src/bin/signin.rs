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
const SETUP_PREFIXES: [&str; 2] = [
    "https://setup.icloud.com/setup/ws/1/accountLogin",
    "https://setup.icloud.com/setup/ws/1/validate",
];
const TOKEN: &str = "X-APPLE-WEBAUTH-TOKEN";
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

fn is_setup_call(uri: &str) -> bool {
    SETUP_PREFIXES.iter().any(|p| {
        uri.strip_prefix(p)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('?'))
    })
}

/// A completed sign-in: account info present and no 2FA still pending
/// (icloud-md's `isFullySignedInBody`).
fn is_fully_signed_in(body: &Value) -> bool {
    let challenge = |v: &Value| v.get("hsaChallengeRequired") == Some(&Value::Bool(true));
    body.get("dsInfo").is_some_and(Value::is_object) && !challenge(body) && !challenge(&body["dsInfo"])
}

fn client_params(uri: &str) -> Value {
    let mut params = json!({"clientId": null, "clientBuildNumber": null, "clientMasteringNumber": null});
    if let Ok(uri) = url::Url::parse(uri) {
        for (k, v) in uri.query_pairs() {
            if params.get(k.as_ref()).is_some() && !v.is_empty() {
                params[k.as_ref()] = Value::String(v.into_owned());
            }
        }
    }
    params
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
}

impl Capture {
    /// Waits a moment for the setup response's own Set-Cookies to land in
    /// the jar, then prints the jar and the params and quits.
    fn finish(self: &Rc<Self>, setup_uri: String) {
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
                        let mut out = client_params(&setup_uri);
                        out["cookies"] = Value::Array(cookies_json(cookies));
                        println!("{out}");
                        me2.exit.set(0);
                    }
                    Err(e) => eprintln!("icloud-session-signin: reading cookies: {e}"),
                }
                me2.main_loop.quit();
            });
        });
    }

    /// Fallback when the setup response body cannot be read: a 2xx setup
    /// call plus the token cookie in the jar.
    fn finish_if_token(self: &Rc<Self>, setup_uri: String) {
        let me = self.clone();
        self.cookies.all_cookies(None::<&gio::Cancellable>, move |result| {
            let has_token = result
                .map(|cookies| cookies.into_iter().any(|mut c| c.name().is_some_and(|n| n == TOKEN)))
                .unwrap_or(false);
            if has_token {
                me.finish(setup_uri);
            }
        });
    }
}

/// The window's Wayland app_id and desktop entry name.
const APP_ID: &str = "io.github.ferdousbhai.ICloudSession";

fn main() -> ExitCode {
    if let Some(arg) = std::env::args().nth(1) {
        return if arg == "-V" || arg == "--version" {
            println!("icloud-session-signin {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        } else {
            eprintln!("usage: icloud-session-signin   (prints the captured session as JSON)");
            ExitCode::from(64)
        };
    }
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
    });

    // The page's own setup calls: remember the params, check the answer.
    let last_setup_uri = Rc::new(RefCell::new(None::<String>));
    {
        let capture = capture.clone();
        let last_setup_uri = last_setup_uri.clone();
        view.connect_resource_load_started(move |_, resource, request| {
            let Some(uri) = request.uri().map(|u| u.to_string()) else {
                return;
            };
            if !is_setup_call(&uri) {
                return;
            }
            *last_setup_uri.borrow_mut() = Some(uri.clone());
            let capture = capture.clone();
            resource.connect_finished(move |resource| {
                let status = resource.response().map_or(0, |r| r.status_code());
                if !(200..300).contains(&status) {
                    return;
                }
                let capture = capture.clone();
                let uri = uri.clone();
                resource.data(None::<&gio::Cancellable>, move |data| match data {
                    Ok(bytes) => {
                        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                        if is_fully_signed_in(&body) {
                            capture.finish(uri);
                        }
                    }
                    Err(_) => capture.finish_if_token(uri),
                });
            });
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
    view.connect_load_changed(|view, event| {
        if event == webkit6::LoadEvent::Finished {
            eprintln!("icloud-session-signin: loaded {}", view.uri().unwrap_or_default());
        }
    });
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
    view.load_uri(HOME);
    window.present();
    capture.main_loop.run();
    window.destroy();
    ExitCode::from(capture.exit.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_calls() {
        assert!(is_setup_call("https://setup.icloud.com/setup/ws/1/validate?clientId=x"));
        assert!(is_setup_call("https://setup.icloud.com/setup/ws/1/accountLogin"));
        assert!(!is_setup_call("https://setup.icloud.com/setup/ws/1/validateX"));
        assert!(!is_setup_call("https://setup.icloud.com/setup/ws/1/logout"));
    }

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
    fn params_from_the_setup_query() {
        let p = client_params(
            "https://setup.icloud.com/setup/ws/1/validate?clientBuildNumber=2530B&clientMasteringNumber=2530M&clientId=abc&requestId=r",
        );
        assert_eq!(
            p,
            json!({"clientId": "abc", "clientBuildNumber": "2530B", "clientMasteringNumber": "2530M"})
        );
        assert_eq!(client_params("https://setup.icloud.com/x")["clientId"], Value::Null);
    }
}
