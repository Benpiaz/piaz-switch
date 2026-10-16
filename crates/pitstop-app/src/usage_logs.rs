//! The Usage pane's side of the app: the tools' session logs read so far,
//! the price table in use, and the command that turns both into a report.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use pitstop_core::ledger::{self, Ledger};
use pitstop_core::pricing::{self, Prices};
use pitstop_core::{Error, Host};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::app;
use crate::model::UiError;
use crate::spend::{self, Range, UsageReport};

#[derive(Default)]
pub struct UsageLogs {
    /// One scan at a time; a second caller waits and then finds little new.
    ledger: Mutex<Ledger>,
    prices: Mutex<Option<Arc<Prices>>>,
}

/// The report and what it was read from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAnswer {
    #[serde(flatten)]
    pub report: UsageReport,
    /// Epoch seconds the logs were read at.
    pub scanned_at: i64,
    /// Log files read.
    pub files: usize,
    /// Logs that could not be read.
    pub errors: Vec<String>,
    /// The folders looked in, for an empty pane to name.
    pub searched: Vec<String>,
    /// When the price table in use was fetched; none for the shipped one.
    pub prices_at: Option<i64>,
}

impl UsageLogs {
    /// The price table: the one fetched last, over the one Pitstop ships.
    fn prices(&self, host: &dyn Host) -> Arc<Prices> {
        let mut prices = self.prices.lock().unwrap_or_else(|e| e.into_inner());
        prices
            .get_or_insert_with(|| Arc::new(Prices::new(Prices::shipped(), pricing::cached(host))))
            .clone()
    }

    fn set_prices(&self, prices: Prices) {
        *self.prices.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(prices));
    }
}

/// The logs' folders. With `PITSTOP_HOME` everything comes from that
/// sandbox, so the tools' own variables are not followed.
fn roots(host: &dyn Host) -> ledger::Roots {
    let sandboxed = std::env::var_os("PITSTOP_HOME").is_some();
    ledger::roots(&host.home(), &|name| {
        if sandboxed {
            None
        } else {
            std::env::var_os(name)
        }
    })
}

fn read(app: &AppHandle, range: Range) -> UsageAnswer {
    let state = app.state::<UsageLogs>();
    let host = app::host();
    let roots = roots(&host);
    let prices = state.prices(&host);
    let mut ledger = state.ledger.lock().unwrap_or_else(|e| e.into_inner());
    let scan = ledger.scan(&roots, SystemTime::now());
    let report = spend::report(
        ledger.entries(),
        &prices,
        range,
        &spend::buckets(range, &chrono::Local::now()),
    );
    UsageAnswer {
        report,
        scanned_at: app::now(),
        files: scan.files,
        errors: scan.errors,
        searched: roots.all().map(|p| p.display().to_string()).collect(),
        prices_at: prices.fetched_at(),
    }
}

/// Read what the logs gained and report `range` (`24h`, `7d`, `30d`, `90d`).
#[tauri::command]
pub async fn usage_report(app: AppHandle, range: String) -> Result<UsageAnswer, UiError> {
    let range = Range::parse(&range)
        .ok_or_else(|| UiError::from(&Error::InvalidData(format!("unknown range `{range}`"))))?;
    tauri::async_runtime::spawn_blocking(move || read(&app, range))
        .await
        .map_err(|e| UiError::from(&Error::InvalidData(e.to_string())))
}

/// How often the saved price table's age is checked while the app runs.
const PRICE_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(3_600);

/// Keep the price table at most a day old, off the calling thread: at start
/// and every hour after, fetch it when the saved one is missing or a day
/// old. The pane hears `usage-changed` when a new one lands.
pub fn refresh_prices(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        let host = app::host();
        let now = app::now();
        if pricing::stale(pricing::cached(&host).as_ref(), now) {
            if let Ok(table) = pricing::fetch(now) {
                let _ = pricing::save(&host, &table);
                app.state::<UsageLogs>()
                    .set_prices(Prices::new(Prices::shipped(), Some(table)));
                let _ = app.emit("usage-changed", ());
            }
        }
        std::thread::sleep(PRICE_CHECK_EVERY);
    });
}
