//! Ports icloud-md `src/commands/clone.test.ts`.

use icloud_notes_sync::cmd::clone::{CloneOptions, run_clone_with};
use icloud_notes_sync::cmd::remote::{FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress};
use icloud_notes_sync::vault::state::{Account, CloneState, write_clone_state};

#[test]
fn refuses_an_already_cloned_folder_without_authenticating() {
    let dir = tempfile::tempdir().unwrap();
    let state = CloneState {
        account: Some(Account {
            apple_id: "me@example.com".into(),
            dsid: "1234".into(),
        }),
        sync_token: Some("token".into()),
        ..Default::default()
    };
    write_clone_state(dir.path(), &state).unwrap();
    let connector = FnConnector(|| -> Result<Remote, Error> { panic!("must not authenticate") });
    let err = run_clone_with(
        &connector,
        dir.path(),
        &mut NoProgress,
        &mut |_| {},
        &CloneOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("is already a cloned notes directory"));
}
