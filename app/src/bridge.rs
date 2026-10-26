//! A JSON command bridge over [`Session`], for apps that are not written in Rust (the Android app).
//!
//! The desktop app calls `Session` through Tauri commands; this is the same set of commands behind one
//! function, `Engine::call(command, json_args) -> json`, so a Kotlin (or any) front end needs only
//! strings in and out. Keys never cross it: only names, texts and the DTOs the desktop screens use.
//! The answer is `{"ok": value}` or `{"err": "message"}`; it never panics across the boundary.

use crate::{exists, Session};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use spike_core::vault::KdfParams;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One app instance: where its state lives, the unlocked session if any, and the runtime that runs
/// the session's network calls.
pub struct Engine {
    dir: PathBuf,
    kdf: KdfParams,
    session: Mutex<Option<Arc<Session>>>,
    /// A backup file the person opened on the create screen and has not yet restored (ADR 0029).
    backup: Mutex<Option<crate::backup::Opened>>,
    rt: tokio::runtime::Runtime,
}

fn s(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("missing text argument '{key}'"))
}

fn b(args: &Value, key: &str) -> Result<bool> {
    args.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("missing yes/no argument '{key}'"))
}

fn list(args: &Value, key: &str) -> Result<Vec<String>> {
    args.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing list argument '{key}'"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("'{key}' must be a list of text"))
        })
        .collect()
}

fn val<T: Serialize>(v: T) -> Result<Value> {
    Ok(serde_json::to_value(v)?)
}

impl Engine {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        Self::with_kdf(dir, KdfParams::default())
    }

    /// Like [`Engine::new`], with the passphrase-hardening cost chosen (tests use a cheap one).
    pub fn with_kdf(dir: impl Into<PathBuf>, kdf: KdfParams) -> Result<Self> {
        Ok(Self {
            dir: dir.into(),
            kdf,
            session: Mutex::new(None),
            backup: Mutex::new(None),
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .context("starting the runtime")?,
        })
    }

    fn session(&self) -> Result<Arc<Session>> {
        self.session
            .lock()
            .map_err(|_| anyhow!("internal error"))?
            .clone()
            .ok_or_else(|| anyhow!("locked"))
    }

    /// Start listening for direct connections if the person turned them on. A failure (the port is
    /// taken) is not fatal: the relay carries everything, and the Settings screen shows the state.
    fn resume_direct(&self) {
        if let Ok(x) = self.session() {
            if let Err(e) = self.rt.block_on(x.direct_resume()) {
                eprintln!("direct connections could not start: {e:#}");
            }
        }
    }

    fn set(&self, session: Option<Session>) {
        *self.session.lock().expect("session lock") = session.map(Arc::new);
    }

    /// Run one command. Blocking: call it from a background thread.
    pub fn call(&self, command: &str, args_json: &str) -> String {
        let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let args: Value = if args_json.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(args_json)
                    .map_err(|e| anyhow!("arguments are not JSON: {e}"))?
            };
            self.dispatch(command, &args)
        }));
        match answer {
            Ok(Ok(v)) => json!({ "ok": v }),
            Ok(Err(e)) => json!({ "err": format!("{e:#}") }),
            Err(_) => json!({ "err": "internal error" }),
        }
        .to_string()
    }

    fn dispatch(&self, cmd: &str, a: &Value) -> Result<Value> {
        // Commands that need no unlocked session.
        match cmd {
            "app_status" => {
                return Ok(json!({
                    "exists": exists(&self.dir),
                    "unlocked": self.session.lock().map(|x| x.is_some()).unwrap_or(false),
                }))
            }
            "create_identity" => {
                let session = Session::create(&self.dir, s(a, "passphrase")?, self.kdf)?;
                self.set(Some(session));
                return Ok(Value::Null);
            }
            "unlock" => {
                let session = Session::unlock(&self.dir, s(a, "passphrase")?)?;
                self.set(Some(session));
                self.resume_direct();
                return Ok(Value::Null);
            }
            "lock" => {
                self.set(None);
                return Ok(Value::Null);
            }
            // Recovery (ADR 0029): a fresh install opens a backup file, sees what is in it, then
            // restores it as a new device.
            "backup_new_key" => return val(crate::backup::generate_key()),
            "backup_open" => {
                if exists(&self.dir) {
                    bail!("this device already has an identity");
                }
                let bytes = read_file(&s(a, "path")?)?;
                let (opened, summary) = crate::backup::backup_open(&bytes, &s(a, "passphrase")?)?;
                *self.backup.lock().map_err(|_| anyhow!("internal error"))? = Some(opened);
                return val(summary);
            }
            "backup_restore" => {
                let opened = self
                    .backup
                    .lock()
                    .map_err(|_| anyhow!("internal error"))?
                    .take()
                    .ok_or_else(|| anyhow!("open the backup file first"))?;
                let relay = a.get("relay").and_then(Value::as_str).map(str::to_string);
                let session = self.rt.block_on(crate::backup::backup_restore(
                    opened,
                    &self.dir,
                    s(a, "passphrase")?,
                    self.kdf,
                    relay,
                ))?;
                self.set(Some(session));
                self.resume_direct();
                return Ok(Value::Null);
            }
            "backup_cancel" => {
                *self.backup.lock().map_err(|_| anyhow!("internal error"))? = None;
                return Ok(Value::Null);
            }
            _ => {}
        }
        let x = self.session()?;
        let rt = &self.rt;
        match cmd {
            "overview" => val(x.overview()?),
            "conversation" => val(x.conversation(&s(a, "name")?, 200)?),
            "send" => val(rt.block_on(x.send_reply(
                &s(a, "name")?,
                &s(a, "text")?,
                a.get("reply_to").and_then(Value::as_str),
            ))?),
            "react" => val(rt.block_on(x.react(&s(a, "id")?, &s(a, "emoji")?))?),
            "sync" => val(rt.block_on(x.sync(Duration::from_millis(700)))?),
            "mark_read" => val(rt.block_on(x.mark_read(&s(a, "name")?))?),
            "add_contact" => val(x.add_contact(&s(a, "name")?, &s(a, "card")?)?),
            "mark_verified" => val(x.mark_verified(&s(a, "name")?, &s(a, "fingerprint")?)?),
            "set_relay" => val(x.set_relay(&s(a, "url")?)?),
            "set_allow_insecure" => val(x.set_allow_insecure(b(a, "allow")?)?),
            "test_relay" => val(rt.block_on(x.test_relay())?),
            "edit_message" => val(rt.block_on(x.edit(&s(a, "id")?, &s(a, "text")?))?),
            "delete_message" => val(rt.block_on(x.delete(&s(a, "id")?))?),
            "message_requests" => val(x.message_requests()?),
            "accept_request" => val(x.accept_request(&s(a, "id")?, &s(a, "name")?)?),
            "decline_request" => val(x.decline_request(&s(a, "id")?)?),
            "block_request" => val(x.block_request(&s(a, "id")?)?),
            "blocked" => val(x.blocked()?),
            "unblock" => val(x.unblock(&s(a, "owner")?)?),
            "apps" => val(x.apps()?),
            "app_register" => val(x.app_register(&s(a, "app")?, &s(a, "name")?)?),
            "app_remove" => val(x.app_remove(&s(a, "app")?)?),
            "group" => val(x.group(&s(a, "id")?)?),
            "group_conversation" => val(x.group_conversation(&s(a, "id")?, 200)?),
            "group_create" => val(rt.block_on(x.group_create_with(
                &s(a, "name")?,
                &list(a, "members")?,
                b(a, "any_can_add").unwrap_or(false),
                b(a, "history_week").unwrap_or(false),
            ))?),
            "group_create_large" => {
                val(rt.block_on(x.group_create_large(&s(a, "name")?, &list(a, "members")?))?)
            }
            "group_send" => val(rt.block_on(x.group_send_reply(
                &s(a, "id")?,
                &s(a, "text")?,
                a.get("reply_to").and_then(Value::as_str),
            ))?),
            "group_add" => val(rt.block_on(x.group_add(&s(a, "id")?, &list(a, "names")?))?),
            "group_remove" => val(rt.block_on(x.group_remove(&s(a, "id")?, &s(a, "owner")?))?),
            "group_make_admin" => {
                val(rt.block_on(x.group_make_admin(&s(a, "id")?, &s(a, "owner")?))?)
            }
            "group_rename" => val(rt.block_on(x.group_rename(&s(a, "id")?, &s(a, "name")?))?),
            "group_set_add_by" => val(rt.block_on(x.group_set_add_by(&s(a, "id")?, b(a, "any")?))?),
            "group_leave" => val(rt.block_on(x.group_leave(&s(a, "id")?))?),
            "group_accept" => val(rt.block_on(x.group_accept(&s(a, "id")?))?),
            "group_decline" => val(rt.block_on(x.group_decline(&s(a, "id")?))?),
            "group_mark_read" => val(rt.block_on(x.group_mark_read(&s(a, "id")?))?),
            "add_member_as_contact" => {
                val(x.add_member_as_contact(&s(a, "owner")?, &s(a, "name")?)?)
            }
            "qr_svg" => val(crate::qr_svg(&s(a, "text")?)?),
            // The QR code as a grid of dark/light squares, for drawing natively: `{size, rows}` where
            // each row is a string of 0 (light) and 1 (dark).
            "qr_matrix" => {
                use qrcode::{Color, EcLevel, QrCode};
                let code =
                    QrCode::with_error_correction_level(s(a, "text")?.as_bytes(), EcLevel::L)
                        .map_err(|e| anyhow!("that does not fit in a QR code: {e}"))?;
                let size = code.width();
                let rows: Vec<String> = code
                    .to_colors()
                    .chunks(size)
                    .map(|r| {
                        r.iter()
                            .map(|c| if *c == Color::Dark { '1' } else { '0' })
                            .collect()
                    })
                    .collect();
                Ok(json!({ "size": size, "rows": rows }))
            }
            // Direct connections on the local network (ADR 0027).
            "direct_status" => val(x.direct_status()?),
            "direct_enable" => val(rt.block_on(x.direct_enable())?),
            "direct_disable" => val(x.direct_disable()?),
            "direct_set_address" => {
                val(x.direct_set_address(&s(a, "name")?, &s(a, "address").unwrap_or_default())?)
            }
            // Wait up to `ms` for a message that arrived directly; the sync totals, or null.
            "direct_wait" => {
                let ms = a
                    .get("ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(20_000)
                    .min(120_000);
                val(rt.block_on(x.direct_wait(ms)))
            }
            // Calls (ADR 0028). The screen makes the WebRTC offer/answer and hands the text up.
            "call_start" => val(rt.block_on(x.call_start(
                &s(a, "name")?,
                b(a, "video").unwrap_or(false),
                &s(a, "sdp")?,
            ))?),
            "push_register" => val(rt.block_on(x.push_register(&s(a, "url")?))?),
            "push_unregister" => val(rt.block_on(x.push_unregister())?),
            "call_ice_servers" => val(rt.block_on(x.call_ice_servers())),
            "call_restart" => val(rt.block_on(x.call_restart(&s(a, "sdp")?))?),
            "call_restart_answer" => val(rt.block_on(x.call_restart_answer(&s(a, "sdp")?))?),
            "call_restart_ask" => val(rt.block_on(x.call_restart_ask())?),
            "call_accept" => val(rt.block_on(x.call_accept(&s(a, "sdp")?))?),
            "call_decline" => val(rt.block_on(x.call_decline(b(a, "all").unwrap_or(false)))?),
            "call_hangup" => val(rt.block_on(x.call_hangup())?),
            "call_fail" => val(rt.block_on(x.call_fail())?),
            "call_ice" => val(rt.block_on(x.call_local_ice(list(a, "candidates")?))?),
            // Wait up to `ms` for something to act on: ring, connect, remote candidates, end.
            "call_wait" => {
                let ms = a
                    .get("ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(20_000)
                    .min(120_000);
                val(rt.block_on(x.call_wait(ms)))
            }
            "backup_status" => val(x.backup_status()?),
            // Writes the sealed backup to `path`; the app then hands that file to the person.
            "backup_export" => {
                let bytes =
                    x.backup_export(&s(a, "passphrase")?, b(a, "history").unwrap_or(false))?;
                let path = PathBuf::from(s(a, "path")?);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, bytes).context("saving the backup")?;
                val(path.display().to_string())
            }
            "agents" => val(x.agents()?),
            "decide" => val(rt.block_on(x.decide(&s(a, "id")?, b(a, "approve")?))?),
            "remove_device" => val(rt.block_on(x.remove_device(&s(a, "id")?))?),
            // Wait for the relay to say something is waiting (a message, or an entry in a large
            // group), up to `ms`. Returns whether it did. The caller then runs `sync`.
            "wait_for_push" => {
                let ms = a
                    .get("ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(25_000)
                    .min(120_000);
                val(rt.block_on(async {
                    let mut relay = x.listener_connect().await?;
                    x.listener_subscribe(&mut relay).await;
                    match relay.next_incoming(Some(Duration::from_millis(ms))).await {
                        Ok(Some(_)) => Ok::<bool, anyhow::Error>(true),
                        Ok(None) => Ok(false),
                        Err(e) => Err(e),
                    }
                })?)
            }
            // Like `wait_for_push`, but a one-to-one delivery that arrives is stored and
            // acknowledged right here on the open connection (ADR 0030, the fast path). Returns
            // `{pushed, sync}`: `sync` is the result when it was handled, null when the caller
            // should run `sync` (a group entry, a failure, linking in progress, nothing arrived).
            "listen_once" => {
                let ms = a
                    .get("ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(25_000)
                    .min(120_000);
                val(rt.block_on(async {
                    let mut relay = x.listener_connect().await?;
                    x.listener_subscribe(&mut relay).await;
                    match relay.next_incoming(Some(Duration::from_millis(ms))).await {
                        Ok(Some(spike_cli::client::Incoming::Delivery(d))) if !x.paused() => {
                            let sync = x.listener_receive(&mut relay, &d).await.ok();
                            Ok::<Value, anyhow::Error>(serde_json::json!({"pushed": true, "sync": sync}))
                        }
                        Ok(Some(_)) => Ok(serde_json::json!({"pushed": true, "sync": null})),
                        Ok(None) => Ok(serde_json::json!({"pushed": false, "sync": null})),
                        Err(e) => Err(e),
                    }
                })?)
            }
            // Files travel by path: the app copies what the person picked into its own cache, the
            // engine reads it (and `save_attachment` writes into a folder the app names).
            "send_file" => {
                let bytes = read_file(&s(a, "path")?)?;
                let cap = a.get("caption").and_then(Value::as_str).unwrap_or("");
                val(rt.block_on(x.send_file(&s(a, "name")?, &s(a, "file_name")?, &bytes, cap))?)
            }
            "group_send_file" => {
                let bytes = read_file(&s(a, "path")?)?;
                let cap = a.get("caption").and_then(Value::as_str).unwrap_or("");
                val(rt.block_on(x.group_send_file(
                    &s(a, "id")?,
                    &s(a, "file_name")?,
                    &bytes,
                    cap,
                ))?)
            }
            "save_attachment" => {
                let (name, bytes) = rt.block_on(x.attachment(&s(a, "id")?))?;
                let dir = PathBuf::from(s(a, "dir")?);
                std::fs::create_dir_all(&dir)?;
                let path = unique_path(&dir, &name)?;
                std::fs::write(&path, bytes).context("saving the file")?;
                val(path.display().to_string())
            }
            // Linking a phone: the new device offers a code, the existing one enters it; both then
            // show the same safety code.
            "link_offer" => val(x.link_offer()?),
            "link_wait" => val(rt.block_on(x.link_wait(Duration::from_secs(90)))?),
            "link_receive" => val(rt.block_on(x.link_receive(Duration::from_secs(120)))?),
            "link_cancel" => {
                x.link_cancel();
                Ok(Value::Null)
            }
            "link_begin" => val(rt.block_on(x.link_begin(&s(a, "code")?))?),
            "link_finish" => {
                let days = a.get("days").and_then(Value::as_u64).unwrap_or(30);
                val(rt.block_on(x.link_finish(&s(a, "history")?, days))?)
            }
            other => bail!("unknown command '{other}'"),
        }
    }
}

fn read_file(path: &str) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| "reading the chosen file")
}

/// `dir/name`, or `dir/name (2)`, ... so a saved file never overwrites another.
fn unique_path(dir: &std::path::Path, name: &str) -> Result<PathBuf> {
    let first = dir.join(name);
    if !first.exists() {
        return Ok(first);
    }
    let p = std::path::Path::new(name);
    let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or("file");
    let ext = p.extension().and_then(|x| x.to_str());
    for n in 2..1000 {
        let f = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let c = dir.join(f);
        if !c.exists() {
            return Ok(c);
        }
    }
    bail!("too many files with that name in the folder")
}
