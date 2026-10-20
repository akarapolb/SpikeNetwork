//! Updates, only when you ask for them.
//!
//! The app never checks for updates by itself: a check is a request to the update server, which
//! would tell its operator that this computer runs Spike and when. The person presses "Check for
//! updates" in Settings; nothing else contacts the server.
//!
//! The updater is a build option (`--features updater`) because it needs a signing key pair and an
//! update server that do not exist yet. Without the feature every command here answers that this
//! build cannot update itself. Downloads are verified against the public key compiled into the app
//! (`plugins.updater.pubkey` in the release config) before anything is installed. See
//! `docs/desktop-release.md`.

use serde::Serialize;

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

#[cfg(feature = "updater")]
pub async fn check(app: &tauri::AppHandle) -> Result<UpdateInfo, String> {
    use tauri_plugin_updater::UpdaterExt;
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

/// Download the newer version, check its signature, install it and restart the app. The app's own
/// data is not touched: it lives in the profile folder, not next to the program.
#[cfg(feature = "updater")]
pub async fn install(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| format!("{e}"))?;
    let Some(update) = updater
        .check()
        .await
        .map_err(|e| format!("could not check for updates: {e}"))?
    else {
        return Err("you already have the newest version".into());
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("the update was not installed: {e}"))?;
    app.restart();
}
