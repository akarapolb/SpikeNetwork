//! Spike Connect desktop (Tauri 2). See `docs/adr/0019-tauri-desktop-shell.md`.
//!
//! The commands here are thin wrappers over [`api::Session`]; the webview
//! never sees a key. Only the unlocked session is held in memory, and only
//! while the app is unlocked.

pub mod api;

use api::{AgentsDto, MessageDto, Overview, Session, SyncDto};
use spike_core::vault::KdfParams;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Manager, State};

pub struct AppState {
    dir: PathBuf,
    session: Mutex<Option<Arc<Session>>>,
}

#[derive(serde::Serialize)]
pub struct Status {
    exists: bool,
    unlocked: bool,
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

fn session(state: &State<'_, AppState>) -> Result<Arc<Session>, String> {
    state
        .session
        .lock()
        .map_err(|_| "internal error".to_string())?
        .clone()
        .ok_or_else(|| "locked".to_string())
}

fn set(state: &State<'_, AppState>, s: Option<Session>) {
    *state.session.lock().expect("session lock") = s.map(Arc::new);
}

#[tauri::command]
fn app_status(state: State<'_, AppState>) -> Status {
    Status {
        exists: api::exists(&state.dir),
        unlocked: state.session.lock().map(|s| s.is_some()).unwrap_or(false),
    }
}

#[tauri::command]
async fn create_identity(state: State<'_, AppState>, passphrase: String) -> Result<(), String> {
    let dir = state.dir.clone();
    // Argon2 takes a moment: keep it off the UI thread.
    let s = tokio::task::spawn_blocking(move || {
        Session::create(&dir, passphrase, KdfParams::default())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(err)?;
    set(&state, Some(s));
    Ok(())
}

#[tauri::command]
async fn unlock(state: State<'_, AppState>, passphrase: String) -> Result<(), String> {
    let dir = state.dir.clone();
    let s = tokio::task::spawn_blocking(move || Session::unlock(&dir, passphrase))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)?;
    set(&state, Some(s));
    Ok(())
}

#[tauri::command]
fn lock(state: State<'_, AppState>) {
    set(&state, None);
}

#[tauri::command]
fn overview(state: State<'_, AppState>) -> Result<Overview, String> {
    session(&state)?.overview().map_err(err)
}

#[tauri::command]
fn conversation(state: State<'_, AppState>, name: String) -> Result<Vec<MessageDto>, String> {
    session(&state)?.conversation(&name, 200).map_err(err)
}

#[tauri::command]
async fn send(state: State<'_, AppState>, name: String, text: String) -> Result<usize, String> {
    session(&state)?.send(&name, &text).await.map_err(err)
}

#[tauri::command]
async fn sync(state: State<'_, AppState>) -> Result<SyncDto, String> {
    session(&state)?
        .sync(Duration::from_millis(700))
        .await
        .map_err(err)
}

#[tauri::command]
async fn mark_read(state: State<'_, AppState>, name: String) -> Result<(), String> {
    session(&state)?.mark_read(&name).await.map_err(err)
}

#[tauri::command]
fn add_contact(state: State<'_, AppState>, name: String, card: String) -> Result<String, String> {
    session(&state)?.add_contact(&name, &card).map_err(err)
}

#[tauri::command]
fn mark_verified(
    state: State<'_, AppState>,
    name: String,
    fingerprint: String,
) -> Result<(), String> {
    session(&state)?
        .mark_verified(&name, &fingerprint)
        .map_err(err)
}

#[tauri::command]
fn agents(state: State<'_, AppState>) -> Result<AgentsDto, String> {
    session(&state)?.agents().map_err(err)
}

#[tauri::command]
async fn decide(state: State<'_, AppState>, id: String, approve: bool) -> Result<String, String> {
    session(&state)?.decide(&id, approve).await.map_err(err)
}

#[tauri::command]
fn set_relay(state: State<'_, AppState>, url: String) -> Result<(), String> {
    session(&state)?.set_relay(&url).map_err(err)
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // SPIKE_DESKTOP_HOME lets you run a second instance (another identity) on one machine.
            let dir = match std::env::var_os("SPIKE_DESKTOP_HOME") {
                Some(d) => PathBuf::from(d),
                None => app.path().app_data_dir()?,
            };
            app.manage(AppState {
                dir,
                session: Mutex::new(None),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_status,
            create_identity,
            unlock,
            lock,
            overview,
            conversation,
            send,
            sync,
            mark_read,
            add_contact,
            mark_verified,
            agents,
            decide,
            set_relay,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Spike Connect");
}
