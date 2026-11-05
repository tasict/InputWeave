//! The license's requests (docs/architecture.md §11): the
//! [`Keeper`](inputweave_app_core::license::Keeper)'s outbox decides, this
//! runs its requests, one at a time to Polar (over HTTPS, on a thread of its
//! own) and one at a time to the agent (over IPC), and hands each answer
//! back. What the outbox must not lose when the app quits (an activation the
//! agent has not taken, activations to free, a Remove License on its way) is
//! written to `app-license.json` beside the app's settings, readable only by
//! the user, before the request that follows is sent. Also the links to
//! Polar's checkout and customer portal.

use std::cell::{Cell, OnceCell, Ref, RefCell};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use gtk::{gio, glib};
use inputweave_app_core::agent::{AgentError, Answer, NO_RESULT};
use inputweave_app_core::license::{Done, Keeper, Task};
use inputweave_app_core::polar::{self, Response, Transport};
use inputweave_app_core::proto::client_message::Command;
use inputweave_app_core::proto::{RemoveLicense, SetLicenseKey};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{log, paths};

use crate::strings::t;
use crate::ui::Ui;

/// Polar's answers are small; anything longer is not one.
const MAX_BODY: u64 = 256 * 1024;
/// An unreadable `app-license.json` is read again this often.
const REREAD: Duration = Duration::from_secs(60);
/// After this many failed writes in a row the dialog says the license
/// can't be saved (the writes go on).
const WRITES_BEFORE_SAYING: u32 = 3;
/// The longest wait between two tries at writing.
const MAX_WRITE_WAIT: Duration = Duration::from_secs(60);

/// HTTPS with the system's certificates: one agent for each wait, made on
/// the thread of the first request (reading the CA store takes a moment).
struct Https {
    quick: OnceLock<ureq::Agent>,
    patient: OnceLock<ureq::Agent>,
}

/// The system's CA store (`/etc/ssl/certs`, or what `SSL_CERT_FILE` and
/// `SSL_CERT_DIR` name), read once.
fn system_roots() -> ureq::tls::RootCerts {
    static ROOTS: OnceLock<ureq::tls::RootCerts> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let found = rustls_native_certs::load_native_certs();
            if !found.errors.is_empty() {
                log::info(&format!(
                    "reading the system's certificates: {}",
                    found.errors[0]
                ));
            }
            let certs: Vec<_> = found
                .certs
                .iter()
                .map(|der| ureq::tls::Certificate::from_der(der.as_ref()).to_owned())
                .collect();
            ureq::tls::RootCerts::new_with_certs(&certs)
        })
        .clone()
}

fn https_agent(timeout: Duration) -> ureq::Agent {
    // ring, given to ureq rather than taken from a process-wide default.
    let ring = Arc::new(rustls::crypto::ring::default_provider());
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(system_roots())
        .unversioned_rustls_crypto_provider(ring)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(polar::user_agent())
        .build()
        .into()
}

impl Https {
    fn new() -> Https {
        Https {
            quick: OnceLock::new(),
            patient: OnceLock::new(),
        }
    }
}

impl Transport for Https {
    fn post(&self, url: &str, json: &str, timeout: Duration) -> Result<Response, String> {
        let agent = if timeout > polar::TIMEOUT {
            self.patient
                .get_or_init(|| https_agent(polar::ACTIVATE_TIMEOUT))
        } else {
            self.quick.get_or_init(|| https_agent(polar::TIMEOUT))
        };
        let mut answer = agent
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(json)
            .map_err(|e| e.to_string())?;
        let header = |name: &str| {
            answer
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let (date, retry_after) = (header("date"), header("retry-after"));
        let status = answer.status().as_u16();
        // Cut off after the headers is no answer, not an unreadable one.
        let body = answer
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_string()
            .map_err(|e| format!("reading Polar's answer: {e}"))?;
        Ok(Response {
            status,
            body,
            date,
            retry_after,
        })
    }
}

/// `$XDG_CONFIG_HOME/inputweave/app-license.json`.
fn saved_path() -> Option<PathBuf> {
    paths::config_dir().map(|d| d.join("app-license.json"))
}

#[derive(Default)]
pub struct Control {
    keeper: RefCell<Keeper>,
    https: OnceCell<Arc<Https>>,
    /// The key the Enter License Key dialog sent, once the agent took it:
    /// the dialog follows the record from then on.
    entered: RefCell<Option<String>>,
    /// Whether what the keeper holds is kept on disk (not for screenshots).
    persist: Cell<bool>,
    /// `app-license.json` could not be read (permissions, I/O): it is left
    /// as it is, read again at this time, and nothing about the license is
    /// asked meanwhile.
    unreadable: Cell<Option<SystemTime>>,
    /// Failed writes in a row, and when to try the next.
    write_failures: Cell<u32>,
    next_write: Cell<Option<SystemTime>>,
    /// The last trouble with the file, said once.
    said: RefCell<String>,
}

impl Control {
    /// Takes up what the last run left (`app-license.json`), and keeps it
    /// from now on: a grant is reported only once it is written.
    pub fn restore(&self) {
        self.persist.set(true);
        self.read();
    }

    /// Reads `app-license.json`. None there is an empty outbox; one that
    /// cannot be read is left alone and read again later; one that is not
    /// an outbox at all is moved aside to `app-license.json.bad`.
    fn read(&self) {
        let Some(path) = saved_path() else {
            self.say("no folder for the app's settings: the license's pending work waits");
            self.unreadable.set(Some(SystemTime::now() + REREAD));
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) if e.kind() == ErrorKind::InvalidData => Some(String::new()),
            Err(e) => {
                self.say(&format!(
                    "cannot read {}: {e}; tried again later",
                    path.display()
                ));
                self.unreadable.set(Some(SystemTime::now() + REREAD));
                return;
            }
        };
        let mut keeper = match text.map(|t| Keeper::restored(&t)) {
            None => Keeper::default(),
            Some(Some(keeper)) => keeper,
            Some(None) => {
                let bad = path.with_extension("json.bad");
                match std::fs::rename(&path, &bad) {
                    Ok(()) => log::info(&format!(
                        "{} is not the license's pending work: moved to {}",
                        path.display(),
                        bad.display()
                    )),
                    Err(e) => log::info(&format!("cannot move {} aside: {e}", path.display())),
                }
                Keeper::default()
            }
        };
        keeper.keep_on_disk();
        *self.keeper.borrow_mut() = keeper;
        self.unreadable.set(None);
    }

    fn https(&self) -> Arc<Https> {
        self.https.get_or_init(|| Arc::new(Https::new())).clone()
    }

    pub fn keeper(&self) -> Ref<'_, Keeper> {
        self.keeper.borrow()
    }

    /// Whether the computer shows as licensed, everywhere the app says it
    /// (the License page, the sidebar's foot, About).
    pub fn licensed(&self, model: &Snapshot) -> bool {
        self.keeper.borrow().licensed(model.status.license.as_ref())
    }

    /// The pending work can't be kept on disk: the file can't be read, or
    /// writing it failed a few times in a row (it is still tried).
    pub fn cannot_save(&self) -> bool {
        self.unreadable.get().is_some() || self.write_failures.get() >= WRITES_BEFORE_SAYING
    }

    /// Screenshots of the outbox's states (never kept on disk there).
    pub fn replace_keeper(&self, keeper: Keeper) {
        *self.keeper.borrow_mut() = keeper;
    }

    pub fn entered(&self) -> Option<String> {
        self.entered.borrow().clone()
    }

    pub fn forget_entered(&self) {
        *self.entered.borrow_mut() = None;
    }

    /// Writes what the keeper must not lose, when it changed: through a
    /// file beside it, readable only by the user (it holds keys). A failed
    /// write stays to write and is tried again, waiting longer each time up
    /// to a minute; meanwhile no grant is reported.
    fn save(&self) {
        if !self.persist.get()
            || self.unreadable.get().is_some()
            || !self.keeper.borrow().dirty()
            || self.next_write.get().is_some_and(|t| SystemTime::now() < t)
        {
            return;
        }
        let Some(path) = saved_path() else {
            self.write_failed("no folder for the app's settings");
            return;
        };
        let text = self.keeper.borrow().saved();
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let part = path.with_extension("json.part");
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&part)?;
            // Also when a `.part` was left with other permissions.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&part, &path)
        };
        match write() {
            Ok(()) => {
                self.keeper.borrow_mut().written();
                self.write_failures.set(0);
                self.next_write.set(None);
                self.said.borrow_mut().clear();
            }
            Err(e) => self.write_failed(&e.to_string()),
        }
    }

    fn write_failed(&self, error: &str) {
        let failures = self.write_failures.get() + 1;
        self.write_failures.set(failures);
        let wait = Duration::from_secs(1 << failures.min(6)).min(MAX_WRITE_WAIT);
        self.next_write.set(Some(SystemTime::now() + wait));
        self.say(&format!("cannot keep the license's pending work: {error}"));
    }

    /// Logs `text` unless it was the last thing said.
    fn say(&self, text: &str) {
        if *self.said.borrow() != text {
            log::info(text);
            *self.said.borrow_mut() = text.to_owned();
        }
    }
}

/// Starts the requests the license needs, if any: one to Polar and one to
/// the agent can be on their way at once. Called on every refresh (at least
/// once a second) and after every answer, so a wait that ended, the link
/// coming back or the License page appearing are all taken up there.
/// Without the agent only activations are freed; with `app-license.json`
/// unreadable, nothing is asked.
pub fn pump(ui: &Rc<Ui>) {
    if let Some(when) = ui.license.unreadable.get() {
        if SystemTime::now() < when {
            return;
        }
        ui.license.read();
        if ui.license.unreadable.get().is_some() {
            return;
        }
    }
    let model = ui.snapshot();
    let visible = ui.license_shown();
    // A write that failed before is tried again first.
    ui.license.save();
    loop {
        let task = ui.license.keeper.borrow_mut().next(
            model.status.license.as_ref(),
            &model.status.this_device_name,
            visible,
            model.connected(),
            SystemTime::now(),
        );
        let Some(task) = task else {
            break;
        };
        ui.license.save();
        run(ui, task);
    }
    ui.license.save();
}

/// What the agent's answer means to the keeper: `Ok`, the code it refused
/// with, or no answer (none at all, or one this client made up for an
/// answer without a result).
fn refusal(answer: &Result<Answer, AgentError>) -> Result<(), Option<&str>> {
    match answer {
        Ok(_) => Ok(()),
        Err(AgentError::Refused { code, .. }) if !code.is_empty() && code != NO_RESULT => {
            Err(Some(code.as_str()))
        }
        Err(_) => Err(None),
    }
}

fn run(ui: &Rc<Ui>, task: Task) {
    match task {
        Task::Report(result) => {
            let request = ui.agent.request(Command::RecordLicenseResult(result));
            let ui = ui.clone();
            glib::spawn_future_local(async move {
                let answer = request.await;
                if let Err(e) = &answer {
                    log::info(&format!("license report: {e}"));
                }
                let model = ui.snapshot();
                ui.license.keeper.borrow_mut().reported(
                    refusal(&answer),
                    model.status.license.as_ref(),
                    SystemTime::now(),
                );
                ui.license.save();
                // What follows shows from the model, and runs from there.
                ui.refresh();
            });
        }
        Task::Remove => {
            let request = ui.agent.request(Command::RemoveLicense(RemoveLicense {}));
            let ui = ui.clone();
            glib::spawn_future_local(async move {
                let answer = request.await;
                if let Err(e) = &answer {
                    log::info(&format!("cannot remove the license: {e}"));
                }
                let refused = ui
                    .license
                    .keeper
                    .borrow_mut()
                    .removed(refusal(&answer), SystemTime::now());
                ui.license.save();
                if refused {
                    ui.toast(&t(keys::COMMAND_FAILED));
                }
                ui.refresh();
            });
        }
        task => {
            let https = ui.license.https();
            let base = polar::api();
            let (send, receive) = async_channel::bounded::<Done>(1);
            let spawned = std::thread::Builder::new()
                .name("polar".into())
                .spawn(move || {
                    let t: &dyn Transport = https.as_ref();
                    let done = match task {
                        Task::Activate { key, label } => {
                            Done::Activated(polar::activate(t, &base, &key, &label))
                        }
                        Task::Probe { key } => Done::Probed(polar::validate(t, &base, &key, None)),
                        Task::Validate { key, activation_id } => {
                            Done::Validated(polar::validate(t, &base, &key, Some(&activation_id)))
                        }
                        Task::Free { key, activation_id } => {
                            Done::Deactivated(polar::deactivate(t, &base, &key, &activation_id))
                        }
                        Task::Report(_) | Task::Remove => unreachable!("these go to the agent"),
                    };
                    let _ = send.send_blocking(done);
                });
            if spawned.is_err() {
                ui.license.keeper.borrow_mut().abandoned(SystemTime::now());
                return;
            }
            let ui = ui.clone();
            glib::spawn_future_local(async move {
                match receive.recv().await {
                    Ok(done) => ui
                        .license
                        .keeper
                        .borrow_mut()
                        .finished(done, SystemTime::now()),
                    Err(_) => ui.license.keeper.borrow_mut().abandoned(SystemTime::now()),
                }
                // A grant is on disk before its report goes out.
                ui.license.save();
                ui.refresh();
            });
        }
    }
}

/// Enter License Key's Activate: sends the key to the agent; the dialog
/// follows it from there. `done` gets the refusal, if any. The dialog
/// follows the record only once the agent answered (its answer is in the
/// snapshot then), so what the record said about an earlier try of the same
/// key (seats full, invalid) never shows for this one.
pub fn enter(ui: &Rc<Ui>, key: String, done: impl FnOnce(Result<(), Option<String>>) + 'static) {
    *ui.license.entered.borrow_mut() = None;
    ui.license.keeper.borrow_mut().retry_now(&key);
    let request = ui
        .agent
        .request(Command::SetLicenseKey(SetLicenseKey { key: key.clone() }));
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        match request.await {
            Ok(_) => {
                *ui.license.entered.borrow_mut() = Some(key);
                done(Ok(()));
                pump(&ui);
            }
            Err(AgentError::Refused { code, .. }) if !code.is_empty() && code != NO_RESULT => {
                *ui.license.entered.borrow_mut() = None;
                done(Err(Some(code)));
            }
            Err(AgentError::Refused { .. }) => {
                *ui.license.entered.borrow_mut() = None;
                done(Err(None));
            }
            Err(AgentError::Unavailable(_)) => {
                *ui.license.entered.borrow_mut() = None;
                done(Err(None));
            }
        }
    });
}

/// Try Again after seats full: the same key, entered again.
pub fn try_again(ui: &Rc<Ui>) {
    let model = ui.snapshot();
    let Some(key) = model
        .status
        .license
        .as_ref()
        .filter(|l| !l.key.is_empty())
        .map(|l| l.key.clone())
    else {
        return;
    };
    ui.license.keeper.borrow_mut().retry_now(&key);
    ui.command(Command::SetLicenseKey(SetLicenseKey { key }));
}

/// Remove License, once confirmed: the outbox frees this computer's seat
/// with Polar (queued first, and kept until Polar answers), then removes
/// the license here, trying again until the agent takes it.
pub fn remove(ui: &Rc<Ui>) {
    let model = ui.snapshot();
    ui.license
        .keeper
        .borrow_mut()
        .remove(model.status.license.as_ref());
    ui.license.save();
    ui.refresh();
    pump(ui);
}

/// Opens Polar's checkout or customer portal in the browser.
pub fn open(ui: &Rc<Ui>, url: &str) {
    let parent = ui.window().window().clone();
    gtk::UriLauncher::new(url).launch(Some(&parent), gio::Cancellable::NONE, |result| {
        if let Err(e) = result {
            log::info(&format!("cannot open a link: {e}"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use inputweave_app_core::polar::{Activation, Deactivation, Validation};

    /// The key `scripts/mock-polar.py` knows by default
    /// (testdata/polar/validate-200.json).
    const MOCK_KEY: &str = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42";

    /// Activate, validate and deactivate over HTTP against the mock:
    ///
    /// ```sh
    /// python3 scripts/mock-polar.py --port 8787 --no-rate-limit &
    /// INPUTWEAVE_POLAR_URL=http://127.0.0.1:8787/v1/customer-portal/license-keys/ \
    ///     cargo test -p inputweave-app -- --ignored against_the_mock
    /// ```
    #[test]
    #[ignore = "needs scripts/mock-polar.py at INPUTWEAVE_POLAR_URL"]
    fn against_the_mock() {
        let base = polar::api();
        assert_ne!(base, polar::API, "INPUTWEAVE_POLAR_URL is set");
        let https = Https::new();
        let activation_id = match polar::activate(&https, &base, MOCK_KEY, "ubuntu-desk").outcome {
            Activation::Activated { activation_id, .. } => activation_id,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            polar::validate(&https, &base, MOCK_KEY, Some(&activation_id)).outcome,
            Validation::Valid { .. }
        ));
        assert_eq!(
            polar::deactivate(&https, &base, MOCK_KEY, &activation_id),
            Deactivation::Done
        );
        assert_eq!(
            polar::validate(&https, &base, MOCK_KEY, Some(&activation_id)).outcome,
            Validation::NotFound
        );
    }

    /// Polar itself, over HTTPS with the system's certificates: a key that
    /// does not exist is a 404, so TLS, Cloudflare's view of the User-Agent
    /// and the reading of the answer all hold.
    #[test]
    #[ignore = "reaches api.polar.sh"]
    fn polar_answers_an_unknown_key_with_not_found() {
        let https = Https::new();
        let answer = polar::validate(
            &https,
            polar::API,
            "INPUTWEAVE-00000000-0000-4000-8000-000000000000",
            None,
        );
        assert_eq!(answer.outcome, Validation::NotFound);
        assert!(answer.at.is_some(), "Polar's answer carries a Date");
    }
}
