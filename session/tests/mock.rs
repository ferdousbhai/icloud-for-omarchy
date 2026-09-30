//! Mock mode: no D-Bus, a fake signed-in session, every request sent to
//! the mock URL (sign-in too, as `/mock/reauthenticate`). One test, because it sets process environment.

use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;

use icloud_session::{Error, MOCK_APPLE_ID, MOCK_DSID, Session};

#[test]
fn mock_mode_needs_no_dbus_and_rewrites_every_url() {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
    {
        let (server, seen) = (server.clone(), seen.clone());
        thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut body = Vec::new();
                request.as_reader().read_to_end(&mut body).unwrap();
                let cookie = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Cookie"))
                    .map(|h| h.value.to_string());
                seen.lock().unwrap().push((request.url().to_string(), cookie));
                let status = if request.url().starts_with("/gone") {
                    421
                } else if request.url().starts_with("/findme-auth") {
                    450
                } else {
                    200
                };
                let _ = request.respond(tiny_http::Response::from_string("{\"ok\":true}").with_status_code(status));
            }
        });
    }
    // SAFETY: the only test in this binary; no other thread reads the environment.
    unsafe {
        std::env::set_var("ICLOUD_SESSION_MOCK", "1");
        std::env::set_var("ICLOUD_SESSION_MOCK_URL", format!("{base}/"));
        // Any D-Bus use would fail loudly.
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus");
    }

    let status = icloud_session::status().unwrap();
    assert!(status.signed_in && !status.signing_in && status.find_my_authorized);
    assert_eq!(status.dsid.as_deref(), Some(MOCK_DSID));
    // Signing in tells the fake server, which plays the account.
    icloud_session::sign_in().unwrap();
    let reauth = seen.lock().unwrap().drain(..).map(|(url, _)| url).collect::<Vec<_>>();
    assert_eq!(reauth.len(), 1);
    assert!(reauth[0].starts_with("/mock/reauthenticate?"), "{reauth:?}");
    icloud_session::authorize_find_my().unwrap();
    icloud_session::sign_out().unwrap();
    assert_eq!(icloud_session::watch().unwrap().next(), None);

    let s = Session::connect().unwrap();
    assert_eq!(s.apple_id(), MOCK_APPLE_ID);
    let ws = s.webservices().unwrap();
    assert_eq!(ws.url("findme"), Some(base.as_str()));
    assert_eq!(ws.url("ckdatabasews"), Some(base.as_str()));

    let r = s
        .get("https://p42-fmipweb.icloud.com:443/fmipservice/client/web/refreshClient?x=1")
        .unwrap();
    assert_eq!(r.json::<serde_json::Value>().unwrap()["ok"], true);
    let (url, cookie) = seen.lock().unwrap()[0].clone();
    assert_eq!(
        url,
        "/fmipservice/client/web/refreshClient?x=1&clientBuildNumber=mock&clientMasteringNumber=mock&clientId=mock&dsid=mock"
    );
    assert_eq!(cookie, None);

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("photo.jpg");
    s.download("https://cvws.icloud-content.com/B/abc?o=1", &dest).unwrap();
    assert_eq!(fs::read_to_string(&dest).unwrap(), "{\"ok\":true}");
    assert_eq!(seen.lock().unwrap()[1].0, "/B/abc?o=1");

    assert!(matches!(s.get(&format!("{base}/gone")), Err(Error::SignInRequired)));
    let before = seen.lock().unwrap().len();
    assert!(matches!(
        s.post_json(&format!("{base}/findme-auth"), &serde_json::json!({})),
        Err(Error::FindMyAuthRequired)
    ));
    assert_eq!(seen.lock().unwrap().len(), before + 1, "a 450 is not retried");
}
