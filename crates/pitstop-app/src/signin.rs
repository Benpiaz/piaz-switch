//! A private sign-in run from the window: one at a time, watched on a
//! thread that tells the window what the tool says through `sign-in` events.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pitstop_core::signin::{self, Progress, Session};
use pitstop_core::ProviderId;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::app::{self, AppState};
use crate::model::UiError;

/// How long a sign-in may wait for the browser before it is given up.
const GIVE_UP_AFTER: Duration = Duration::from_secs(15 * 60);

pub struct SignInJob {
    provider: ProviderId,
    session: Arc<Mutex<Option<Session>>>,
    url: Arc<Mutex<Option<String>>>,
}

impl SignInJob {
    /// The sign-in page the tool printed, for the Open button.
    pub fn url(&self) -> Option<String> {
        self.url.lock().ok()?.clone()
    }

    pub fn send_code(&self, code: &str) -> Result<(), UiError> {
        let mut session = self.session.lock().map_err(|_| gone())?;
        let session = session.as_mut().ok_or_else(gone)?;
        session.send_code(code).map_err(|e| UiError::from(&e))
    }

    /// Stop the tool; the watcher reports the sign-in as cancelled.
    pub fn cancel(&self) {
        if let Ok(mut session) = self.session.lock() {
            session.take();
        }
    }

    fn running(&self) -> bool {
        self.session.lock().map(|s| s.is_some()).unwrap_or(false)
    }
}

fn gone() -> UiError {
    UiError::new("signIn", "The sign-in is no longer running.")
}

fn busy() -> UiError {
    UiError::new("signInRunning", "Another sign-in is already running.")
}

/// Whether a sign-in to `provider` is running: Antigravity's runs as a
/// window of Antigravity itself, which closing Antigravity would close too.
pub fn running_for(app: &AppHandle, provider: ProviderId) -> bool {
    let state = app.state::<AppState>();
    let job = state.sign_in.lock();
    job.is_ok_and(|job| {
        job.as_ref()
            .is_some_and(|job| job.provider == provider && job.running())
    })
}

/// Stop the sign-in that is running, if any; its watcher reports it cancelled.
pub fn cancel(app: &AppHandle) {
    let state = app.state::<AppState>();
    let job = state.sign_in.lock();
    if let Ok(job) = job {
        if let Some(job) = job.as_ref() {
            job.cancel();
        }
    };
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SignInEvent {
    /// `running`, `done`, `failed` or `cancelled`.
    phase: &'static str,
    provider: &'static str,
    name: String,
    again: bool,
    url: Option<String>,
    wants_code: bool,
    refused_code: bool,
    error: Option<UiError>,
}

/// Start `provider`'s sign-in for the account `name`: a new account, or
/// `again` for one whose parked login no longer works.
pub fn start(
    app: &AppHandle,
    provider: ProviderId,
    name: String,
    again: bool,
) -> Result<(), UiError> {
    let state = app.state::<AppState>();
    if state
        .sign_in
        .lock()
        .map_err(|_| gone())?
        .as_ref()
        .is_some_and(SignInJob::running)
    {
        return Err(busy());
    }

    // Refuse a name before anyone signs in, not after.
    let engine = app::engine();
    if again {
        let known = engine
            .state()
            .map_err(|e| UiError::from(&e))?
            .accounts
            .iter()
            .any(|a| a.provider == provider && a.name == name);
        if !known {
            return Err(UiError::from(&pitstop_core::Error::UnknownAccount(name)));
        }
    } else {
        engine
            .check_new_name(provider, &name)
            .map_err(|e| UiError::from(&e))?;
    }

    // Started without holding the slot: finding the tool can take seconds.
    let session = Session::start(provider, &app::real_home()).map_err(|e| UiError::from(&e))?;
    let mut slot = state.sign_in.lock().map_err(|_| gone())?;
    if slot.as_ref().is_some_and(SignInJob::running) {
        // Another one started meanwhile; dropping this one stops its tool.
        return Err(busy());
    }
    let job = SignInJob {
        provider,
        session: Arc::new(Mutex::new(Some(session))),
        url: Arc::new(Mutex::new(None)),
    };
    let (session, url) = (Arc::clone(&job.session), Arc::clone(&job.url));
    *slot = Some(job);
    drop(slot);

    let app = app.clone();
    std::thread::spawn(move || watch(&app, session, url, provider, name, again));
    Ok(())
}

fn watch(
    app: &AppHandle,
    session: Arc<Mutex<Option<Session>>>,
    url: Arc<Mutex<Option<String>>>,
    provider: ProviderId,
    name: String,
    again: bool,
) {
    let event = |phase, progress: &Progress, error: Option<UiError>| SignInEvent {
        phase,
        provider: provider.as_str(),
        name: name.clone(),
        again,
        url: progress.url.clone(),
        wants_code: progress.wants_code,
        refused_code: progress.refused_code,
        error,
    };
    let started = Instant::now();
    let mut said = Progress::default();
    let _ = app.emit("sign-in", event("running", &said, None));

    let outcome = loop {
        std::thread::sleep(Duration::from_millis(250));
        let Ok(mut guard) = session.lock() else {
            break Err(gone());
        };
        let Some(running) = guard.as_mut() else {
            let _ = app.emit("sign-in", event("cancelled", &said, None));
            return;
        };
        let progress = running.progress();
        if progress != said {
            said = progress;
            if let Ok(mut url) = url.lock() {
                url.clone_from(&said.url);
            }
            let _ = app.emit("sign-in", event("running", &said, None));
        }
        match running.try_finish() {
            Ok(None) if started.elapsed() < GIVE_UP_AFTER => continue,
            Ok(None) => {
                guard.take();
                break Err(UiError::new(
                    "signInTimedOut",
                    "The sign-in waited too long for the browser and was stopped.",
                ));
            }
            Ok(Some(true)) => {
                let login = signin::take_login(&app::host(), provider, running.dir());
                // Dropping the session deletes its directory.
                guard.take();
                break login.map_err(|e| UiError::from(&e));
            }
            Ok(Some(false)) => {
                let output = running.output();
                guard.take();
                let last = output
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("The sign-in did not finish.");
                break Err(UiError::new("signIn", last));
            }
            Err(e) => {
                guard.take();
                break Err(UiError::from(&e));
            }
        }
    };

    let result = outcome.and_then(|login| {
        let state = app.state::<AppState>();
        let _op = state.ops.lock();
        let engine = app::engine();
        let parked = if again {
            engine
                .sign_in_again(&name, Some(provider), &login)
                .map(|_| ())
        } else {
            engine.add_signed_in(provider, &name, &login).map(|_| ())
        };
        parked.map_err(|e| UiError::from(&e))
    });
    match result {
        Ok(()) => {
            let _ = app.emit("sign-in", event("done", &said, None));
            app::refresh(app);
            app::read_usage_now(app);
        }
        Err(error) => {
            let _ = app.emit("sign-in", event("failed", &said, Some(error)));
        }
    }
}
