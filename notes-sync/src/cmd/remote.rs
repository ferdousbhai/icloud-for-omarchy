//! Reaching iCloud: which transport a command uses and whose account it is.
//! Replaces icloud-md's `auth/folderAuth.ts` (`bindKnownAccount`,
//! `resolveFolderAccount`): sign-in is icloud-session's job, so all that is
//! left is picking the transport and checking the signed-in account against
//! `--account` (clone) or the vault's bound account (everything else).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::Value;

use super::errors::Error;
use crate::cloudkit::transport::{LiveTransport, ReplayTransport};
use crate::cloudkit::{CkError, Database, Transport};
use crate::vault::state::Account;

/// `ICLOUD_NOTES_SYNC_CASSETTE`: serve CloudKit from a cassette instead of
/// icloud-session (the differential harness).
pub const CASSETTE_ENV: &str = "ICLOUD_NOTES_SYNC_CASSETTE";
/// `ICLOUD_NOTES_SYNC_REQUEST_LOG`: where a cassette run logs its requests.
pub const REQUEST_LOG_ENV: &str = "ICLOUD_NOTES_SYNC_REQUEST_LOG";

/// A connected CloudKit database plus the account it belongs to.
pub struct Remote {
    pub db: Database<Box<dyn Transport>>,
    pub account: Account,
}

/// Opens a [`Remote`]. Commands connect lazily, only once they know they
/// need the network (as icloud-md resolves auth lazily).
pub trait Connector {
    fn connect(&self) -> Result<Remote, Error>;
}

/// The production connector: a cassette when `ICLOUD_NOTES_SYNC_CASSETTE`
/// is set, else icloud-session.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultConnector;

impl Connector for DefaultConnector {
    fn connect(&self) -> Result<Remote, Error> {
        if let Some(cassette) = std::env::var_os(CASSETTE_ENV).filter(|v| !v.is_empty()) {
            let record = std::env::var_os(REQUEST_LOG_ENV)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from);
            let transport = ReplayTransport::open(&PathBuf::from(cassette), record)?;
            let account = Account {
                apple_id: transport.account().apple_id.clone(),
                dsid: transport.account().dsid.clone(),
            };
            return Ok(Remote {
                db: Database::new(Box::new(transport)),
                account,
            });
        }
        let live = LiveTransport::connect()?;
        let account = Account {
            apple_id: live.apple_id().to_owned(),
            dsid: live.dsid().to_owned(),
        };
        Ok(Remote {
            db: Database::new(Box::new(live)),
            account,
        })
    }
}

/// A connector over a fixed transport factory (tests).
pub struct FnConnector<F: Fn() -> Result<Remote, Error>>(pub F);

impl<F: Fn() -> Result<Remote, Error>> Connector for FnConnector<F> {
    fn connect(&self) -> Result<Remote, Error> {
        (self.0)()
    }
}

/// Connects once and hands every later caller the same connection: `sync`
/// runs push and pull over one session (one icloud-sessiond `Session()`
/// call, one TLS connection). A failed connect is not kept; the next caller
/// tries again.
pub struct SharedConnector<'a> {
    inner: &'a dyn Connector,
    connected: RefCell<Option<(Rc<dyn Transport>, Account)>>,
}

impl<'a> SharedConnector<'a> {
    pub fn new(inner: &'a dyn Connector) -> Self {
        SharedConnector {
            inner,
            connected: RefCell::new(None),
        }
    }
}

impl Connector for SharedConnector<'_> {
    fn connect(&self) -> Result<Remote, Error> {
        let mut connected = self.connected.borrow_mut();
        if connected.is_none() {
            let Remote { db, account } = self.inner.connect()?;
            *connected = Some((Rc::from(db.transport), account));
        }
        let (transport, account) = connected.as_ref().expect("connected above");
        Ok(Remote {
            db: Database::new(Box::new(SharedTransport(Rc::clone(transport)))),
            account: account.clone(),
        })
    }
}

/// One of [`SharedConnector`]'s handles on its connection.
struct SharedTransport(Rc<dyn Transport>);

impl Transport for SharedTransport {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        self.0.post_json(path, body)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        self.0.download(url, dest)
    }

    fn download_bytes(&self, url: &str) -> Result<Vec<u8>, CkError> {
        self.0.download_bytes(url)
    }
}

/// `resolveFolderAccount`: the vault must be bound to an account, and it
/// must be the one signed in.
pub fn resolve_folder_account(
    connector: &dyn Connector,
    target_dir: &Path,
    account: Option<&Account>,
) -> Result<Remote, Error> {
    let Some(account) = account else {
        return Err(Error::UnboundAccount {
            target_dir: target_dir.display().to_string(),
        });
    };
    let remote = connector.connect()?;
    if remote.account.dsid != account.dsid {
        return Err(Error::AccountMismatch {
            target_dir: target_dir.display().to_string(),
            expected: account.apple_id.clone(),
            actual: remote.account.apple_id.clone(),
        });
    }
    Ok(remote)
}

/// `clone --account <appleId|dsid>`: must name the signed-in account.
pub fn bind_account(connector: &dyn Connector, requested: Option<&str>) -> Result<Remote, Error> {
    let remote = connector.connect()?;
    if let Some(requested) = requested.filter(|r| !r.is_empty())
        && requested != remote.account.dsid
        && !requested.eq_ignore_ascii_case(&remote.account.apple_id)
    {
        return Err(Error::RequestedAccountMismatch {
            requested: requested.to_owned(),
            actual: remote.account.apple_id.clone(),
        });
    }
    Ok(remote)
}
