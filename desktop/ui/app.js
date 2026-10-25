"use strict";
/* Spike Connect desktop UI. No framework, no build step, no remote content (see the CSP in tauri.conf.json).
   Every value from the backend is inserted as text (createTextNode), never as HTML.
   Keys never reach this file: the backend only returns names, message text and request summaries. */

// Shortcuts use the platform's own modifier: Cmd on a Mac, Ctrl elsewhere.
const MAC = /Mac|iPhone|iPad/.test(navigator.platform || "");
const MOD = MAC ? "⌘" : "Ctrl+";
const invoke = (cmd, args, options) => window.__TAURI__.core.invoke(cmd, args, options);
const $ = (id) => document.getElementById(id);
function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === "class") el.className = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, "");
    else el.setAttribute(k, v);
  }
  for (const kid of kids.flat(3)) {
    if (kid == null || kid === false) continue;
    el.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
  return el;
}
const PATHS = {
  chats: '<path d="M4 5h16v11H9l-5 4z"/>',
  agents: '<path d="M12 3l1.8 4.6L18.5 9l-4.7 1.4L12 15l-1.8-4.6L5.5 9l4.7-1.4z"/><path d="M19 15l.8 2 2 .8-2 .8-.8 2-.8-2-2-.8 2-.8z"/>',
  settings: '<path d="M4 7h10M18 7h2M4 17h4M12 17h8"/><circle cx="16" cy="7" r="2"/><circle cx="10" cy="17" r="2"/>',
  addperson: '<circle cx="9" cy="8" r="3.5"/><path d="M2.5 20c.8-3.5 3.4-5.5 6.5-5.5s5.7 2 6.5 5.5"/><path d="M19 8v6M16 11h6"/>',
  lock: '<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>',
  back: '<path d="M15 5l-7 7 7 7"/>',
  chev: '<path d="M9 5l7 7-7 7"/>',
  shield: '<path d="M12 3l7 3v6c0 4-3 7-7 9-4-2-7-5-7-9V6z"/><path d="M9 12l2 2 4-4"/>',
  send: '<path d="M4 12l16-8-6 16-2-7z"/>',
  file: '<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/>',
  devices: '<rect x="3" y="5" width="12" height="9" rx="1.5"/><path d="M1.5 17h15"/><rect x="17" y="8" width="5.5" height="12" rx="1.2"/>',
  clip: '<path d="M21 12l-8.5 8.5a5 5 0 0 1-7-7L14 5a3.5 3.5 0 0 1 5 5l-8.5 8.5a2 2 0 0 1-3-3L15 8"/>',
  moon: '<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>',
  group: '<circle cx="9" cy="8" r="3"/><circle cx="16.5" cy="9.5" r="2.5"/><path d="M3 19c.7-3 3-4.8 6-4.8s5.3 1.8 6 4.8"/><path d="M15 14.6c2.6.1 4.6 1.6 5.2 4.4"/>',
  newgroup: '<circle cx="8" cy="8" r="3"/><path d="M2.5 19c.7-3 3-4.8 5.5-4.8s4.8 1.8 5.5 4.8"/><circle cx="15" cy="9" r="2.3"/><path d="M19 13v6M16 16h6"/>',
};
function icon(name) { const s = document.createElementNS("http://www.w3.org/2000/svg", "svg"); s.setAttribute("viewBox", "0 0 24 24"); s.setAttribute("class", "i"); s.setAttribute("aria-hidden", "true"); s.innerHTML = PATHS[name]; return s; }
function toast(msg) { const t = $("toast"); t.textContent = msg; t.hidden = false; clearTimeout(toast.t); toast.t = setTimeout(() => (t.hidden = true), 4200); }
const MINOR = { JPY: 0, KRW: 0, KWD: 3, BHD: 3 };
function money(minor, cur) { const d = MINOR[cur] ?? 2; if (!d) return `${minor} ${cur}`; const u = 10 ** d; return `${Math.floor(minor / u)}.${String(minor % u).padStart(d, "0")} ${cur}`; }
const clock = (ts) => new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
const dayOf = (ts) => new Date(ts).toLocaleDateString([], { weekday: "short", day: "numeric", month: "short" });
const ini = (n) => (n.trim()[0] || "?").toUpperCase();
async function call(cmd, args) { try { return await invoke(cmd, args); } catch (e) { if (e === "locked") { gate("unlock"); } else toast(String(e)); throw e; } }

/* ---------- state ---------- */
const st = { seenNotes: new Map(), group: null, gdetail: null, verifyFor: null, newGroup: null, picked: [], picking: false, nameMember: null, reqs: [], blocked: [], showReq: {}, naming: null, editing: null, confirmDelete: null, linkNew: false, linking: false, linkStep: 0, linkCode: "", linkSafety: "", linkMode: "all", linkErr: "", grantForm: null, section: "chats", chat: null, screen: null, ov: null, msgs: [], agents: null, offline: false, verifyDone: null, addMsg: null, polling: null, busy: false };

/* ---------- gate ---------- */
async function gate(mode) {
  stop();
  $("app").hidden = true; $("gate").hidden = false;
  if (!mode) mode = (await invoke("app_status")).exists ? "unlock" : "create";
  const create = mode === "create";
  const pass = h("input", { id: "pass", type: "password", autocomplete: create ? "new-password" : "current-password", "aria-describedby": "passHelp" });
  const pass2 = create ? h("input", { id: "pass2", type: "password", autocomplete: "new-password", "aria-label": "Repeat the passphrase" }) : null;
  const err = h("div", { class: "err", role: "alert", hidden: true });
  const btn = h("button", { class: "btn primary big", type: "submit" }, create ? "Create my identity" : "Unlock");
  const submit = async (e) => {
    e.preventDefault(); err.hidden = true;
    if (create && pass.value !== pass2.value) { err.textContent = "The two passphrases differ."; err.hidden = false; return; }
    btn.disabled = true; btn.textContent = create ? "Creating…" : "Unlocking…";
    try { await invoke(create ? "create_identity" : "unlock", { passphrase: pass.value }); pass.value = ""; if (pass2) pass2.value = ""; await openApp(); }
    catch (x) { err.textContent = String(x).replace(/^.*wrong passphrase.*$/i, "Wrong passphrase."); err.hidden = false; btn.disabled = false; btn.textContent = create ? "Create my identity" : "Unlock"; pass.focus(); }
  };
  $("gate").replaceChildren(h("form", { class: "gatebox", onsubmit: submit },
    h("span", { class: "eyebrow" }, "Spike Connect"),
    h("h1", {}, create ? "Your identity lives on this computer" : "Welcome back"),
    h("p", { class: "lead" }, create ? "No phone number, email or account. You are a key that only your devices hold." : "Your keys and history on this computer are sealed with your passphrase."),
    h("label", { class: "lbl", for: "pass", style: "font-weight:600" }, create ? "Choose a passphrase" : "Passphrase"), h("div", { class: "field" }, pass),
    create ? [h("div", { class: "field" }, pass2), h("div", { class: "note", id: "passHelp" }, "10 characters or more. It encrypts your keys and history here. If you lose it and have no other device, this identity cannot be recovered.")] : h("div", { id: "passHelp", class: "sr" }, ""),
    err, btn,
    create ? h("label", { class: "toggle" }, h("input", { type: "checkbox", onchange: (e) => { st.linkNew = e.target.checked; } }), h("span", {}, "I already use Spike on another device: link this computer to it after setup")) : null,
    create ? h("button", { class: "btn", type: "button", onclick: () => restoreGate() }, "Restore from a backup file") : null,
    h("p", { class: "note" }, "Unreviewed prototype software: do not use it for real secrets yet.")));
  pass.focus();
}
async function openApp() { $("gate").hidden = true; $("gate").replaceChildren(); $("app").hidden = false; st.chat = null; st.group = null; st.screen = null; await refresh(); if (st.linkNew) { st.linkNew = false; st.section = "devices"; st.screen = "linknew"; render(); startLinkNew(); } else syncNow(); st.polling = setInterval(() => { if (!document.hidden) syncNow(); }, 60000); armAutoLock(); }
function stop() { clearInterval(st.polling); st.polling = null; clearTimeout(st.idleTimer); st.ov = st.agents = st.gdetail = null; st.msgs = []; st.editing = st.confirmDelete = null; }

/* The backend holds one connection to the relay and tells us the moment something arrives; the slow poll above is only a safety net. */
function listenToBackend() {
  const ev = window.__TAURI__ && window.__TAURI__.event;
  if (!ev) return;
  ev.listen("synced", async (e) => { if (!st.ov) return; st.offline = false; for (const n of (e.payload && e.payload.notes) || []) { const t = st.seenNotes.get(n); if (!t || Date.now() - t > 10000) toast(n); st.seenNotes.set(n, Date.now()); } await refresh(); await markOpenRead(); scrollEnd(); });
  ev.listen("relay-status", (e) => { if (!st.ov) return; const off = !e.payload.online; if (off !== st.offline) { st.offline = off; render(); } });
}

/* Auto-lock after a period without use. The setting lives in this window's storage; it is not a secret. */
const AUTOLOCK_CHOICES = [[5, "5 minutes"], [15, "15 minutes"], [30, "30 minutes"], [60, "1 hour"], [0, "Never"]];
function autoLockMinutes() { try { const v = localStorage.getItem("autolock"); return v === null ? 15 : Number(v); } catch (_) { return 15; } }
function armAutoLock() {
  clearTimeout(st.idleTimer);
  const m = autoLockMinutes();
  if (!m || $("app").hidden) return;
  st.idleTimer = setTimeout(() => { if (!$("app").hidden && !st.linking) lockNow(); else armAutoLock(); }, m * 60000);
}
["pointerdown", "keydown", "wheel", "touchstart"].forEach((t) => document.addEventListener(t, () => { if (!$("app").hidden) armAutoLock(); }, { passive: true }));

/* ---------- data ---------- */
async function refresh() {
  try {
    st.ov = await invoke("overview");
    st.agents = await invoke("agents");
    if (st.ov.requests) st.reqs = await invoke("message_requests"); else st.reqs = [];
    if (st.chat) st.msgs = await invoke("conversation", { name: st.chat });
    if (st.group) await loadGroup();
  } catch (e) { if (e === "locked") return gate("unlock"); toast(String(e)); }
  render();
  if (st.ov) invoke("set_attention", { count: unreadTotal() + invitesCount() + st.ov.waiting + st.ov.requests }).catch(() => {});
}
async function syncNow() {
  if (st.busy || st.linking) return; st.busy = true;
  try {
    const r = await invoke("sync");
    st.offline = false;
    r.notes.forEach(toast);
    if (r.received || r.agent_requests || r.groups || r.message_requests) await refresh(); else render();
    await markOpenRead();
  } catch (e) { if (e === "locked") return gate("unlock"); st.offline = true; render(); }
  finally { st.busy = false; }
}

/* ---------- shell ---------- */
function go(section) { st.section = section; st.screen = null; render(); }
function renderRail() {
  const unread = st.ov ? unreadTotal() + invitesCount() : 0, w = st.ov ? st.ov.waiting : 0;
  const keys = { chats: 1, agents: 2, devices: 3, settings: 4 };
  const item = (k, label, n, warn) => h("button", { type: "button", "aria-current": st.section === k ? "page" : null, "aria-label": n ? `${label}, ${n} ${warn ? "waiting" : "unread"}` : label, onclick: () => go(k) },
    icon(k), h("span", { class: "tip", "aria-hidden": "true" }, `${label}  ${MOD}${keys[k]}`), n ? h("span", { class: "badge" + (warn ? " warn" : ""), "aria-hidden": "true" }, n) : null);
  $("rail").replaceChildren(
    h("span", { class: "tile sm me", title: "This device", "aria-hidden": "true" }, icon("shield")),
    item("chats", "Chats", unread), item("agents", "Agents", w, true), item("devices", "Devices"), item("settings", "Settings"),
    h("span", { class: "spacer" }),
    h("button", { type: "button", "aria-label": "Switch light or dark", onclick: () => { const r = document.documentElement; const dark = matchMedia("(prefers-color-scheme: dark)").matches; r.dataset.theme = (r.dataset.theme || (dark ? "dark" : "light")) === "dark" ? "light" : "dark"; } }, icon("moon"), h("span", { class: "tip", "aria-hidden": "true" }, "Light or dark")),
    h("span", { class: "sep", "aria-hidden": "true" }),
    h("button", { type: "button", "aria-label": "Lock", onclick: lockNow }, icon("lock"), h("span", { class: "tip", "aria-hidden": "true" }, `Lock  ${MOD}L`)));
}
async function lockNow() { try { await invoke("lock"); } catch (_) {} gate("unlock"); }
function render() {
  if (!st.ov) return;
  renderRail();
  const S = SECTIONS[st.section], items = S.list ? S.list() : [];
  $("list").replaceChildren(...items);
  $("shell").classList.toggle("nolist", !items.length);
  const main = st.screen ? SCREENS[st.screen]() : S.main();
  const banners = [];
  if (st.ov.me.removed) banners.push(h("div", { class: "err-banner", role: "alert" }, h("b", {}, "This device was removed from your identity."), " It keeps what it already has but receives nothing new. Create a new identity here, or link it again from your main device."));
  if (st.offline) banners.push(h("div", { class: "err-banner", role: "status" }, "Cannot reach the relay at ", h("b", {}, st.ov.relay), ". Messages are kept and sent when it is back. Check Settings."));
  $("main").replaceChildren(...banners, ...main);
}
function header(title, back) { return h("div", { class: "head" }, back ? h("button", { class: "iconbtn", type: "button", "aria-label": "Back", onclick: back }, icon("back")) : null, h("h1", {}, title)); }
function tile(c, sm) { return h("span", { class: `tile${sm ? " sm" : ""}${c.agent ? " sq" : ""} ${tint(c.name)}`, "aria-hidden": "true" }, ini(c.name)); }
// A soft tint picked from the name, so people are easier to tell apart. Status colours stay red, amber and green.
function tint(name) { let n = 0; for (const ch of name || "") n = (n * 31 + ch.codePointAt(0)) >>> 0; return `c-t${n % 6}`; }
// An empty state: a quiet icon above the same sentence as before.
function emptyState(text, ic = "chats") { return h("div", { class: "empty" }, h("span", { class: "emptyic", "aria-hidden": "true" }, icon(ic)), h("p", {}, text)); }

/* ---------- chats ---------- */
function chatsList() {
  const cs = st.ov.contacts, gs = st.ov.groups;
  // One list, newest activity first; contacts without messages stay alphabetical after the active ones.
  const entries = [...cs.map((c) => ({ ts: c.last_ts || 0, name: c.name, row: contactRow(c) })), ...gs.map((g) => ({ ts: g.last_ts || 0, name: g.name, row: groupRow(g) }))]
    .sort((x, y) => y.ts - x.ts || x.name.localeCompare(y.name));
  return [h("div", { class: "listhead" }, h("div", { class: "title" }, h("h1", {}, "Chats"),
      h("button", { class: "iconbtn", type: "button", "aria-label": "New group", title: "New group", onclick: () => { if (!cs.length) return toast("Add a contact first: a group is made from your contacts."); st.newGroup = { name: "", members: [], anyCanAdd: false, week: false, large: false }; st.screen = "newgroup"; render(); } }, icon("newgroup")),
      h("button", { class: "iconbtn", type: "button", "aria-label": "Add a contact", title: "Add a contact", onclick: () => { st.screen = "add"; st.addMsg = null; render(); } }, icon("addperson")))),
    h("nav", { class: "rows", "aria-label": "Conversations" },
      st.ov.requests ? h("button", { class: "reqrow", type: "button", onclick: () => { st.screen = "requests"; st.naming = null; render(); } }, icon("addperson"),
        h("span", { style: "flex:1;font-size:14px;text-align:left" }, h("b", { style: "font-weight:600" }, `${st.ov.requests} message request${st.ov.requests > 1 ? "s" : ""}`), " ", h("span", { class: "muted" }, "from people not in your contacts")), icon("chev")) : null,
      entries.length ? entries.map((e) => e.row)
        : emptyState("No contacts yet. Add one with the button above: paste their identity card.", "addperson")),
  ];
}
function contactRow(c) {
  return h("button", { class: "row", type: "button", "aria-current": String(!st.screen && st.chat === c.name), onclick: () => openChat(c.name) }, tile(c),
    h("span", { class: "body" }, h("span", { class: "line1" }, h("b", {}, c.name), c.agent ? h("span", { class: "tag plain" }, "agent") : null, c.verified ? null : h("span", { class: "tag bad" }, "Not verified")), h("span", { class: "preview" }, c.last || "No messages yet")),
    h("span", { class: "meta" }, c.last_ts ? clock(c.last_ts) : "", c.unread ? h("span", { class: "count" }, c.unread) : null));
}
function ownActions(m) {
  const link = (label, fn) => h("button", { class: "linkish", type: "button", style: "border:0;background:none;padding:0 0 0 8px;color:var(--accent);font-size:11.5px;cursor:pointer", onclick: fn }, label);
  if (st.confirmDelete === m.id) return [" · Delete for everyone? ", link("Yes, delete", async () => { st.confirmDelete = null; try { await invoke("delete_message", { id: m.id }); await refresh(); } catch (e) { toast(String(e)); render(); } }), link("Cancel", () => { st.confirmDelete = null; render(); })];
  return [m.body ? link("Edit", () => { st.editing = m.id; st.confirmDelete = null; render(); $("main").querySelector(".editbox textarea")?.focus(); }) : null, link("Delete", () => { st.confirmDelete = m.id; render(); })];
}
function editBox(m) {
  const ta = h("textarea", { "aria-label": "Edit message", rows: "2", style: "width:100%;min-width:280px;border:1px solid var(--line);border-radius:10px;padding:8px;background:var(--panel);color:var(--text)" }, m.body);
  const save = async () => { const text = ta.value; if (!text.trim()) return toast("A message cannot be empty. Delete it instead."); try { await invoke("edit_message", { id: m.id, text }); st.editing = null; await refresh(); } catch (e) { toast(String(e)); } };
  ta.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); save(); } else if (e.key === "Escape") { st.editing = null; render(); } });
  return h("div", { class: "editbox", style: "display:flex;flex-direction:column;gap:6px;align-items:flex-end" }, ta, h("span", { style: "display:flex;gap:8px" }, h("button", { class: "btn", type: "button", onclick: () => { st.editing = null; render(); } }, "Cancel"), h("button", { class: "btn primary", type: "button", onclick: save }, "Save")));
}
const fmtSize = (n) => (n >= 1048576 ? `${(n / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);
const MAX_FILE = 100 * 1024 * 1024;
async function sendFile(name, file, caption) {
  if (file.size === 0) return toast("That file is empty.");
  if (file.size > MAX_FILE) return toast("Files are limited to 100 MB.");
  toast(`Encrypting and uploading ${file.name}…`);
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    const enc = encodeURIComponent;
    const n = await invoke("send_file", bytes, { headers: { "x-contact": enc(name), "x-filename": enc(file.name), "x-caption": enc(caption || "") } });
    const t = $("main").querySelector("textarea"); if (t) t.value = "";
    toast(n ? "File sent." : "Saved, but no device could be reached yet. It will be delivered when they connect.");
    await refresh(); scrollEnd();
  } catch (e) { toast(String(e)); }
}
async function saveFile(m) {
  toast("Fetching and checking the file…");
  try { toast("Saved to " + (await invoke("save_attachment", { id: m.id })) + ". Only open files from people you trust."); } catch (e) { toast(String(e)); }
}
async function openChat(name) {
  st.chat = name; st.group = null; st.gdetail = null; st.screen = null; st.verifyDone = null; st.verifyFor = null;
  try { st.msgs = await invoke("conversation", { name }); } catch (e) { toast(String(e)); }
  render(); scrollEnd();
  const c = st.ov.contacts.find((x) => x.name === name);
  if (c && c.unread) { try { await invoke("mark_read", { name }); await refresh(); } catch (_) {} }
}
// A message that arrives in the conversation you are looking at is read.
async function markOpenRead() {
  if (st.section !== "chats" || st.screen || document.hidden || !st.ov) return;
  if (st.group) {
    const g = st.ov.groups.find((x) => x.id === st.group);
    if (!g || !g.unread) return;
    try { await invoke("group_mark_read", { id: st.group }); st.ov = await invoke("overview"); await loadGroup(); render(); scrollEnd(); } catch (_) {}
    return;
  }
  const c = st.chat && st.ov.contacts.find((x) => x.name === st.chat);
  if (!c || !c.unread) return;
  try { await invoke("mark_read", { name: st.chat }); st.ov = await invoke("overview"); st.msgs = await invoke("conversation", { name: st.chat }); render(); scrollEnd(); } catch (_) {}
}
function scrollEnd() { const s = $("main").querySelector(".scroll"); if (s) s.scrollTop = s.scrollHeight; }
/* A call line in the conversation offers to call again, the same kind of call. */
function callBack(video) {
  if (cs) { toast("You are already in a call."); return; }
  if (st.chat) startCall(st.chat, video);
}
function threadItems(inGroup) {
  const items = []; let day = "", prev = null;
  for (const m of st.msgs) {
    const d = dayOf(m.ts); if (d !== day) { day = d; prev = null; items.push(h("div", { class: "day" }, d)); }
    // A message right after one from the same sender sits closer to it.
    const cont = prev && prev.out === m.out && prev.from === m.from && m.ts - prev.ts < 300000; prev = m;
    items.push(h("div", { class: "msg" + (m.out ? " out" : "") + (cont ? " cont" : "") },
      inGroup && !m.out && m.from && !cont ? h("span", { class: "sender", title: m.shared_by ? `Shared with you by ${m.shared_by}. Nobody can prove who wrote it.` : null }, m.from, m.from_verified ? null : h("span", { class: "tag bad", style: "margin-left:6px" }, "Not verified")) : null,
      m.deleted ? h("div", { class: "bubble" }, h("i", {}, "Message deleted")) : st.editing === m.id ? editBox(m) : (m.body ? h("div", { class: "bubble" }, m.body) : null),
      m.file ? h("div", { class: "file" }, h("span", { class: "fi" }, icon("file")), h("span", { style: "flex:1;min-width:0" }, h("div", { style: "font-weight:600" }, m.file.name), h("div", { class: "note" }, fmtSize(m.file.size))), h("button", { class: "btn", type: "button", onclick: () => saveFile(m) }, "Save")) : null,
      h("span", { class: "meta" }, clock(m.ts), m.edited ? " · edited" : "", !inGroup && !m.deleted && typeof m.id === "string" && m.id.startsWith("call-") ? [" · ", h("button", { class: "linkish", type: "button", style: "border:0;background:none;padding:0;color:var(--accent);font-size:11.5px;cursor:pointer", onclick: () => callBack(/video call/i.test(m.body)) }, "Call back")] : null, inGroup && m.shared_by ? ` · shared by ${m.shared_by} when you joined` : "", m.out && !m.deleted && st.editing !== m.id ? ownActions(m) : null)));
  }
  return items;
}
function chatMain() {
  if (st.group) return groupMain();
  const c = st.ov.contacts.find((x) => x.name === st.chat);
  if (!c) return [h("div", { class: "head" }, h("h1", {}, "Chats")), emptyState(st.ov.contacts.length ? "Choose a conversation." : "Add a contact to start.")];
  const head = h("div", { class: "head" }, tile(c, true), h("span", { class: "who" }, h("b", {}, c.name), h("span", { class: c.agent ? "verified" : null }, c.agent ? [icon("shield"), "Agent"] : `End-to-end encrypted · ${c.devices} device${c.devices > 1 ? "s" : ""}`)),
    callButtons(c), h("button", { class: c.verified ? "btn quiet" : "btn", type: "button", onclick: () => { st.screen = "verify"; st.verifyDone = null; render(); } }, c.verified ? "Safety number" : "Verify"));
  const items = threadItems(false);
  const picker = h("input", { type: "file", hidden: true, onchange: async (e) => { const f = e.target.files[0]; e.target.value = ""; if (f) await sendFile(c.name, f, ta.value); } });
  const ta = h("textarea", { "aria-label": "Message to " + c.name, placeholder: "Message", rows: "1", onkeydown: (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } } });
  async function send() {
    const text = ta.value; if (!text.trim()) return; ta.disabled = true;
    try { const n = await invoke("send", { name: c.name, text }); ta.value = ""; if (!n) toast("Saved, but no device could be reached yet. It will be delivered when they connect."); await refresh(); scrollEnd(); }
    catch (e) { toast(String(e)); }
    finally { const t = $("main").querySelector("textarea"); if (t) { t.disabled = false; t.focus(); } }
  }
  return [head, h("div", { class: "scroll" }, items.length ? h("div", { class: "thread" }, items) : emptyState(c.verified ? "No messages yet." : "No messages yet. Verify this contact first: compare their safety number over a call or in person.")),
    h("div", { class: "composer" }, h("div", { class: "bar" }, h("button", { class: "iconbtn", type: "button", "aria-label": "Attach a file", title: "Attach a file", onclick: () => picker.click() }, icon("clip")), picker, ta, h("button", { class: "iconbtn", type: "button", "aria-label": "Send", style: "background:var(--accent);color:var(--accent-ink);border:0", onclick: send }, icon("send"))), h("span", { class: "note" }, `Encrypted separately for each of ${c.name}'s devices.`))];
}

/* ---------- verify + add ---------- */
function verifyScreen() {
  const c = st.ov.contacts.find((x) => x.name === (st.verifyFor || st.chat)); const first = c.name.split(" ")[0];
  const done = st.verifyDone;
  return [header(`Verify ${c.name}`, () => { st.screen = st.verifyFor ? "group" : null; st.verifyFor = null; render(); }), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("p", { class: "lead" }, `Compare this with ${first} in person or on a call you trust, reading it aloud. If it matches on both sides, nobody is in the middle.`),
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Safety number"), h("div", { class: "mono", style: "font-size:20px;overflow-wrap:anywhere;padding:10px;background:var(--panel-2);border-radius:12px;text-align:center" }, c.fingerprint)),
    c.verified && !done ? h("div", { class: "status soft" }, h("b", {}, `${c.name} is verified.`), h("span", {}, "If this number ever changes, you will be warned.")) : null,
    done === "match" ? h("div", { class: "status good", role: "status" }, h("b", {}, `${c.name} is verified.`)) : null,
    done === "nomatch" ? h("div", { class: "status bad", role: "status" }, h("b", {}, "Do not trust this conversation yet."), h("span", {}, `Someone may be in the middle. Ask ${first} to show their number in person.`)) : null,
    !c.verified && !done ? h("div", { style: "display:grid;grid-template-columns:1fr 1fr;gap:10px" },
      h("button", { class: "btn big", type: "button", onclick: () => { st.verifyDone = "nomatch"; render(); } }, "They don't match"),
      h("button", { class: "btn primary big", type: "button", onclick: async () => { try { await invoke("mark_verified", { name: c.name, fingerprint: c.fingerprint }); st.verifyDone = "match"; await refresh(); } catch (e) { toast(String(e)); } } }, "They match")) : null))];
}
/** A QR code of this person's identity card, for a phone to scan (Add a contact, Scan). */
function cardQr(card) {
  const img = h("img", { class: "qrimg", alt: "QR code of your identity card", width: 240, height: 240 });
  invoke("qr_svg", { text: card }).then((svg) => { img.src = "data:image/svg+xml;utf8," + encodeURIComponent(svg); }).catch(() => { img.remove(); });
  return img;
}
function addScreen() {
  const name = h("input", { "aria-label": "Name", placeholder: "Name, e.g. Bob Chen" }), card = h("textarea", { class: "cardbox", "aria-label": "Identity card", placeholder: "Paste their identity card (starts with spike2:)" });
  return [header("Add a contact", () => { st.screen = null; render(); }), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("p", { class: "lead" }, "There is no phone-number or email lookup. You swap cards directly, so no server learns who knows whom."),
    h("label", { style: "font-weight:600" }, "Name"), h("div", { class: "field" }, name),
    h("label", { style: "font-weight:600" }, "Their identity card"), card,
    h("button", { class: "btn primary big", type: "button", onclick: async () => { try { st.addMsg = await invoke("add_contact", { name: name.value, card: card.value }); const n = name.value.trim(); await refresh(); st.screen = null; st.section = "chats"; await openChat(n); toast(st.addMsg); } catch (e) { toast(String(e)); } } }, "Add contact"),
    h("p", { class: "note" }, "They arrive as not verified until you compare safety numbers. Your own card is in Settings."),
    h("h2", { class: "eyebrow", style: "margin-top:12px" }, "Your code, for a phone to scan"), cardQr(st.ov.me.card)))];
}

/* ---------- agents ---------- */
function approvalCard(r) {
  const typed = !!r.typed, cur = typed ? r.typed.currency : null;
  const amount = r.amount_cents != null ? (cur ? money(r.amount_cents, cur) : `${r.amount_cents} cents`) : null;
  const sec = h("section", { class: "card" + (r.status === "waiting" ? " attention" : ""), "aria-label": "Request from " + r.from },
    h("span", { class: "eyebrow" }, r.status === "waiting" ? "Needs your approval" : r.status === "approved" ? `Approved${r.by ? " by " + r.by : ""}` : `Denied${r.by ? " by " + r.by : ""}`),
    h("h2", { style: "margin:0;font-size:22px;line-height:1.2" }, typed && r.summary ? r.summary : `“${r.note}”`),
    typed ? h("span", { style: "display:flex;gap:6px;align-items:center;flex-wrap:wrap" }, h("span", { class: "tag neutral" }, "typed action"), h("span", { class: "note" }, "This line comes from the signed fields. The service must match them exactly."))
      : h("div", { class: "status soft", style: "padding:8px 12px;border-radius:10px" }, h("b", {}, "Free text. "), "These are the agent's own words. Only the scope", r.amount_cents ? " and amount" : "", " were checked."),
    h("dl", { class: "kv" }, h("dt", {}, "From"), h("dd", {}, r.from),
      typed ? [h("dt", {}, "Agent says"), h("dd", {}, r.note, h("span", { class: "note" }, " (not verified)"))] : null,
      typed ? Object.entries(r.typed).map(([k, v]) => [h("dt", {}, k[0].toUpperCase() + k.slice(1)), h("dd", { class: "mono" }, v)]) : null,
      amount ? [h("dt", {}, "Amount"), h("dd", { style: "font-weight:600" }, amount + " once")] : null,
      h("dt", {}, "Scope"), h("dd", { class: "mono" }, r.scope),
      r.status === "waiting" ? [h("dt", {}, "Expires"), h("dd", {}, new Date(r.expires).toLocaleString())] : null));
  if (r.status === "waiting") sec.append(h("div", { style: "display:grid;grid-template-columns:1fr 1fr;gap:10px" },
    h("button", { class: "btn big", type: "button", onclick: () => decide(r, false) }, "Deny"),
    h("button", { class: "btn primary big", type: "button", onclick: () => decide(r, true) }, amount ? `Approve ${amount}` : "Approve")),
    h("span", { class: "note", style: "text-align:center" }, r.forwarded ? "Your main device records this and sends the signed receipt." : "Your decision is signed by this device and added to the agent log."));
  return sec;
}
async function decide(r, approve) { try { toast(await invoke("decide", { id: r.id, approve })); await refresh(); } catch (e) { toast(String(e)); } }
function agentsMain() {
  const a = st.agents, w = a.requests.filter((r) => r.status === "waiting"), done = a.requests.filter((r) => r.status !== "waiting");
  const grant = (g) => h("section", { class: "card" }, h("h2", { class: "eyebrow" }, `What ${g.agent} may do`),
    h("div", { class: "chips" }, g.scopes.map((s) => h("span", { class: "yes" }, s))),
    h("dl", { class: "kv" }, h("dt", {}, "Per request"), h("dd", {}, g.max_cents == null ? "no money allowed" : `up to ${g.max_cents} cents; alone up to ${g.auto_below_cents ?? 0}`),
      h("dt", {}, "Per 24 hours"), h("dd", {}, g.daily_cents == null ? "no cumulative cap" : `${g.daily_cents} cents`), h("dt", {}, "Rate"), h("dd", {}, `${g.per_hour} requests an hour`),
      h("dt", {}, "Typed only"), h("dd", {}, g.typed_only ? "Yes: free text is refused" : "No: free text is allowed but marked unchecked"), h("dt", {}, "Ends"), h("dd", {}, new Date(g.expires).toLocaleDateString())),
    a.approver ? h("div", { style: "display:flex;gap:8px" },
      h("button", { class: "btn", type: "button", onclick: () => { openGrantForm(g.agent); } }, "Change"),
      h("button", { class: "btn danger", type: "button", onclick: async () => { try { await invoke("revoke", { name: g.agent }); toast(`${g.agent}'s grant removed. Approvals already given live until they expire.`); await refresh(); } catch (e) { toast(String(e)); } } }, "Revoke")) : null);
  return [h("div", { class: "head" }, h("h1", { style: "flex:none" }, "Agent requests"), h("span", { class: "tag " + (w.length ? "waiting" : "neutral") }, w.length ? `${w.length} waiting` : "nothing waiting"), h("span", { class: "spacer" }), a.approver ? null : h("span", { class: "note" }, "This is not your main device: decisions are forwarded to it")),
    h("div", { class: "scroll" }, h("div", { class: "page" },
      w.length || done.length ? [w.map(approvalCard), done.length ? h("h2", { class: "eyebrow", style: "margin:8px 0 0" }, "Decided") : null, done.map(approvalCard)] : h("div", { class: "status plain" }, h("b", {}, "No requests yet"), h("span", { class: "note" }, "Give an agent a standing permission first; its requests then appear here.")),
      a.grants.map(grant),
      a.approver ? h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: () => openGrantForm(null) }, icon("addperson"), "Give an agent a grant") : null))];
}
async function openGrantForm(agent) {
  let types = [];
  try { types = await invoke("action_types"); } catch (_) {}
  const g = agent && st.agents.grants.find((x) => x.agent === agent);
  st.grantForm = { types, agent: agent || (st.ov.contacts[0] && st.ov.contacts[0].name) || "", scopes: g ? [...g.scopes] : [], custom: "", days: 30, max: g && g.max_cents != null ? String(g.max_cents) : "", auto: g && g.auto_below_cents != null ? String(g.auto_below_cents) : "", daily: g && g.daily_cents != null ? String(g.daily_cents) : "", perHour: g ? g.per_hour : 10, typedOnly: g ? g.typed_only : false };
  st.section = "agents"; st.screen = "grant"; render();
}
function grantScreen() {
  const f = st.grantForm, num = (v) => (v.trim() === "" ? null : Number(v));
  const toggleScope = (n, on) => { f.scopes = on ? [...new Set([...f.scopes, n])] : f.scopes.filter((x) => x !== n); render(); };
  const input = (key, label, hint, type) => h("label", { style: "display:flex;flex-direction:column;gap:4px" }, h("b", { style: "font-weight:600" }, label), h("div", { class: "field" }, h("input", { type: type || "text", inputmode: "numeric", value: f[key], oninput: (e) => { f[key] = e.target.value; } })), hint ? h("span", { class: "note" }, hint) : null);
  const save = async () => {
    const scopes = [...f.scopes, ...f.custom.split(/[\s,]+/).filter(Boolean)];
    try {
      await invoke("grant", { request: { agent: f.agent, scopes, days: Number(f.days), max_cents: num(f.max), auto_below_cents: num(f.auto), per_hour: Number(f.perHour), daily_cents: num(f.daily), typed_only: f.typedOnly } });
      toast(`${f.agent} may now ask within these limits.`); st.screen = null; await refresh();
    } catch (e) { toast(String(e)); }
  };
  return [header("Give an agent a grant", () => { st.screen = null; render(); }), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("p", { class: "lead" }, "An agent is a contact that asks you for permission. Anything outside the grant is denied. A new grant replaces the old one."),
    h("label", { style: "display:flex;flex-direction:column;gap:4px" }, h("b", { style: "font-weight:600" }, "Agent"), h("div", { class: "field" }, h("select", { "aria-label": "Agent", style: "border:0;background:transparent;flex:1;min-height:42px", onchange: (e) => { f.agent = e.target.value; } }, st.ov.contacts.map((c) => h("option", { value: c.name, selected: c.name === f.agent || null }, c.name))))),
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "What it may ask for"),
      f.types.map((t) => h("label", { class: "pick" }, h("input", { type: "checkbox", checked: f.scopes.includes(t.name) || null, onchange: (e) => toggleScope(t.name, e.target.checked) }),
        h("span", { style: "flex:1" }, h("b", { style: "font-weight:600" }, t.name), h("div", { class: "note" }, `${t.template.replace(/\{(\w+)\}/g, (_, k) => "<" + k + ">")}${t.money ? " · moves money" : ""}`)))),
      h("label", { style: "display:flex;flex-direction:column;gap:4px;margin-top:6px" }, h("b", { style: "font-weight:600" }, "Other permissions (free text)"), h("div", { class: "field" }, h("input", { placeholder: "e.g. travel.dates.read, separated by spaces", value: f.custom, disabled: f.typedOnly || null, oninput: (e) => { f.custom = e.target.value; } })), h("span", { class: "note" }, "These cannot be checked by a service. Not allowed when typed actions only is on."))),
    h("label", { class: "toggle" }, h("input", { type: "checkbox", checked: f.typedOnly || null, onchange: (e) => { f.typedOnly = e.target.checked; if (f.typedOnly) f.custom = ""; render(); } }), h("span", {}, h("b", { style: "font-weight:600" }, "Accept typed actions only"), h("div", { class: "note" }, "Requests must carry fields you see and a service checks. Free text is refused."))),
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Limits (amounts in cents: 100 = 1.00 of the request's currency)"),
      input("max", "Most per request", "Empty means no money at all.", "number"), input("auto", "Approve without asking up to", "Empty means always ask you.", "number"), input("daily", "Most in any 24 hours", "Counts approved and waiting requests. Empty means no cap.", "number"),
      input("perHour", "Most requests per hour", "", "number"), input("days", "Grant lasts (days)", "", "number")),
    h("button", { class: "btn primary big", type: "button", disabled: !f.agent || null, onclick: save }, "Save grant")))];
}

/* ---------- devices and linking ---------- */
function devicesMain() {
  const me = st.ov.me;
  return [h("div", { class: "head" }, h("h1", {}, "Your devices"), h("button", { class: "btn primary", type: "button", disabled: !me.approver || null, title: me.approver ? "" : "Link new devices from your main device", onclick: () => { st.screen = "linkold"; st.linkStep = 1; st.linkErr = ""; st.linkCode = ""; render(); } }, "Link a new device")),
    h("div", { class: "scroll" }, h("div", { class: "page" },
      h("p", { class: "lead" }, "Contacts encrypt every message separately for each of these. Signed device list ", h("b", {}, "v" + me.list_version), "."),
      h("section", { class: "card", style: "gap:0;padding-top:4px;padding-bottom:4px" }, me.device_list.map((d) => h("div", { class: "devrow" }, h("span", { class: "tile sm sq c-neutral" }, icon("devices")),
        h("span", { style: "flex:1;min-width:0" }, h("b", { style: "font-weight:600" }, d.this ? "This computer" : "Another device"), h("div", { class: "note mono" }, d.id + (d.relay ? " · " + d.relay : "")), d.approver ? h("div", { class: "note" }, "Main device: decides agent requests and links new devices") : null),
        d.this ? h("span", { class: "pill neutral" }, "You are here") : (me.approver ? h("button", { class: "btn danger", type: "button", onclick: () => removeDevice(d) }, "Remove") : null)))),
      h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Your identity"), h("span", { class: "mono", style: "font-size:16px;overflow-wrap:anywhere" }, me.fingerprint), h("span", { class: "note" }, "Contacts compare this when they verify you. It signs your device list; it never signs messages.")),
      h("div", { class: "status soft" }, h("b", {}, "Losing every device."), h("span", {}, "Keep another device linked, or make an encrypted backup file in Settings: with neither, if you lose every device and the passphrase, the identity is gone.")))) ];
}
async function removeDevice(d) {
  if (!confirm("Remove this device from your identity?\n\nIt keeps what it already has but will receive nothing new, and your contacts stop sending to it. Link it again later if you change your mind.")) return;
  try { await invoke("remove_device", { id: d.id }); toast("Device removed. Your contacts learn the new list automatically."); await refresh(); } catch (e) { toast(String(e)); }
}
// NEW device: show a code, wait for the other device, compare, receive.
async function startLinkNew() {
  st.linking = true; st.linkStep = 1; st.linkErr = ""; st.linkSafety = "";
  try { st.linkCode = await invoke("link_offer"); st.linkQr = ""; try { st.linkQr = await invoke("qr_svg", { text: st.linkCode }); } catch (_) {} } catch (e) { st.linkErr = String(e); st.linking = false; return render(); }
  render();
  while (st.screen === "linknew" && !st.linkSafety) {
    try { st.linkSafety = await invoke("link_wait"); st.linkStep = 2; render(); }
    catch (e) { if (!/timed out/i.test(String(e))) { st.linkErr = String(e); break; } }
  }
  render();
}
function linkNewScreen() {
  const cancel = async () => { try { await invoke("link_cancel"); } catch (_) {} st.linking = false; st.screen = null; st.linkSafety = ""; render(); syncNow(); };
  const done = st.linkStep === 3;
  return [header("Link this computer", cancel), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("div", { class: "steps" }, ["1 · Show code", "2 · Compare", "3 · Done"].map((t, i) => h("span", { class: i + 1 === st.linkStep ? "on" : i + 1 < st.linkStep ? "done" : "" }, t))),
    st.linkErr ? h("div", { class: "status bad", role: "alert" }, h("b", {}, "Linking stopped."), h("span", {}, st.linkErr)) : null,
    st.linkStep === 1 && !st.linkErr ? [
      h("p", { class: "lead" }, "On a device that already uses your identity, open Devices, choose Link a new device, and paste this code."),
      st.linkQr ? h("img", { class: "qrimg", alt: "QR code containing the link code", width: 240, height: 240, src: "data:image/svg+xml;utf8," + encodeURIComponent(st.linkQr) }) : null,
      h("p", { class: "note" }, "Scan this with a phone that already has Spike, or copy the code and paste it on the other computer."),
      h("textarea", { class: "cardbox mono", readonly: true, "aria-label": "Link code", onfocus: (e) => e.target.select() }, st.linkCode),
      h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: async () => { try { await navigator.clipboard.writeText(st.linkCode); toast("Code copied."); } catch (_) { toast("Select the text and copy it."); } } }, "Copy code"),
      h("p", { class: "note", role: "status" }, "Waiting for your other device…")] : null,
    st.linkStep === 2 ? [
      h("p", { class: "lead" }, "Both screens must show the same code. If they differ, someone is in the middle: cancel."),
      h("div", { class: "code mono", style: "font-size:26px;text-align:center;letter-spacing:3px;padding:14px;background:var(--panel-2);border-radius:12px" }, st.linkSafety),
      h("div", { style: "display:grid;grid-template-columns:1fr 1fr;gap:10px" }, h("button", { class: "btn big", type: "button", onclick: cancel }, "Different: cancel"),
        h("button", { class: "btn primary big", type: "button", onclick: async () => { try { toast("Receiving your identity and history…"); const r = await invoke("link_receive"); st.linkStep = 3; st.linking = false; st.linkErr = ""; await refresh(); toast(`Linked: ${r.contacts} contacts and ${r.messages} messages copied.`); st.screen = null; st.section = "chats"; render(); syncNow(); } catch (e) { st.linkErr = String(e); st.linking = false; render(); } } }, "Same code"))] : null,
    st.linkErr ? h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: () => { st.screen = null; st.linking = false; render(); } }, "Back") : null,
    done ? h("div", { class: "status good" }, "Linked.") : null))];
}
// EXISTING device: paste the code, compare, choose history.
function linkOldScreen() {
  const modes = [["all", "All text, recent attachments", "Recommended. Older attachments stay here and open on demand."], ["recent", "Last 30 days only", "Messages and attachments from the past month."], ["none", "Start fresh", "No history. New messages only."]];
  const cancel = async () => { try { await invoke("link_cancel"); } catch (_) {} st.screen = null; render(); };
  const box = h("textarea", { class: "cardbox mono", "aria-label": "Code from the new device", placeholder: "Paste the code the new device shows" });
  return [header("Link a new device", cancel), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("div", { class: "steps" }, ["1 · Paste code", "2 · Compare", "3 · History"].map((t, i) => h("span", { class: i + 1 === st.linkStep ? "on" : i + 1 < st.linkStep ? "done" : "" }, t))),
    st.linkErr ? h("div", { class: "status bad", role: "alert" }, h("span", {}, st.linkErr)) : null,
    st.linkStep === 1 ? [h("p", { class: "lead" }, "On the new device choose “I already use Spike on another device”. It shows a code. Paste it here."), box,
      h("button", { class: "btn primary big", type: "button", onclick: async () => { st.linkErr = ""; try { st.linkSafety = await invoke("link_begin", { code: box.value }); st.linkStep = 2; } catch (e) { st.linkErr = String(e); } render(); } }, "Continue")] : null,
    st.linkStep === 2 ? [h("p", { class: "lead" }, "Both screens must show the same code. If they differ, someone is in the middle: cancel and try again."),
      h("div", { class: "code mono", style: "font-size:26px;text-align:center;letter-spacing:3px;padding:14px;background:var(--panel-2);border-radius:12px" }, st.linkSafety),
      h("div", { style: "display:grid;grid-template-columns:1fr 1fr;gap:10px" }, h("button", { class: "btn big", type: "button", onclick: cancel }, "Different: cancel"), h("button", { class: "btn primary big", type: "button", onclick: () => { st.linkStep = 3; render(); } }, "Same code"))] : null,
    st.linkStep === 3 ? [h("p", { class: "lead" }, "What should the new device bring?"),
      h("div", { role: "radiogroup", "aria-label": "History to copy", style: "display:flex;flex-direction:column;gap:8px" }, modes.map(([v, t, d]) => h("button", { class: "option", type: "button", role: "radio", "aria-checked": String(st.linkMode === v), onclick: () => { st.linkMode = v; render(); } }, h("span", { class: "radio" }), h("span", {}, h("b", { style: "font-weight:600" }, t), h("div", { class: "note" }, d))))),
      h("button", { class: "btn primary big", type: "button", onclick: async () => { try { toast("Linking…"); const r = await invoke("link_finish", { history: st.linkMode, days: 30 }); st.linkErr = ""; st.screen = null; await refresh(); toast(`Linked. Device list v${r.version}; ${r.contacts} contacts and ${r.messages} messages sent, encrypted to the new device.`); } catch (e) { st.linkErr = String(e); render(); } } }, "Link device"),
      h("span", { class: "note", style: "text-align:center" }, "Sent encrypted to the new device only. Deleted and expired messages are never copied.")] : null))];
}

/* ---------- settings ---------- */
async function refreshBlocked() { try { st.blocked = await invoke("blocked"); } catch (_) { st.blocked = []; } render(); }
async function refreshDirect() { try { st.direct = await invoke("direct_status"); } catch (_) { st.direct = null; } render(); }
/** Direct connections (ADR 0027): messages between two devices on the same network without the relay. */
function directCard() {
  const d = st.direct;
  if (!d) return null;
  const set = async (cmd) => { try { st.direct = await invoke(cmd); toast(st.direct.listening ? "Direct connections are on." : "Direct connections are off."); } catch (e) { toast(String(e)); await refreshDirect(); return; } render(); };
  return h("section", { class: "card", id: "direct-card" }, h("h2", { class: "eyebrow" }, "Direct connections"),
    h("label", { class: "toggle" }, h("input", { id: "direct-toggle", type: "checkbox", checked: d.enabled || null, onchange: (e) => set(e.target.checked ? "direct_enable" : "direct_disable") }),
      h("span", {}, h("b", { style: "font-weight:600" }, "Send and receive directly on this network"), h("div", { class: "note" }, "Messages go straight to your contact's device when it is on the same Wi-Fi, and through the relay otherwise. Turning this on opens a port on this computer; only devices of people you have added can use it. Anyone on the network can see which two devices talk, when, and how much, never what."))),
    d.enabled ? h("div", { class: "status soft", id: "direct-status" }, h("b", {}, d.listening ? "Listening." : "Not listening."),
      h("span", { class: "mono", id: "direct-addresses", style: "overflow-wrap:anywhere" }, d.addresses.length ? "Give this to your other device: " + d.addresses.join("  ") : d.listening ? `Port ${d.port}` : ""),
      h("span", { class: "note" }, `Sent directly: ${d.sent_direct}. Sent by the relay after a direct try failed: ${d.sent_fallback}. Received directly: ${d.received}.`)) : null,
    d.enabled ? d.contacts.map((c) => { const inp = h("input", { "aria-label": `Direct address for ${c.name}`, class: "mono", value: c.address, placeholder: c.learned[0] || "192.168.1.20:8788" });
      return h("div", { class: "devrow", style: "flex-wrap:wrap" }, h("span", { style: "min-width:90px;font-weight:600" }, c.name), h("div", { class: "field", style: "flex:1;min-width:180px" }, inp),
        h("button", { class: "btn", type: "button", onclick: async () => { try { await invoke("direct_set_address", { name: c.name, address: inp.value }); toast(inp.value.trim() ? "Address saved." : "Address cleared."); await refreshDirect(); } catch (e) { toast(String(e)); } } }, "Save"),
        c.learned.length ? h("div", { class: "note", style: "flex-basis:100%" }, "Last seen at " + c.learned.join(", ")) : null); }) : null);
}
function settingsMain() {
  if (!st.blockedLoaded) { st.blockedLoaded = true; refreshBlocked(); }
  if (!st.directLoaded) { st.directLoaded = true; refreshDirect(); }
  const me = st.ov.me, relay = h("input", { "aria-label": "Relay address", value: st.ov.relay, class: "mono" });
  const caPicker = h("input", { type: "file", accept: ".crt,.pem,.cer", hidden: true, onchange: async (e) => {
    const f = e.target.files[0]; e.target.value = ""; if (!f) return;
    if (f.size > 65536) return toast("That file is too large to be a certificate.");
    try { const n = await invoke("set_relay_ca", { pem: await f.text() }); toast(`Trusting ${n} certificate${n > 1 ? "s" : ""} from ${f.name}.`); await refresh(); syncNow(); } catch (x) { toast(String(x)); } } });
  return [h("div", { class: "head" }, h("h1", {}, "Settings")), h("div", { class: "scroll" }, h("div", { class: "page" },
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Your identity"), h("span", { class: "mono", style: "font-size:16px;overflow-wrap:anywhere" }, me.fingerprint),
      h("span", { class: "note" }, `${me.devices} device${me.devices > 1 ? "s" : ""}${me.approver ? " · this device approves agent requests" : ""}. Contacts compare this when they verify you.`),
      h("h2", { class: "eyebrow", style: "margin-top:8px" }, "Your identity card"), h("textarea", { class: "cardbox mono", readonly: true, "aria-label": "Your identity card", onfocus: (e) => e.target.select() }, me.card),
      cardQr(me.card),
      h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: async () => { try { await navigator.clipboard.writeText(me.card); toast("Card copied. Send it to your contact."); } catch (_) { toast("Select the text above and copy it."); } } }, "Copy card")),
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Home relay"), h("div", { class: "field" }, relay),
      h("span", { class: "note" }, "Use wss:// for a relay on another computer. Plain ws:// is only accepted for this computer unless you allow it below. A relay sees which devices connect and message sizes, never text or files."),
      h("div", { style: "display:flex;gap:8px;flex-wrap:wrap" },
        h("button", { class: "btn", type: "button", onclick: async () => { try { await invoke("set_relay", { url: relay.value }); toast("Relay saved."); await refresh(); syncNow(); } catch (e) { toast(String(e)); } } }, "Save relay"),
        h("button", { class: "btn", type: "button", onclick: async () => { try { await invoke("set_relay", { url: relay.value }); toast("Connecting…"); toast(await invoke("test_relay")); } catch (e) { toast(String(e)); } } }, "Test connection"))),
    backupCard(),
    directCard(),
    callsCard(),
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Trust for a relay you run yourself"),
      h("span", {}, st.ov.relay_trust.ca_certs ? `${st.ov.relay_trust.ca_certs} extra certificate${st.ov.relay_trust.ca_certs > 1 ? "s are" : " is"} trusted, as well as the usual public ones.` : "Only the usual public certificate authorities are trusted."),
      h("span", { class: "note" }, "A relay with a self-signed or private certificate needs that certificate trusted here: choose its .crt or .pem file (the certificate, never the private key)."),
      h("div", { style: "display:flex;gap:8px;flex-wrap:wrap" }, caPicker, h("button", { class: "btn", type: "button", onclick: () => caPicker.click() }, "Choose certificate file…"),
        st.ov.relay_trust.ca_certs ? h("button", { class: "btn danger", type: "button", onclick: async () => { try { await invoke("set_relay_ca", { pem: null }); toast("Certificate removed."); await refresh(); } catch (e) { toast(String(e)); } } }, "Stop trusting it") : null),
      h("label", { class: "toggle", style: "margin-top:6px" }, h("input", { type: "checkbox", checked: st.ov.relay_trust.allow_insecure || null, onchange: async (e) => { try { await invoke("set_allow_insecure", { allow: e.target.checked }); await refresh(); } catch (x) { toast(String(x)); await refresh(); } } }),
        h("span", {}, h("b", { style: "font-weight:600" }, "Allow plain ws:// on a network I trust"), h("div", { class: "note" }, "Messages stay end-to-end encrypted, but anyone on the network can see which devices talk, when, and how much. Use it only on a private network you control; never over the internet."))),
      st.ov.relay_trust.allow_insecure ? h("div", { class: "status warn", role: "alert" }, h("b", {}, "Plain connections are allowed."), h("span", {}, "Turn this off unless you are on a network you control.")) : null),
    st.blocked.length ? h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Blocked people"),
      h("span", { class: "note" }, "Their messages are dropped when they arrive. (The relay still carries them until then: it cannot tell who sent a message.)"),
      st.blocked.map((b) => h("div", { class: "devrow" }, h("span", { style: "flex:1;min-width:0" }, h("span", { class: "mono", style: "overflow-wrap:anywhere" }, b.fingerprint), h("div", { class: "note" }, "blocked " + new Date(b.at).toLocaleDateString())),
        h("button", { class: "btn", type: "button", onclick: async () => { try { await invoke("unblock", { owner: b.owner }); toast("Unblocked. Only a new conversation from them will be seen."); await refreshBlocked(); } catch (e) { toast(String(e)); } } }, "Unblock")))) : null,
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Privacy and security"),
      h("label", { style: "display:flex;flex-direction:column;gap:4px" }, h("b", { style: "font-weight:600" }, "Lock after no use for"), h("div", { class: "field" }, h("select", { "aria-label": "Auto-lock", style: "border:0;background:transparent;flex:1;min-height:42px", onchange: (e) => { try { localStorage.setItem("autolock", e.target.value); } catch (_) {} armAutoLock(); toast(Number(e.target.value) ? `Locks after ${e.target.selectedOptions[0].text} without use.` : "Auto-lock is off."); } }, AUTOLOCK_CHOICES.map(([v, l]) => h("option", { value: String(v), selected: v === autoLockMinutes() || null }, l)))),
        h("span", { class: "note" }, "Locking wipes your keys from memory. While locked, this computer does not receive messages.")),
      h("span", { class: "note" }, "The window title, and the taskbar or dock, show how many messages and requests are waiting.")),
    updatesCard(),
    h("div", { class: "status soft" }, h("b", {}, "Not built in the app yet:"), h("span", {}, "an installer with auto-update, and desktop notification banners (the taskbar flashes instead). This is an unreviewed prototype.")),
    h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: lockNow }, icon("lock"), "Lock now")))];
}

/* ---------- backup and recovery (ADR 0029) ---------- */
const BACKUP_WARNING = "This file is a copy of your identity. Anyone who has it and knows its passphrase can pretend to be you and will receive your new messages. Keep it somewhere only you can reach, and keep the passphrase separate from the file. There is no copy on any server: if you lose both this file and your devices, your identity cannot be recovered. A restored account starts new conversations with each contact, and messages sent to you while you were locked out of every device may be lost.";
function backupCard() {
  const b = st.bk || (st.bk = {});
  if (!st.bkLoaded) { st.bkLoaded = true; invoke("backup_status").then((s) => { b.status = s; render(); }).catch(() => {}); }
  const s = b.status;
  const own = h("input", { id: "bkOwn", type: "password", class: "mono", autocomplete: "new-password", "aria-label": "Your own backup passphrase", placeholder: "or type your own passphrase (14+ characters)", value: b.own || "", oninput: (e) => { b.own = e.target.value; } });
  const makeKey = async () => { try { b.key = await invoke("backup_new_key"); b.ack = false; b.saved = null; render(); } catch (e) { toast(String(e)); } };
  const save = async () => {
    const pass = b.own || b.key;
    if (!pass) return toast("Make a backup key, or type your own passphrase.");
    if (!b.own && !b.ack) return toast("Tick the box to say you wrote the key down.");
    b.busy = true; render();
    try { b.saved = await invoke("backup_export", { passphrase: pass, history: !!b.history }); b.key = null; b.own = ""; b.ack = false; b.status = await invoke("backup_status"); }
    catch (e) { toast(String(e)); }
    b.busy = false; render();
  };
  return h("section", { class: "card", id: "backupCard" }, h("h2", { class: "eyebrow" }, "Backup and recovery"),
    s ? h("div", { class: "status " + (s.stale ? "warn" : "soft"), role: s.stale ? "alert" : "status" }, h("b", {}, s.last_ms ? "Last backup: " + new Date(s.last_ms).toLocaleString() : "No backup yet"), s.stale && s.reason ? h("span", {}, s.reason + " Make a new one and delete the old file.") : null) : null,
    h("div", { class: "status soft" }, h("span", {}, BACKUP_WARNING)),
    b.key ? [h("div", { class: "note" }, "Your backup key. It is shown once and is not stored. Write it down and keep it apart from the file:"),
      h("span", { class: "mono", id: "bkKey", style: "font-size:18px;overflow-wrap:anywhere;user-select:all" }, b.key),
      h("label", { class: "toggle" }, h("input", { id: "bkAck", type: "checkbox", checked: b.ack || null, onchange: (e) => { b.ack = e.target.checked; } }), h("span", {}, "I wrote the key down"))]
      : h("button", { class: "btn", id: "bkMake", style: "align-self:flex-start", type: "button", onclick: makeKey }, "Make a backup key (recommended)"),
    h("div", { class: "field" }, own),
    h("label", { class: "toggle" }, h("input", { id: "bkHist", type: "checkbox", checked: b.history || null, onchange: (e) => { b.history = e.target.checked; } }), h("span", {}, "Include my message history", h("div", { class: "note" }, "Makes the file larger and more valuable to anyone who gets it. Files are not included."))),
    b.saved ? h("div", { class: "status soft", id: "bkSaved", role: "status" }, h("b", {}, "Saved."), h("span", {}, b.saved + ". Move it somewhere safe.")) : null,
    h("button", { class: "btn primary", id: "bkSave", style: "align-self:flex-start", type: "button", disabled: b.busy || null, onclick: save }, b.busy ? "Sealing…" : "Save backup file"));
}
function restoreGate() {
  const r = { step: "open", file: null, summary: null, busy: false, err: "" };
  const enc = encodeURIComponent;
  const back = () => { invoke("backup_cancel").catch(() => {}); gate("create"); };
  const draw = () => {
    const err = h("div", { class: "err", role: "alert", hidden: r.err ? null : true }, r.err);
    if (r.step === "open") {
      const picker = h("input", { id: "rsFile", type: "file", onchange: (e) => { r.file = e.target.files[0] || null; } });
      const pass = h("input", { id: "rsPass", type: "password", class: "mono", autocomplete: "off", "aria-label": "Backup key or passphrase" });
      const open = async (e) => {
        e.preventDefault(); r.err = "";
        if (!r.file) { r.err = "Choose the backup file."; return draw(); }
        r.busy = true; draw();
        try { r.summary = await invoke("backup_open", new Uint8Array(await r.file.arrayBuffer()), { headers: { "x-passphrase": enc(pass.value) } }); r.step = "confirm"; }
        catch (x) { r.err = String(x); }
        r.busy = false; draw();
      };
      return $("gate").replaceChildren(h("form", { class: "gatebox", onsubmit: open },
        h("span", { class: "eyebrow" }, "Spike Connect"), h("h1", {}, "Restore from a backup"),
        h("p", { class: "lead" }, "Choose the backup file you saved and type its key or passphrase."),
        h("label", { class: "lbl", for: "rsFile", style: "font-weight:600" }, "Backup file"), picker,
        h("label", { class: "lbl", for: "rsPass", style: "font-weight:600" }, "Backup key or passphrase"), h("div", { class: "field" }, pass),
        err, h("button", { class: "btn primary big", id: "rsOpen", type: "submit", disabled: r.busy || null }, r.busy ? "Opening…" : "Open backup"),
        h("button", { class: "btn", type: "button", onclick: back }, "Back")));
    }
    const sm = r.summary;
    const p1 = h("input", { id: "rsDev1", type: "password", autocomplete: "new-password", "aria-label": "New passphrase for this computer" });
    const p2 = h("input", { id: "rsDev2", type: "password", autocomplete: "new-password", "aria-label": "Repeat the passphrase" });
    const relay = h("input", { id: "rsRelay", class: "mono", "aria-label": "Relay address", value: sm.relay });
    const go = async (e) => {
      e.preventDefault(); r.err = "";
      if (p1.value !== p2.value) { r.err = "The two passphrases differ."; return draw(); }
      r.busy = true; draw();
      try { await invoke("backup_restore", { passphrase: p1.value, relay: relay.value }); await openApp(); return; }
      catch (x) { r.err = String(x); }
      r.busy = false; draw();
    };
    $("gate").replaceChildren(h("form", { class: "gatebox", onsubmit: go },
      h("span", { class: "eyebrow" }, "Spike Connect"), h("h1", {}, "Bring your identity back"),
      h("p", { class: "lead", id: "rsSummary" }, `Backup made ${new Date(sm.created_ms).toLocaleString()}: ${sm.contacts} contact${sm.contacts === 1 ? "" : "s"}, ${sm.groups} group${sm.groups === 1 ? "" : "s"}, ${sm.messages} message${sm.messages === 1 ? "" : "s"}.`),
      h("div", { class: "status warn" }, h("span", {}, "Restoring signs out every other device that uses this identity, including the one you lost. You can link them again afterwards. This needs the relay to be reachable.")),
      h("label", { class: "lbl", for: "rsRelay", style: "font-weight:600" }, "Relay"), h("div", { class: "field" }, relay),
      h("label", { class: "lbl", for: "rsDev1", style: "font-weight:600" }, "Choose a passphrase for this computer"), h("div", { class: "field" }, p1), h("div", { class: "field" }, p2),
      h("div", { class: "note" }, "10 characters or more. This is a new passphrase for this computer, separate from the backup's."),
      err, h("button", { class: "btn primary big", id: "rsGo", type: "submit", disabled: r.busy || null }, r.busy ? "Restoring…" : "Restore my identity"),
      h("button", { class: "btn", type: "button", onclick: back }, "Cancel")));
  };
  draw();
}

/* ---------- updates: only when asked (docs/desktop-release.md) ---------- */
function updatesCard() {
  const u = st.update;
  const check = async () => { st.update = { busy: "Checking…" }; render(); try { st.update = await invoke("update_check"); } catch (e) { st.update = { error: String(e) }; } render(); };
  const install = async () => { st.update = { ...st.update, busy: "Downloading and verifying…" }; render(); try { await invoke("update_install"); } catch (e) { st.update = { error: String(e) }; render(); } };
  return h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Updates"),
    h("span", { class: "note" }, "Spike never looks for updates by itself, because a check tells the update server that this computer runs Spike. Check when you choose."),
    u && u.error ? h("div", { class: "status bad", role: "alert" }, h("span", {}, u.error)) : null,
    u && u.supported === false ? h("div", { class: "status soft" }, h("span", {}, "This build cannot update itself. Install a newer version from where you got this one.")) : null,
    u && u.available ? h("div", { class: "status soft" }, h("b", {}, "Version " + u.available + " is available."), u.notes ? h("span", {}, u.notes) : null) : null,
    u && u.supported && !u.available && !u.busy && !u.error ? h("span", { class: "note" }, "You have the newest version (" + u.current + ").") : null,
    u && u.busy ? h("span", { class: "note" }, u.busy) : null,
    h("div", { style: "display:flex;gap:8px" },
      h("button", { class: "btn", type: "button", disabled: (u && u.busy) || null, onclick: check }, "Check for updates"),
      u && u.available ? h("button", { class: "btn primary", type: "button", disabled: u.busy || null, onclick: install }, "Install and restart") : null));
}

/* ---------- message requests (ADR 0021) ---------- */
function requestsScreen() {
  const back = () => { st.screen = null; render(); };
  const card = (r) => {
    const shown = !!st.showReq[r.id], naming = st.naming === r.id;
    const input = h("input", { "aria-label": "Name for this person", placeholder: "A name for them", value: "", maxlength: "64" });
    const accept = async () => { try { const n = await invoke("accept_request", { id: r.id, name: input.value }); toast(`Added. ${n} message${n === 1 ? "" : "s"} joined the conversation. They are not verified yet.`); const name = input.value.trim(); st.naming = null; st.screen = null; await refresh(); st.section = "chats"; await openChat(name); } catch (e) { toast(String(e)); } };
    return h("section", { class: "card", "aria-label": "Message request" },
      h("div", { style: "display:flex;gap:12px;align-items:center" }, h("span", { class: "tile sm sq c-neutral", "aria-hidden": "true" }, "?"),
        h("span", { style: "flex:1;min-width:0" }, h("b", { style: "font-weight:600" }, "Someone new wants to message you"), h("div", { class: "note" }, `Their code: `, h("span", { class: "mono" }, r.fingerprint)), h("div", { class: "note" }, new Date(r.received).toLocaleString()))),
      shown ? [h("div", { class: "status soft", style: "gap:8px" }, r.held.map((m) => h("div", {}, h("span", { style: "white-space:pre-wrap;overflow-wrap:anywhere" }, m.body), m.file ? h("div", { class: "note" }, `A file named “${m.file}” came with it. Files from people you have not accepted are not kept.`) : null))),
          h("span", { class: "note" }, "Shown as plain text: links are not clickable.")]
        : h("div", { class: "status soft" }, h("span", {}, `${r.held.length} message${r.held.length === 1 ? "" : "s"} hidden until you choose to look. Links and files stay blocked until you accept.`), h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: () => { st.showReq[r.id] = true; render(); } }, "Show the message")),
      naming ? h("div", { style: "display:flex;flex-direction:column;gap:8px" }, h("label", { style: "font-weight:600" }, "What should you call them?"), h("div", { class: "field" }, input),
          h("div", { style: "display:flex;gap:8px" }, h("button", { class: "btn primary", type: "button", onclick: accept }, "Add contact"), h("button", { class: "btn", type: "button", onclick: () => { st.naming = null; render(); } }, "Cancel")),
          h("span", { class: "note" }, "They will be added as not verified. Compare their safety number with them before you trust it is really them."))
        : h("div", { style: "display:grid;grid-template-columns:1fr 1fr 1fr;gap:8px" },
            h("button", { class: "btn danger", type: "button", onclick: async () => { if (!confirm("Block this person? Their messages will be dropped when they arrive.")) return; try { await invoke("block_request", { id: r.id }); toast("Blocked."); await refresh(); if (!st.reqs.length) back(); } catch (e) { toast(String(e)); } } }, "Block"),
            h("button", { class: "btn", type: "button", onclick: async () => { try { await invoke("decline_request", { id: r.id }); toast("Declined. They are not told."); await refresh(); if (!st.reqs.length) back(); } catch (e) { toast(String(e)); } } }, "Decline"),
            h("button", { class: "btn primary", type: "button", onclick: () => { st.naming = r.id; render(); $("main").querySelector(".field input")?.focus(); } }, "Accept")));
  };
  return [header("Message requests", back), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("p", { class: "lead" }, "People who are not in your contacts and wrote to you. They cannot see whether you have read anything until you accept."),
    st.reqs.length ? st.reqs.map(card) : h("div", { class: "status plain" }, "No message requests."),
    h("p", { class: "note" }, "Requests are end-to-end encrypted like any message. Declining or blocking deletes the conversation attempt for good: you are not shown anything more from that session, and they are not told. Undecided requests are removed after 30 days.")))];
}

const GROUP_MAX = 20;
/* ---------- groups (ADR 0024) ---------- */
const unreadTotal = () => st.ov.contacts.reduce((a, c) => a + c.unread, 0) + st.ov.groups.reduce((a, g) => a + g.unread, 0);
const invitesCount = () => st.ov.groups.filter((g) => g.status === "invited").length;
const memLetter = (m) => (m.you ? "Y" : m.name.startsWith("~") ? "?" : ini(m.name));
async function sendGroupFile(id, file, caption) {
  if (file.size === 0) return toast("That file is empty.");
  if (file.size > MAX_FILE) return toast("Files are limited to 100 MB.");
  toast(`Encrypting and uploading ${file.name}…`);
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    const enc = encodeURIComponent;
    const r = await invoke("group_send_file", bytes, { headers: { "x-group": id, "x-filename": enc(file.name), "x-caption": enc(caption || "") } });
    const t = $("main").querySelector("textarea"); if (t) t.value = "";
    toast(r.reached < r.people ? `Sent to ${r.reached} of ${r.people} people. The others could not be reached yet.` : "File sent to the group.");
    r.notes.forEach(toast); await refresh(); scrollEnd();
  } catch (e) { toast(String(e)); }
}
const groupTile = (sm) => h("span", { class: `tile${sm ? " sm" : ""} c-neutral`, "aria-hidden": "true" }, icon("group"));
const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
async function loadGroup() {
  try { st.msgs = await invoke("group_conversation", { id: st.group }); st.gdetail = await invoke("group", { id: st.group }); }
  catch (_) { st.group = null; st.gdetail = null; st.msgs = []; if (st.screen === "group") st.screen = null; }
}
async function openGroup(id) {
  st.group = id; st.chat = null; st.screen = null; st.verifyDone = null; st.verifyFor = null; st.picking = false; st.nameMember = null;
  await loadGroup();
  render(); scrollEnd();
  const g = st.ov.groups.find((x) => x.id === id);
  if (g && g.unread) { try { await invoke("group_mark_read", { id }); await refresh(); } catch (_) {} }
}
function openMembers() { st.screen = "group"; st.picked = []; st.picking = false; st.nameMember = null; render(); }
function groupRow(g) {
  const invited = g.status === "invited", left = g.status === "left";
  return h("button", { class: "row", type: "button", "aria-current": String(!st.screen && st.group === g.id), onclick: () => openGroup(g.id) }, groupTile(false),
    h("span", { class: "body" }, h("span", { class: "line1" }, h("b", {}, g.name), invited ? h("span", { class: "tag waiting" }, "Invite") : left ? h("span", { class: "tag plain" }, "left") : g.unverified ? h("span", { class: "tag bad" }, `${g.unverified} not verified`) : null, g.large ? h("span", { class: "tag plain" }, "Large") : null),
      h("span", { class: "preview" }, invited ? `Invite from ${g.invited_by || "someone"}` : g.last || `${plural(g.members, "person", "people")} · no messages yet`)),
    h("span", { class: "meta" }, g.last_ts ? clock(g.last_ts) : "", g.unread ? h("span", { class: "count" }, g.unread) : null));
}
function groupMain() {
  const g = st.ov.groups.find((x) => x.id === st.group), d = st.gdetail;
  if (!g || !d) return [h("div", { class: "head" }, h("h1", {}, "Chats")), emptyState("Choose a conversation.")];
  if (d.status === "invited") return inviteScreen();
  const left = d.status !== "joined";
  const head = h("div", { class: "head" }, groupTile(true), h("span", { class: "who" }, h("b", {}, d.name), h("span", {}, left ? "You are not in this group" : `${plural(d.members.length, "person", "people")} · end-to-end encrypted${d.large ? " · large group" : ""}`)),
    h("button", { class: "btn", type: "button", onclick: openMembers }, icon("group"), "Members"));
  const warn = !left && d.unverified ? h("div", { class: "sysline attention", style: "align-self:stretch;max-width:none;margin:12px 20px 0" },
    h("span", { style: "flex:1" }, `${unverifiedText(d)} Messages from them are marked.`),
    h("button", { class: "btn", type: "button", onclick: openMembers }, "Review")) : null;
  const refusedNote = !left && d.large && d.refused ? h("div", { class: "sysline", style: "align-self:stretch;max-width:none;margin:12px 20px 0" },
    h("span", { style: "flex:1" }, `${plural(d.refused, "change was", "changes were")} refused because ${d.refused === 1 ? "it" : "they"} broke the group's rules, and skipped. Latest: ${d.refused_why || "unknown"}.`)) : null;
  const items = threadItems(true);
  const ta = h("textarea", { "aria-label": "Message to " + d.name, placeholder: "Message", rows: "1", onkeydown: (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } } });
  async function send() {
    const text = ta.value; if (!text.trim()) return; ta.disabled = true;
    try {
      const r = await invoke("group_send", { id: d.id, text }); ta.value = "";
      if (r.reached < r.people) toast(`Reached ${r.reached} of ${r.people} people. The others could not be reached yet.`);
      r.notes.forEach(toast); await refresh(); scrollEnd();
    } catch (e) { toast(String(e)); }
    finally { const t = $("main").querySelector("textarea"); if (t) { t.disabled = false; t.focus(); } }
  }
  const picker = h("input", { type: "file", hidden: true, onchange: async (e) => { const f = e.target.files[0]; e.target.value = ""; if (f) await sendGroupFile(d.id, f, ta.value); } });
  const composer = left ? h("div", { class: "composer" }, h("span", { class: "note" }, d.large ? "You are not in this group. Your history stays on this computer; an admin can invite you again." : "You are not in this group. Your history stays on this computer; nobody can add you back without your OK."))
    : h("div", { class: "composer" }, h("div", { class: "bar" }, d.large ? null : h("button", { class: "iconbtn", type: "button", "aria-label": "Attach a file", title: "Attach a file for everyone in the group", onclick: () => picker.click() }, icon("clip")), d.large ? null : picker, ta, h("button", { class: "iconbtn", type: "button", "aria-label": "Send", style: "background:var(--accent);color:var(--accent-ink);border:0", onclick: send }, icon("send"))),
      h("span", { class: "note" }, d.large ? `One encrypted copy for all ${d.members.length - 1} other ${d.members.length === 2 ? "person" : "people"}. Files are not available in large groups yet.` : `Encrypted separately for every device of the ${d.members.length - 1} other ${d.members.length === 2 ? "person" : "people"}, and for your own other devices. A file is encrypted once and shared with everyone.`));
  return [head, warn, refusedNote, h("div", { class: "scroll" }, items.length ? h("div", { class: "thread" }, items) : emptyState("No messages yet.")), composer].filter(Boolean);
}
const HISTORY_CHOICES = [["join", "Messages from when they join"], ["week", "The last 7 days"]];
const historyNote = (v) => v === "week"
  ? "When someone joins, they ask whoever added them for the last 7 days of messages and files. That person shares it: nobody can prove who wrote those messages, so they are marked as shared. Deleted messages are not included."
  : "Earlier messages stay with the people who were here. This is the default.";
function unverifiedText(d) {
  const others = d.members.length - 1;
  if (d.unverified === others) return others === 1 ? "The other person is not verified." : `None of the other ${others} people are verified.`;
  return `${d.unverified} of the other ${others} ${d.unverified === 1 ? "is" : "are"} not verified.`;
}
function inviteScreen() {
  const d = st.gdetail, back = () => { st.group = null; st.gdetail = null; st.screen = null; render(); };
  const stranger = !d.large && ((d.invited_by || "").startsWith("~") || !d.invited_by);
  const decide = async (kind) => {
    if (kind === "block" && !confirm(`Block ${d.invited_by || "this person"}?

This declines the invite, and anything they send you from now on is dropped, on all your devices. You can unblock them in Settings.`)) return;
    try {
      await invoke({ accept: "group_accept", decline: "group_decline", block: "group_block" }[kind], { id: d.id });
      if (kind !== "accept") { st.group = null; st.gdetail = null; }
      if (kind === "block") st.blockedLoaded = false;
      await refresh();
      toast(kind === "accept" ? (d.large ? `You joined ${d.name}.` : `You joined ${d.name}. Your other devices join too.`) : kind === "block" ? "Declined and blocked. They are not told." : "Declined. They are not told.");
    } catch (e) { toast(String(e)); }
  };
  return [header("Group invite", back), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("p", { class: "lead" }, `${stranger ? "Someone who is not in your contacts" : d.invited_by} ${d.large ? "invited you to" : "added you to"} “${d.name}”. Nothing from the group is shown until you accept.`),
    d.large ? h("div", { class: "status soft" }, h("span", {}, h("b", {}, "A large group. "), "You will see who is in it once you join. Large groups are experimental and have not been independently reviewed yet.")) : h("section", { class: "card" }, h("h2", { class: "eyebrow" }, `${plural(d.members.length, "person", "people")} in it`),
      d.members.map((m) => h("div", { class: "member" }, h("span", { class: `tile sm ${m.you ? "c-neutral" : tint(m.name)}`, "aria-hidden": "true" }, memLetter(m)),
        h("span", { class: "body" }, h("b", { style: "font-weight:600" }, m.you ? "You" : m.name, m.name === d.invited_by ? h("span", { class: "tag plain", style: "margin-left:8px" }, "added you") : null), h("span", { class: "note" }, m.you ? "" : m.contact ? (m.verified ? "In your contacts · verified" : "In your contacts · not verified") : "Not in your contacts"))))),
    stranger ? h("div", { class: "status warn" }, h("b", {}, "You do not know who added you."), h("span", {}, "Join only if you expect this. Members you do not know are shown as not verified.")) : null,
    h("div", { style: `display:grid;grid-template-columns:${d.can_block ? "1fr 1fr 1fr" : "1fr 1fr"};gap:10px` },
      d.can_block ? h("button", { class: "btn big danger", type: "button", onclick: () => decide("block") }, "Block") : null,
      h("button", { class: "btn big", type: "button", onclick: () => decide("decline") }, "Decline"),
      h("button", { class: "btn primary big", type: "button", onclick: () => decide("accept") }, "Join the group")),
    h("p", { class: "note" }, d.can_block ? "Decline: they are not told, and anyone can invite you again. Block also drops anything this person sends you from now on. Your decision reaches your other devices." : d.large ? "If you decline, they are not told, and anyone can invite you again. This device only: your other devices are not asked." : "If you decline, they are not told, and anyone can invite you again. Your decision reaches your other devices.")))];
}
function segChoice(label, value, opts, onpick, disabled) {
  return h("div", { class: "optrow" }, h("b", { style: "font-weight:600" }, label),
    h("div", { class: "seg", role: "radiogroup", "aria-label": label }, opts.map(([v, l]) => h("button", { type: "button", role: "radio", "aria-checked": String(value === v), "aria-selected": String(value === v), disabled: disabled || null, onclick: () => onpick(v) }, l))));
}
function pickRow(c, checked, onchange) {
  return h("label", { class: "pick" }, h("input", { type: "checkbox", checked: checked || null, onchange }), tile(c, true),
    h("span", { style: "flex:1;min-width:0" }, h("b", { style: "font-weight:600" }, c.name), h("div", { class: "note" }, c.agent ? "Agent: can post; its approvals stay private to you" : c.verified ? "Verified" : "Not verified: you can add them, and the group will show it")),
    c.verified ? null : h("span", { class: "tag bad" }, "Not verified"));
}
function groupScreen() {
  const d = st.gdetail, back = () => { st.screen = null; render(); };
  if (!d) return [header("Group", back), h("p", { class: "empty" }, "This group is not available.")];
  const joined = d.status === "joined";
  const act = async (cmd, args, msg) => { try { const r = await invoke(cmd, { id: d.id, ...args }); await refresh(); if (msg) toast(msg); if (r && r.notes) r.notes.forEach(toast); } catch (e) { toast(String(e)); } };
  const nameBox = (m) => {
    const inp = h("input", { "aria-label": "Name for this person", placeholder: "A name for them", maxlength: "64" });
    return h("span", { style: "display:flex;gap:8px;align-items:center;margin-top:4px" }, h("div", { class: "field", style: "flex:1" }, inp),
      h("button", { class: "btn primary", type: "button", onclick: async () => { try { toast(await invoke("add_member_as_contact", { owner: m.owner, name: inp.value })); st.nameMember = null; await refresh(); } catch (e) { toast(String(e)); } } }, "Add"),
      h("button", { class: "btn", type: "button", onclick: () => { st.nameMember = null; render(); } }, "Cancel"));
  };
  const memberRow = (m) => h("div", { class: "member" }, h("span", { class: `tile sm ${m.you ? "c-neutral" : tint(m.name)}`, "aria-hidden": "true" }, memLetter(m)),
    h("span", { class: "body" }, h("span", { style: "display:flex;gap:8px;align-items:center" }, h("b", { style: "font-weight:600" }, m.you ? "You" : m.name), m.admin ? h("span", { class: "tag plain" }, "admin") : null),
      h("span", { class: "note" }, m.you ? "Your devices" : m.verified ? "Verified" : m.contact ? "Not verified" : "Not in your contacts · not verified"),
      !m.you && m.fingerprint ? h("span", { class: "note mono" }, m.fingerprint) : null,
      st.nameMember === m.owner ? nameBox(m) : null),
    joined && !m.you ? h("span", { style: "display:flex;gap:6px;flex-wrap:wrap;justify-content:flex-end" },
      m.contact && !m.verified ? h("button", { class: "btn", type: "button", onclick: () => { st.verifyFor = m.name; st.verifyDone = null; st.screen = "verify"; render(); } }, "Verify") : null,
      !m.contact ? h("button", { class: "btn", type: "button", onclick: () => { st.nameMember = m.owner; render(); } }, "Add as contact") : null,
      d.me_admin && !m.admin ? h("button", { class: "btn quiet", type: "button", onclick: () => act("group_make_admin", { owner: m.owner }, `${m.name} is an admin now.`) }, "Make admin") : null,
      d.me_admin ? h("button", { class: "btn danger", type: "button", onclick: () => { if (confirm(`Remove ${m.name}?\n\n${d.large ? "The group moves to a mailbox they cannot reach." : "New messages stop being encrypted for their devices."} They keep what they already received.`)) act("group_remove", { owner: m.owner }, `${m.name} removed.`); } }, "Remove") : null) : null);
  const room = d.max - d.members.length;
  const addable = st.ov.contacts.filter((c) => !d.members.some((m) => m.name === c.name));
  const picker = st.picking ? h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Add people"),
    addable.length ? addable.map((c) => pickRow(c, st.picked.includes(c.name), (e) => {
      if (e.target.checked && st.picked.length >= room) { e.target.checked = false; return toast(`${d.large ? "Large" : "Small"} groups are limited to ${d.max} people.`); }
      st.picked = e.target.checked ? [...st.picked, c.name] : st.picked.filter((x) => x !== c.name);
    })) : h("p", { class: "note", style: "margin:0" }, "Everyone in your contacts is already in this group."),
    h("div", { style: "display:flex;gap:8px" },
      h("button", { class: "btn primary", type: "button", onclick: async () => { const names = st.picked; if (!names.length) return toast("Choose at least one person."); st.picking = false; await act("group_add", { names }, "Added. They see messages from now on."); } }, "Add"),
      h("button", { class: "btn", type: "button", onclick: () => { st.picking = false; render(); } }, "Cancel"))) : null;
  return [header(d.name, back), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("section", { class: "card", style: "flex-direction:row;align-items:center;gap:14px" }, groupTile(false),
      h("span", { style: "flex:1;min-width:0;display:flex;flex-direction:column;gap:6px" },
        d.me_admin && joined ? h("div", { class: "field" }, h("input", { "aria-label": "Group name", value: d.name, onchange: (e) => { const v = e.target.value.trim(); if (v && v !== d.name) act("group_rename", { name: v }, `Renamed to “${v}”.`); else e.target.value = d.name; } })) : h("b", { style: "font-size:18px" }, d.name),
        h("span", { class: "note" }, `${d.members.length} of up to ${d.max} people · ${d.unverified ? d.unverified + " not verified" : "everyone verified"}`))),
    h("section", { class: "card", style: "gap:0" },
      h("div", { style: "display:flex;align-items:center;padding-bottom:6px" }, h("h2", { class: "eyebrow", style: "flex:1" }, `Members (${d.members.length})`),
        joined ? (d.can_add ? h("button", { class: "btn", type: "button", onclick: () => { st.picking = !st.picking; st.picked = []; render(); } }, icon("addperson"), "Add people") : h("span", { class: "note" }, d.members.length >= d.max ? `A ${d.large ? "large" : "small"} group holds up to ${d.max} people` : "Only admins can add people")) : null),
      d.members.map(memberRow)),
    picker,
    joined ? h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Group settings"), d.me_admin ? null : h("span", { class: "note" }, "Only admins can change these."),
      segChoice("Who can add people", d.add_by, [["admins", "Admins only"], ["any", "Any member"]], (v) => act("group_set_add_by", { any: v === "any" }, v === "any" ? "Any member can add people now." : "Only admins can add people now."), !d.me_admin),
      d.large ? null : segChoice("New members see", d.history, HISTORY_CHOICES, (v) => act("group_set_history", { week: v === "week" }, v === "week" ? "New members can ask for the last 7 days." : "New members see messages from when they join."), !d.me_admin),
      d.large ? null : h("span", { class: "note" }, historyNote(d.history))) : null,
    d.large ? h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "How this group is encrypted"),
      h("span", {}, "A large group uses MLS (RFC 9420). Each message is encrypted once for the whole group and stored once on the relay. The relay sees that a group exists, its size and timing, and which devices read it, but not its name, members or content."),
      h("span", { class: "note" }, "Only admins add or remove people. When someone is removed the group moves to a new mailbox they cannot reach; they keep what they already received. Large groups are experimental: they have not been independently reviewed, and files and history for new members are not available yet.")) :
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "How this group is encrypted"),
      h("span", {}, "Each message is encrypted separately for every device of every member, over your one-to-one sessions. The relay sees separate messages, not a group, and not who is in it."),
      h("span", { class: "note" }, "The member list is signed by whoever changes it. If two admins change it at once, every app picks the same winner. Someone you remove keeps what they already received. For more than 20 people, make a large group.")),
    joined ? h("button", { class: "btn danger", style: "align-self:flex-start", type: "button", onclick: async () => {
      if (!confirm(`Leave ${d.name}?\n\n${d.large ? "An admin removes you from the group's keys the next time they sync; until then you are still a member, and you stop reading and writing here at once. " : ""}Your history stays on this computer. ${d.large ? "An admin can invite you again." : "Nobody can add you back without your OK."}`)) return;
      try { await invoke("group_leave", { id: d.id }); st.screen = null; await refresh(); toast(`You left ${d.name}.`); } catch (e) { toast(String(e)); }
    } }, "Leave group") : null))];
}
function newGroupScreen() {
  const f = st.newGroup, back = () => { st.screen = null; st.newGroup = null; render(); };
  const sel = f.members.map((n) => st.ov.contacts.find((c) => c.name === n)).filter(Boolean), unv = sel.filter((c) => !c.verified);
  const ok = () => f.name.trim() && f.members.length;
  const nameIn = h("input", { "aria-label": "Group name", placeholder: "e.g. Pilot team", value: f.name, maxlength: "100", oninput: (e) => { f.name = e.target.value; const b = $("createGroup"); if (b) b.disabled = !ok(); } });
  return [header("New group", back), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    segChoice("Size", f.large ? "large" : "small", [["small", `Small (up to ${GROUP_MAX})`], ["large", "Large (up to 500)"]], (v) => { f.large = v === "large"; render(); }),
    h("p", { class: "lead" }, f.large ? "A large group: up to 500 people, one encrypted copy of each message for everyone. People you add get an invitation and join when they accept. Experimental: not independently reviewed yet." : `A small group: up to ${GROUP_MAX} people. Every message is encrypted separately for each member's devices.`),
    h("label", { class: "lbl", style: "font-weight:600" }, "Group name"), h("div", { class: "field" }, nameIn),
    h("section", { class: "card", style: "gap:2px" }, h("h2", { class: "eyebrow", style: "padding-bottom:6px" }, `Members · ${sel.length} selected`),
      st.ov.contacts.map((c) => pickRow(c, f.members.includes(c.name), (e) => {
        if (!f.large && e.target.checked && f.members.length >= GROUP_MAX - 1) { e.target.checked = false; return toast(`Small groups are limited to ${GROUP_MAX} people, including you.`); }
        f.members = e.target.checked ? [...f.members, c.name] : f.members.filter((x) => x !== c.name); render();
      }))),
    unv.length ? h("div", { class: "status soft" }, h("span", {}, h("b", {}, unv.map((c) => c.name).join(", ")), unv.length > 1 ? " are" : " is", " not verified. You can still add them; everyone in the group will see it.")) : null,
    f.large ? h("div", { class: "status soft" }, h("span", {}, "You will be the only admin. Only admins add or remove people. Everyone you add needs to have opened this app at least once.")) : h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Settings (you will be admin)"),
      segChoice("Who can add people", f.anyCanAdd ? "any" : "admins", [["admins", "Admins only"], ["any", "Any member"]], (v) => { f.anyCanAdd = v === "any"; render(); }),
      segChoice("New members see", f.week ? "week" : "join", HISTORY_CHOICES, (v) => { f.week = v === "week"; render(); }),
      h("span", { class: "note" }, historyNote(f.week ? "week" : "join"))),
    h("button", { id: "createGroup", class: "btn primary big", type: "button", disabled: !ok() || null, onclick: async () => {
      try {
        if (f.large && f.members.length > GROUP_MAX - 1 && !confirm(`Make a large group with ${f.members.length + 1} people?\n\nLarge groups are experimental and have not been independently reviewed.`)) return;
        const r = f.large ? await invoke("group_create_large", { name: f.name, members: f.members }) : await invoke("group_create", { name: f.name, members: f.members, anyCanAdd: f.anyCanAdd, historyWeek: f.week });
        st.newGroup = null; st.screen = null; await refresh(); await openGroup(r.group); toast(f.large ? `Large group created. Invited ${r.reached} of ${r.people} people; they join when they accept.` : `Group created. Reached ${r.reached} of ${r.people} people.`); r.notes.forEach(toast);
      } catch (e) { toast(String(e)); }
    } }, "Create group")))];
}

const SECTIONS = { chats: { list: chatsList, main: chatMain }, agents: { main: agentsMain }, devices: { main: devicesMain }, settings: { main: settingsMain } };
const SCREENS = { group: groupScreen, newgroup: newGroupScreen, requests: requestsScreen, verify: verifyScreen, add: addScreen, grant: grantScreen, linkold: linkOldScreen, linknew: linkNewScreen };

document.addEventListener("keydown", (e) => {
  if (!(MAC ? e.metaKey : e.ctrlKey) || e.altKey || $("app").hidden) return;
  const k = { "1": "chats", "2": "agents", "3": "devices", "4": "settings" }[e.key];
  if (k) { e.preventDefault(); go(k); } else if (e.key.toLowerCase() === "l") { e.preventDefault(); lockNow(); }
});
window.addEventListener("DOMContentLoaded", () => { listenToBackend(); gate(); });
