//! Automatic updates (see `docs/desktop-release.md`).
//!
//! Spike looks for a newer version in the background: a minute after start-up and then every six hours.
//! A newer version is downloaded and its signature is checked against the public key compiled into the app
//! before it is kept. It is installed only when the person chooses "Restart to update": an install restarts
//! the app, so it never happens by itself and never during a call (the screen refuses while a call is up).
//!
//! What this tells the outside: each check asks the update host (GitHub) for the newest version, so the host
//! sees this computer's address and that it runs Spike. Settings turns automatic checks off; "Check for
//! updates" always works.
//!
//! The updater is a build option (the `updater` feature, on by default). Without it every command here says
//! that this build cannot update itself.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct UpdateInfo {
    /// This build can update itself (it was built with the updater and a public key).
    pub supported: bool,
    pub current: String,
    /// A newer version, if a check found one.
    pub available: Option<String>,
    /// Release notes from the update manifest, if any.
    pub notes: Option<String>,
}

/// A newer version that was downloaded and verified, waiting for the person to restart.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    pub version: String,
    pub notes: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct Prefs {
    auto: bool,
}

const PREFS_FILE: &str = "updates.json";

/// Automatic checks are on unless the person turned them off. A missing or unreadable file means on.
pub fn auto_enabled(dir: &Path) -> bool {
    std::fs::read(dir.join(PREFS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Prefs>(&b).ok())
        .is_none_or(|p| p.auto)
}

pub fn set_auto(dir: &Path, on: bool) -> Result<(), String> {
    let json = serde_json::to_vec(&Prefs { auto: on }).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(PREFS_FILE), json).map_err(|e| e.to_string())
}

pub fn current_version(app: &tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

#[cfg(not(feature = "updater"))]
pub fn supported() -> bool {
    false
}

#[cfg(feature = "updater")]
pub fn supported() -> bool {
    true
}

#[cfg(not(feature = "updater"))]
pub async fn check(app: &tauri::AppHandle) -> Result<UpdateInfo, String> {
    Ok(UpdateInfo {
        supported: false,
        current: current_version(app),
        available: None,
        notes: None,
    })
}

#[cfg(not(feature = "updater"))]
pub async fn install(_app: &tauri::AppHandle) -> Result<(), String> {
    Err(
        "this build cannot update itself: install the newer version from where you got this one"
            .into(),
    )
}

#[cfg(not(feature = "updater"))]
pub fn ready() -> Option<Ready> {
    None
}

#[cfg(not(feature = "updater"))]
pub fn start(_app: tauri::AppHandle) {}

#[cfg(feature = "updater")]
mod with_updater {
    use super::{auto_enabled, current_version, Duration, Ready, UpdateInfo};
    use std::sync::Mutex;
    use tauri::{Emitter, Manager};
    use tauri_plugin_updater::UpdaterExt;

    struct Downloaded {
        ready: Ready,
        bytes: Vec<u8>,
    }

    /// The verified package waiting for a restart (at most one).
    static DOWNLOADED: Mutex<Option<Downloaded>> = Mutex::new(None);

    pub fn ready() -> Option<Ready> {
        DOWNLOADED
            .lock()
            .expect("update lock")
            .as_ref()
            .map(|d| d.ready.clone())
    }

    pub async fn check(app: &tauri::AppHandle) -> Result<UpdateInfo, String> {
        let updater = app.updater().map_err(|e| format!("{e}"))?;
        let found = updater
            .check()
            .await
            .map_err(|e| format!("could not check for updates: {e}"))?;
        Ok(UpdateInfo {
            supported: true,
            current: current_version(app),
            available: found.as_ref().map(|u| u.version.clone()),
            notes: found.and_then(|u| u.body),
        })
    }

    /// Look for a newer version and, if there is one, download and verify it. Tells the screen when one is ready.
    /// Errors are returned to the caller, which only logs them: a background check must never bother the person.
    pub async fn fetch(app: &tauri::AppHandle) -> Result<Option<Ready>, String> {
        if let Some(r) = ready() {
            return Ok(Some(r));
        }
        let updater = app.updater().map_err(|e| format!("{e}"))?;
        let Some(update) = updater
            .check()
            .await
            .map_err(|e| format!("could not check for updates: {e}"))?
        else {
            return Ok(None);
        };
        let ready = Ready {
            version: update.version.clone(),
            notes: update.body.clone(),
        };
        // `download` checks the signature against the public key in the app: an unsigned or altered package fails here.
        let bytes = update
            .download(|_, _| {}, || {})
            .await
            .map_err(|e| format!("the update was not downloaded: {e}"))?;
        *DOWNLOADED.lock().expect("update lock") = Some(Downloaded {
            ready: ready.clone(),
            bytes,
        });
        let _ = app.emit("update-ready", ready.clone());
        Ok(Some(ready))
    }

    /// Install the update and restart. Uses the verified package from the background download when it is for the
    /// same version; otherwise downloads it now. The app's data is not touched: it lives in the profile folder.
    pub async fn install(app: &tauri::AppHandle) -> Result<(), String> {
        let updater = app.updater().map_err(|e| format!("{e}"))?;
        let Some(update) = updater
            .check()
            .await
            .map_err(|e| format!("could not check for updates: {e}"))?
        else {
            return Err("you already have the newest version".into());
        };
        let kept = DOWNLOADED.lock().expect("update lock").take();
        match kept {
            Some(d) if d.ready.version == update.version => {
                update
                    .install(&d.bytes)
                    .map_err(|e| format!("the update was not installed: {e}"))?;
            }
            _ => {
                update
                    .download_and_install(|_, _| {}, || {})
                    .await
                    .map_err(|e| format!("the update was not installed: {e}"))?;
            }
        }
        app.restart();
    }

    /// Start the background checks: one minute after start-up, then every six hours, while automatic checks are on.
    pub fn start(app: tauri::AppHandle) {
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            loop {
                let dir = app.state::<crate::AppState>().dir.clone();
                if auto_enabled(&dir) {
                    if let Err(e) = fetch(&app).await {
                        eprintln!("[update] {e}");
                    }
                }
                tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
            }
        });
    }
}

#[cfg(feature = "updater")]
pub use with_updater::{check, fetch, install, ready, start};
