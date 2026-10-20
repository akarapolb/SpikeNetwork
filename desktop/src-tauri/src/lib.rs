//! Spike Connect desktop (Tauri 2). See `docs/adr/0019-tauri-desktop-shell.md`.
//!
//! The commands here are thin wrappers over [`api::Session`]; the webview
//! never sees a key. Only the unlocked session is held in memory, and only
//! while the app is unlocked.

pub mod api;

use api::{
    ActionTypeDto, AgentsDto, BlockedDto, GrantRequest, InstalledDto, LinkedDto, MessageDto,
    MessageRequestDto, Overview, Session, SyncDto,
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
    let handle = tauri::async_runtime::spawn(listen(app.clone(), Arc::downgrade(&session)));
    *state.listener.lock().expect("listener lock") = Some(handle);
}

/// Instant delivery. Holds one relay connection and waits for the relay to push something. It never
/// acknowledges: it only wakes a normal sync (which does the decrypting and acknowledging), so the
/// ratchet state is never touched from two places. Holds no strong reference while waiting.
/// Debug builds log what the listener does (never content); release builds are silent.
macro_rules! dbg_log {
    ($($t:tt)*) => { #[cfg(debug_assertions)] eprintln!("[listener] {}", format!($($t)*)); };
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
        // Wait on this connection. The idle window is only a heartbeat: nothing arrived, keep waiting.
        let woke = loop {
            if weak.strong_count() == 0 {
                return;
            }
            match relay.next_delivery(Some(Duration::from_secs(50))).await {
                Ok(Some(_)) => break true,
                Ok(None) => continue,
                // Displaced by an operation that connected, or the connection dropped.
                Err(e) => {
                    dbg_log!("wait ended: {e:#}");
                    break false;
                }
            }
        };
        drop(relay);
        if !woke {
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
        if let Ok(r) = s.sync(Duration::from_millis(700)).await {
            let attention = s.attention().unwrap_or(0);
            drop(s);
            if let Some(w) = app.get_webview_window("main") {
                if (r.received > 0 || r.agent_requests > 0 || r.message_requests > 0)
                    && !w.is_focused().unwrap_or(true)
                {
                    let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
                }
            }
            set_title(&app, attention);
            let _ = app.emit("synced", r);
        }
        // The sync's own connection replaced ours: pause briefly so two connections do not trade places in a loop.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
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
                listener: Mutex::new(None),
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
            link_wait,
            link_receive,
            link_cancel,
            link_begin,
            link_finish,
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
