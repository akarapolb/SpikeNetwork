//! Spike Connect desktop (Tauri 2). See `docs/adr/0019-tauri-desktop-shell.md`.
//!
//! The commands here are thin wrappers over [`api::Session`]; the webview
//! never sees a key. Only the unlocked session is held in memory, and only
//! while the app is unlocked.

pub mod api;
pub mod updates;

use api::{
    ActionTypeDto, AgentsDto, BlockedDto, DirectDto, GrantRequest, GroupDetailDto, GroupSendDto,
    InstalledDto, LinkedDto, MessageDto, MessageRequestDto, Overview, Session, SyncDto,
};
use spike_core::vault::KdfParams;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tauri::{Emitter, Manager, State};

pub struct AppState {
    dir: PathBuf,
    session: Mutex<Option<Arc<Session>>>,
    /// The task that waits on the relay and wakes a sync. Aborted on lock, so nothing keeps the keys alive.
    listener: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// A backup file opened on the create screen, waiting for the person to confirm the restore.
    backup: Mutex<Option<api::backup::Opened>>,
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
async fn create_identity(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    passphrase: String,
) -> Result<(), String> {
    let dir = state.dir.clone();
    // Argon2 takes a moment: keep it off the UI thread.
    let s = tokio::task::spawn_blocking(move || {
        Session::create(&dir, passphrase, KdfParams::default())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(err)?;
    set(&state, Some(s));
    start_listener(&app, &state);
    Ok(())
}

#[tauri::command]
async fn unlock(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    passphrase: String,
) -> Result<(), String> {
    let dir = state.dir.clone();
    let s = tokio::task::spawn_blocking(move || Session::unlock(&dir, passphrase))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)?;
    set(&state, Some(s));
    start_listener(&app, &state);
    Ok(())
}

#[tauri::command]
fn lock(app: tauri::AppHandle, state: State<'_, AppState>) {
    stop_listener(&state);
    set(&state, None);
    set_title(&app, 0);
}

fn set_title(app: &tauri::AppHandle, attention: usize) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_title(&if attention > 0 {
            format!("({attention}) Spike Connect")
        } else {
            "Spike Connect".to_string()
        });
    }
}

/// The window title and taskbar show what needs you: unread messages and waiting requests.
#[tauri::command]
fn set_attention(app: tauri::AppHandle, count: usize) {
    set_title(&app, count);
}

fn stop_listener(state: &State<'_, AppState>) {
    if let Some(h) = state.listener.lock().expect("listener lock").take() {
        h.abort();
    }
}

fn start_listener(app: &tauri::AppHandle, state: &State<'_, AppState>) {
    stop_listener(state);
    let Some(session) = state.session.lock().expect("session lock").clone() else {
        return;
    };
    let weak = Arc::downgrade(&session);
    let handle = tauri::async_runtime::spawn({
        let app = app.clone();
        async move {
            tokio::select! {
                _ = listen(app.clone(), weak.clone()) => {}
                _ = watch_direct(app, weak) => {}
            }
        }
    });
    *state.listener.lock().expect("listener lock") = Some(handle);
}

/// Instant delivery. Holds one relay connection and waits for the relay to push something. It never
/// acknowledges: it only wakes a normal sync (which does the decrypting and acknowledging), so the
/// ratchet state is never touched from two places. Holds no strong reference while waiting.
/// Debug builds log what the listener does (never content); release builds are silent.
macro_rules! dbg_log {
    ($($t:tt)*) => { #[cfg(debug_assertions)] eprintln!("[listener] {}", format!($($t)*)); };
}

/// Direct connections (ADR 0027): start listening if the person turned them on, and tell the
/// screen when something arrived that way. Holds no strong reference while it waits.
async fn watch_direct(app: tauri::AppHandle, weak: Weak<Session>) {
    let Some(s) = weak.upgrade() else { return };
    if let Err(e) = s.direct_resume().await {
        let _ = app.emit("direct-error", format!("{e:#}"));
    }
    let signal = s.direct_signal();
    drop(s);
    loop {
        signal.notified().await;
        let Some(s) = weak.upgrade() else { return };
        if let Some(r) = s.direct_wait(0).await {
            if let Some(w) = app.get_webview_window("main") {
                if !w.is_focused().unwrap_or(true) {
                    let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
                }
            }
            set_title(&app, s.attention().unwrap_or(0));
            let _ = app.emit("synced", r);
        }
    }
}

async fn listen(app: tauri::AppHandle, weak: Weak<Session>) {
    use serde_json::json;
    dbg_log!("started");
    let mut retry = Duration::from_millis(200);
    let mut online = None;
    loop {
        let Some(s) = weak.upgrade() else { return };
        if s.paused() {
            drop(s);
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }
        dbg_log!("connecting");
        let connected = s.listener_connect().await;
        drop(s);
        let mut relay = match connected {
            Ok(r) => r,
            Err(e) => {
                dbg_log!("connect failed: {e:#}");
                if online != Some(false) {
                    online = Some(false);
                    let _ = app.emit(
                        "relay-status",
                        json!({ "online": false, "error": format!("{e:#}") }),
                    );
                }
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        dbg_log!("connected, waiting");
        if online != Some(true) {
            online = Some(true);
            let _ = app.emit("relay-status", json!({ "online": true }));
        }
        if let Some(s) = weak.upgrade() {
            s.listener_subscribe(&mut relay).await;
        }
        let changed = weak.upgrade().map(|s| s.subscription_signal());
        // Wait on this connection. The idle window is only a heartbeat: nothing arrived, keep waiting.
        let connected_at = std::time::Instant::now();
        let mut resubscribe = false;
        let woke = loop {
            if weak.strong_count() == 0 {
                return;
            }
            let signal = async {
                match &changed {
                    Some(c) => c.notified().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                r = relay.next_incoming(Some(Duration::from_secs(50))) => match r {
                    Ok(Some(_)) => break true,
                    Ok(None) => continue,
                    // Displaced by an operation that connected, or the connection dropped.
                    Err(e) => {
                        dbg_log!("wait ended: {e:#}");
                        break false;
                    }
                },
                // We joined, left or moved a large group: reconnect to listen to the right mailboxes.
                _ = signal => {
                    dbg_log!("large groups changed; listening again");
                    resubscribe = true;
                    break false;
                }
            }
        };
        drop(relay);
        if resubscribe {
            retry = Duration::from_millis(200);
            continue;
        }
        if !woke {
            // Displaced by one of our own operations (they all connect, and the relay keeps one
            // connection per device): that is normal, not a failing relay, so come straight back.
            // Only a connection that does not even last a few seconds counts as trouble and backs off.
            if connected_at.elapsed() > Duration::from_secs(3) {
                retry = Duration::from_millis(200);
            }
            tokio::time::sleep(retry).await;
            retry = (retry * 2).min(Duration::from_secs(30));
            continue;
        }
        retry = Duration::from_millis(200);
        dbg_log!("woken by the relay; syncing");
        let Some(s) = weak.upgrade() else { return };
        if s.paused() {
            continue;
        }
        // Tell the screen as soon as each item is stored, not after the sync's idle wait.
        let mut flashed = false;
        let result = s
            .sync_with(Duration::from_millis(700), |r, attention| {
                if !flashed {
                    flashed = true;
                    if let Some(w) = app.get_webview_window("main") {
                        if !w.is_focused().unwrap_or(true) {
                            let _ = w.request_user_attention(Some(
                                tauri::UserAttentionType::Informational,
                            ));
                        }
                    }
                }
                set_title(&app, attention);
                let _ = app.emit("synced", r.clone());
            })
            .await;
        if let Ok(r) = result {
            let attention = s.attention().unwrap_or(0);
            drop(s);
            set_title(&app, attention);
            let _ = app.emit("synced", r);
        }
        // The sync's own connection replaced ours: pause briefly so two connections do not trade places in a loop.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[tauri::command]
fn direct_status(state: State<'_, AppState>) -> Result<DirectDto, String> {
    session(&state)?.direct_status().map_err(err)
}

#[tauri::command]
async fn direct_enable(state: State<'_, AppState>) -> Result<DirectDto, String> {
    session(&state)?.direct_enable().await.map_err(err)
}

#[tauri::command]
fn direct_disable(state: State<'_, AppState>) -> Result<DirectDto, String> {
    session(&state)?.direct_disable().map_err(err)
}

#[tauri::command]
fn direct_set_address(
    state: State<'_, AppState>,
    name: String,
    address: String,
) -> Result<(), String> {
    session(&state)?
        .direct_set_address(&name, &address)
        .map_err(err)
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

#[tauri::command]
fn set_relay_ca(state: State<'_, AppState>, pem: Option<String>) -> Result<usize, String> {
    session(&state)?.set_relay_ca(pem).map_err(err)
}

#[tauri::command]
fn set_allow_insecure(state: State<'_, AppState>, allow: bool) -> Result<(), String> {
    session(&state)?.set_allow_insecure(allow).map_err(err)
}

#[tauri::command]
async fn test_relay(state: State<'_, AppState>) -> Result<String, String> {
    session(&state)?.test_relay().await.map_err(err)
}

#[tauri::command]
async fn edit_message(state: State<'_, AppState>, id: String, text: String) -> Result<(), String> {
    session(&state)?.edit(&id, &text).await.map_err(err)
}

#[tauri::command]
async fn delete_message(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session(&state)?.delete(&id).await.map_err(err)
}

#[tauri::command]
async fn remove_device(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session(&state)?.remove_device(&id).await.map_err(err)
}

#[tauri::command]
fn message_requests(state: State<'_, AppState>) -> Result<Vec<MessageRequestDto>, String> {
    session(&state)?.message_requests().map_err(err)
}

#[tauri::command]
fn accept_request(state: State<'_, AppState>, id: String, name: String) -> Result<usize, String> {
    session(&state)?.accept_request(&id, &name).map_err(err)
}

#[tauri::command]
fn decline_request(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session(&state)?.decline_request(&id).map_err(err)
}

#[tauri::command]
fn block_request(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session(&state)?.block_request(&id).map_err(err)
}

#[tauri::command]
fn blocked(state: State<'_, AppState>) -> Result<Vec<BlockedDto>, String> {
    session(&state)?.blocked().map_err(err)
}

#[tauri::command]
fn unblock(state: State<'_, AppState>, owner: String) -> Result<(), String> {
    session(&state)?.unblock(&owner).map_err(err)
}

#[tauri::command]
fn group(state: State<'_, AppState>, id: String) -> Result<GroupDetailDto, String> {
    session(&state)?.group(&id).map_err(err)
}

#[tauri::command]
fn group_conversation(state: State<'_, AppState>, id: String) -> Result<Vec<MessageDto>, String> {
    session(&state)?.group_conversation(&id, 200).map_err(err)
}

#[tauri::command]
async fn group_create(
    state: State<'_, AppState>,
    name: String,
    members: Vec<String>,
    any_can_add: bool,
    history_week: bool,
) -> Result<GroupSendDto, String> {
    session(&state)?
        .group_create_with(&name, &members, any_can_add, history_week)
        .await
        .map_err(err)
}

#[tauri::command]
async fn group_create_large(
    state: State<'_, AppState>,
    name: String,
    members: Vec<String>,
) -> Result<GroupSendDto, String> {
    session(&state)?
        .group_create_large(&name, &members)
        .await
        .map_err(err)
}

#[tauri::command]
async fn group_set_history(
    state: State<'_, AppState>,
    id: String,
    week: bool,
) -> Result<GroupSendDto, String> {
    session(&state)?
        .group_set_history(&id, week)
        .await
        .map_err(err)
}

/// A file for a group: its bytes are the raw request body; the group, file name and caption are headers.
#[tauri::command]
async fn group_send_file(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> Result<GroupSendDto, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the file's bytes".into());
    };
    let (id, file, caption) = (
        header(&request, "x-group"),
        header(&request, "x-filename"),
        header(&request, "x-caption"),
    );
    session(&state)?
        .group_send_file(&id, &file, bytes, &caption)
        .await
        .map_err(err)
}

#[tauri::command]
async fn group_send(
    state: State<'_, AppState>,
    id: String,
    text: String,
) -> Result<GroupSendDto, String> {
    session(&state)?.group_send(&id, &text).await.map_err(err)
}

#[tauri::command]
async fn group_add(
    state: State<'_, AppState>,
    id: String,
    names: Vec<String>,
) -> Result<GroupSendDto, String> {
    session(&state)?.group_add(&id, &names).await.map_err(err)
}

#[tauri::command]
async fn group_remove(
    state: State<'_, AppState>,
    id: String,
    owner: String,
) -> Result<GroupSendDto, String> {
    session(&state)?
        .group_remove(&id, &owner)
        .await
        .map_err(err)
}

#[tauri::command]
async fn group_make_admin(
    state: State<'_, AppState>,
    id: String,
    owner: String,
) -> Result<GroupSendDto, String> {
    session(&state)?
        .group_make_admin(&id, &owner)
        .await
        .map_err(err)
}

#[tauri::command]
async fn group_rename(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<GroupSendDto, String> {
    session(&state)?.group_rename(&id, &name).await.map_err(err)
}

#[tauri::command]
async fn group_set_add_by(
    state: State<'_, AppState>,
    id: String,
    any: bool,
) -> Result<GroupSendDto, String> {
    session(&state)?
        .group_set_add_by(&id, any)
        .await
        .map_err(err)
}

#[tauri::command]
async fn group_leave(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session(&state)?.group_leave(&id).await.map_err(err)
}

#[tauri::command]
async fn group_accept(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session(&state)?.group_accept(&id).await.map_err(err)
}

#[tauri::command]
async fn group_decline(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session(&state)?.group_decline(&id).await.map_err(err)
}

#[tauri::command]
async fn group_block(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session(&state)?.group_block(&id).await.map_err(err)
}

#[tauri::command]
async fn group_mark_read(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session(&state)?.group_mark_read(&id).await.map_err(err)
}

#[tauri::command]
fn add_member_as_contact(
    state: State<'_, AppState>,
    owner: String,
    name: String,
) -> Result<String, String> {
    session(&state)?
        .add_member_as_contact(&owner, &name)
        .map_err(err)
}

#[tauri::command]
fn action_types() -> Vec<ActionTypeDto> {
    api::action_types()
}

#[tauri::command]
fn grant(state: State<'_, AppState>, request: GrantRequest) -> Result<(), String> {
    session(&state)?.grant(request).map_err(err)
}

#[tauri::command]
fn revoke(state: State<'_, AppState>, name: String) -> Result<bool, String> {
    session(&state)?.revoke(&name).map_err(err)
}

/// Decode `%XX` escapes (the UI percent-encodes header values so any name or
/// caption survives). Malformed escapes are kept as they are; never panics.
pub fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| {
                std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .flatten();
        match hex {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn header(request: &tauri::ipc::Request<'_>, key: &str) -> String {
    percent_decode(
        request
            .headers()
            .get(key)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    )
}

/// The file's bytes are the raw request body; the contact, file name and caption are headers.
#[tauri::command]
async fn send_file(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> Result<usize, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the file's bytes".into());
    };
    let (name, file, caption) = (
        header(&request, "x-contact"),
        header(&request, "x-filename"),
        header(&request, "x-caption"),
    );
    session(&state)?
        .send_file(&name, &file, bytes, &caption)
        .await
        .map_err(err)
}

/// Where received files are saved: the Downloads folder (or `SPIKE_DESKTOP_DOWNLOADS`).
fn downloads(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    match std::env::var_os("SPIKE_DESKTOP_DOWNLOADS") {
        Some(d) => Ok(PathBuf::from(d)),
        None => app.path().download_dir().map_err(|e| e.to_string()),
    }
}

/// `name`, `name (2)`, `name (3)`... whichever does not exist yet; never overwrites.
fn write_unique(dir: &std::path::Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for n in 1..1000 {
        let candidate = dir.join(if n == 1 {
            name.to_string()
        } else {
            format!("{stem} ({n}){ext}")
        });
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut f) => {
                f.write_all(bytes).map_err(|e| e.to_string())?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("too many files with that name in the folder".into())
}

/// Fetch (and check) a message's file, then save it to Downloads. Returns the path.
#[tauri::command]
async fn save_attachment(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<String, String> {
    let (name, bytes) = session(&state)?.attachment(&id).await.map_err(err)?;
    let dir = downloads(&app)?;
    write_unique(&dir, &name, &bytes).map(|p| p.display().to_string())
}

// ---- backup and recovery (ADR 0029)

#[tauri::command]
fn backup_new_key() -> String {
    api::backup::generate_key()
}

#[tauri::command]
fn backup_status(state: State<'_, AppState>) -> Result<api::backup::BackupStatusDto, String> {
    session(&state)?.backup_status().map_err(err)
}

/// Seal a backup with its own passphrase and save it to Downloads. Returns the path.
#[tauri::command]
async fn backup_export(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    passphrase: String,
    history: bool,
) -> Result<String, String> {
    let s = session(&state)?;
    // Argon2 at the backup's cost takes a moment: keep it off the UI thread.
    let bytes = tokio::task::spawn_blocking(move || s.backup_export(&passphrase, history))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)?;
    write_unique(
        &downloads(&app)?,
        "spike-identity-backup.spikebackup",
        &bytes,
    )
    .map(|p| p.display().to_string())
}

/// The file's bytes are the raw request body; its passphrase is a header.
#[tauri::command]
async fn backup_open(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> Result<api::backup::BackupSummaryDto, String> {
    if api::exists(&state.dir) {
        return Err("this device already has an identity".into());
    }
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the file's bytes".into());
    };
    let (bytes, passphrase) = (bytes.clone(), header(&request, "x-passphrase"));
    let (opened, summary) =
        tokio::task::spawn_blocking(move || api::backup::backup_open(&bytes, &passphrase))
            .await
            .map_err(|e| e.to_string())?
            .map_err(err)?;
    *state
        .backup
        .lock()
        .map_err(|_| "internal error".to_string())? = Some(opened);
    Ok(summary)
}

#[tauri::command]
async fn backup_restore(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    passphrase: String,
    relay: Option<String>,
) -> Result<(), String> {
    let opened = state
        .backup
        .lock()
        .map_err(|_| "internal error".to_string())?
        .take()
        .ok_or("open the backup file first")?;
    let s =
        api::backup::backup_restore(opened, &state.dir, passphrase, KdfParams::default(), relay)
            .await
            .map_err(err)?;
    set(&state, Some(s));
    start_listener(&app, &state);
    Ok(())
}

#[tauri::command]
fn backup_cancel(state: State<'_, AppState>) {
    if let Ok(mut b) = state.backup.lock() {
        *b = None;
    }
}

#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> Result<updates::UpdateInfo, String> {
    updates::check(&app).await
}

#[tauri::command]
async fn update_install(app: tauri::AppHandle) -> Result<(), String> {
    updates::install(&app).await
}

#[tauri::command]
fn qr_svg(text: String) -> Result<String, String> {
    api::qr_svg(&text).map_err(err)
}

#[tauri::command]
fn link_offer(state: State<'_, AppState>) -> Result<String, String> {
    session(&state)?.link_offer().map_err(err)
}

#[tauri::command]
async fn link_wait(state: State<'_, AppState>) -> Result<String, String> {
    session(&state)?
        .link_wait(Duration::from_secs(90))
        .await
        .map_err(err)
}

#[tauri::command]
async fn link_receive(state: State<'_, AppState>) -> Result<InstalledDto, String> {
    session(&state)?
        .link_receive(Duration::from_secs(120))
        .await
        .map_err(err)
}

#[tauri::command]
fn link_cancel(state: State<'_, AppState>) -> Result<(), String> {
    session(&state)?.link_cancel();
    Ok(())
}

#[tauri::command]
async fn link_begin(state: State<'_, AppState>, code: String) -> Result<String, String> {
    session(&state)?.link_begin(&code).await.map_err(err)
}

#[tauri::command]
async fn link_finish(
    state: State<'_, AppState>,
    history: String,
    days: u64,
) -> Result<LinkedDto, String> {
    session(&state)?
        .link_finish(&history, days)
        .await
        .map_err(err)
}

pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(feature = "updater")]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    builder
        .setup(|app| {
            // SPIKE_DESKTOP_HOME lets you run a second instance (another identity) on one machine.
            let dir = match std::env::var_os("SPIKE_DESKTOP_HOME") {
                Some(d) => PathBuf::from(d),
                None => app.path().app_data_dir()?,
            };
            app.manage(AppState {
                dir,
                session: Mutex::new(None),
                listener: Mutex::new(None),
                backup: Mutex::new(None),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_status,
            direct_status,
            direct_enable,
            direct_disable,
            direct_set_address,
            create_identity,
            backup_new_key,
            backup_status,
            backup_export,
            backup_open,
            backup_restore,
            backup_cancel,
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
            action_types,
            message_requests,
            accept_request,
            decline_request,
            block_request,
            blocked,
            unblock,
            set_relay_ca,
            set_allow_insecure,
            test_relay,
            edit_message,
            delete_message,
            remove_device,
            set_attention,
            grant,
            revoke,
            send_file,
            save_attachment,
            link_offer,
            qr_svg,
            update_check,
            update_install,
            link_wait,
            link_receive,
            link_cancel,
            link_begin,
            link_finish,
            group,
            group_conversation,
            group_create,
            group_create_large,
            group_set_history,
            group_send_file,
            group_send,
            group_add,
            group_remove,
            group_make_admin,
            group_rename,
            group_set_add_by,
            group_leave,
            group_accept,
            group_decline,
            group_block,
            group_mark_read,
            add_member_as_contact,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Spike Connect");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding_handles_unicode_and_garbage() {
        assert_eq!(percent_decode("report.pdf"), "report.pdf");
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(
            percent_decode("%E0%B8%AA%E0%B8%A7%E0%B8%B1%E0%B8%AA%E0%B8%94%E0%B8%B5"),
            "สวัสดี"
        );
        // Truncated, non-hex and multi-byte after the percent sign are kept, not panicked on.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%é"), "%é");
        assert_eq!(percent_decode("%4"), "%4");
    }

    #[test]
    fn saved_files_never_overwrite_each_other() {
        let dir = std::env::temp_dir().join(format!("spike-desktop-dl-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let a = write_unique(&dir, "report.pdf", b"one").unwrap();
        let b = write_unique(&dir, "report.pdf", b"two").unwrap();
        let c = write_unique(&dir, "noext", b"three").unwrap();
        let d = write_unique(&dir, "noext", b"four").unwrap();
        let names: Vec<String> = [a, b, c, d]
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["report.pdf", "report (2).pdf", "noext", "noext (2)"]
        );
        assert_eq!(std::fs::read(dir.join("report.pdf")).unwrap(), b"one");
        std::fs::remove_dir_all(&dir).ok();
    }
}
