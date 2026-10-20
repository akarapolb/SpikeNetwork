"use strict";
/* Spike Connect desktop UI. No framework, no build step, no remote content (see the CSP in tauri.conf.json).
   Every value from the backend is inserted as text (createTextNode), never as HTML.
   Keys never reach this file: the backend only returns names, message text and request summaries. */

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
  shield: '<path d="M12 3l7 3v6c0 4-3 7-7 9-4-2-7-5-7-9V6z"/><path d="M9 12l2 2 4-4"/>',
  send: '<path d="M4 12l16-8-6 16-2-7z"/>',
  file: '<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/>',
  devices: '<rect x="3" y="5" width="12" height="9" rx="1.5"/><path d="M1.5 17h15"/><rect x="17" y="8" width="5.5" height="12" rx="1.2"/>',
  clip: '<path d="M21 12l-8.5 8.5a5 5 0 0 1-7-7L14 5a3.5 3.5 0 0 1 5 5l-8.5 8.5a2 2 0 0 1-3-3L15 8"/>',
  moon: '<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>',
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
const st = { linkNew: false, linking: false, linkStep: 0, linkCode: "", linkSafety: "", linkMode: "all", linkErr: "", grantForm: null, section: "chats", chat: null, screen: null, ov: null, msgs: [], agents: null, offline: false, verifyDone: null, addMsg: null, polling: null, busy: false };

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
    h("p", { class: "note" }, "Unreviewed prototype software: do not use it for real secrets yet.")));
  pass.focus();
}
async function openApp() { $("gate").hidden = true; $("gate").replaceChildren(); $("app").hidden = false; st.chat = null; st.screen = null; await refresh(); if (st.linkNew) { st.linkNew = false; st.section = "devices"; st.screen = "linknew"; render(); startLinkNew(); } else syncNow(); st.polling = setInterval(() => { if (!document.hidden) syncNow(); }, 15000); }
function stop() { clearInterval(st.polling); st.polling = null; st.ov = st.agents = null; st.msgs = []; }

/* ---------- data ---------- */
async function refresh() {
  try {
    st.ov = await invoke("overview");
    st.agents = await invoke("agents");
    if (st.chat) st.msgs = await invoke("conversation", { name: st.chat });
  } catch (e) { if (e === "locked") return gate("unlock"); toast(String(e)); }
  render();
}
async function syncNow() {
  if (st.busy || st.linking) return; st.busy = true;
  try {
    const r = await invoke("sync");
    st.offline = false;
    r.notes.forEach(toast);
    if (r.received || r.agent_requests) await refresh(); else render();
    await markOpenRead();
  } catch (e) { if (e === "locked") return gate("unlock"); st.offline = true; render(); }
  finally { st.busy = false; }
}

/* ---------- shell ---------- */
function go(section) { st.section = section; st.screen = null; render(); }
function renderRail() {
  const unread = st.ov ? st.ov.contacts.reduce((a, c) => a + c.unread, 0) : 0, w = st.ov ? st.ov.waiting : 0;
  const keys = { chats: 1, agents: 2, devices: 3, settings: 4 };
  const item = (k, label, n, warn) => h("button", { type: "button", "aria-current": st.section === k ? "page" : null, "aria-label": n ? `${label}, ${n} ${warn ? "waiting" : "unread"}` : label, onclick: () => go(k) },
    icon(k), h("span", { class: "tip", "aria-hidden": "true" }, `${label}  Ctrl+${keys[k]}`), n ? h("span", { class: "badge" + (warn ? " warn" : ""), "aria-hidden": "true" }, n) : null);
  $("rail").replaceChildren(
    h("span", { class: "tile sm c-neutral me", title: "This device", "aria-hidden": "true" }, icon("shield")),
    item("chats", "Chats", unread), item("agents", "Agents", w, true), item("devices", "Devices"), item("settings", "Settings"),
    h("span", { class: "spacer" }),
    h("button", { type: "button", "aria-label": "Switch light or dark", onclick: () => { const r = document.documentElement; const dark = matchMedia("(prefers-color-scheme: dark)").matches; r.dataset.theme = (r.dataset.theme || (dark ? "dark" : "light")) === "dark" ? "light" : "dark"; } }, icon("moon"), h("span", { class: "tip", "aria-hidden": "true" }, "Light or dark")),
    h("span", { class: "sep", "aria-hidden": "true" }),
    h("button", { type: "button", "aria-label": "Lock", onclick: lockNow }, icon("lock"), h("span", { class: "tip", "aria-hidden": "true" }, "Lock  Ctrl+L")));
}
async function lockNow() { try { await invoke("lock"); } catch (_) {} gate("unlock"); }
function render() {
  if (!st.ov) return;
  renderRail();
  const S = SECTIONS[st.section], items = S.list ? S.list() : [];
  $("list").replaceChildren(...items);
  $("shell").classList.toggle("nolist", !items.length);
  const main = st.screen ? SCREENS[st.screen]() : S.main();
  $("main").replaceChildren(...(st.offline ? [h("div", { class: "err-banner", role: "status" }, "Cannot reach the relay at ", h("b", {}, st.ov.relay), ". Messages are kept and sent when it is back. Check Settings.")] : []), ...main);
}
function header(title, back) { return h("div", { class: "head" }, back ? h("button", { class: "iconbtn", type: "button", "aria-label": "Back", onclick: back }, icon("back")) : null, h("h1", {}, title)); }
function tile(c, sm) { return h("span", { class: `tile${sm ? " sm" : ""}${c.agent ? " sq" : ""} c-neutral`, "aria-hidden": "true" }, ini(c.name)); }

/* ---------- chats ---------- */
function chatsList() {
  const cs = st.ov.contacts;
  return [h("div", { class: "listhead" }, h("div", { class: "title" }, h("h1", {}, "Chats"), h("button", { class: "iconbtn", type: "button", "aria-label": "Add a contact", title: "Add a contact", onclick: () => { st.screen = "add"; st.addMsg = null; render(); } }, icon("addperson")))),
    h("nav", { class: "rows", "aria-label": "Conversations" },
      cs.length ? cs.map((c) => h("button", { class: "row", type: "button", "aria-current": String(!st.screen && st.chat === c.name), onclick: () => openChat(c.name) }, tile(c),
        h("span", { class: "body" }, h("span", { class: "line1" }, h("b", {}, c.name), c.agent ? h("span", { class: "tag plain" }, "agent") : null, c.verified ? null : h("span", { class: "tag bad" }, "Not verified")), h("span", { class: "preview" }, c.last || "No messages yet")),
        h("span", { class: "meta" }, c.last_ts ? clock(c.last_ts) : "", c.unread ? h("span", { class: "count" }, c.unread) : null)))
        : h("p", { class: "empty" }, "No contacts yet. Add one with the button above: paste their identity card.")),
  ];
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
  st.chat = name; st.screen = null; st.verifyDone = null;
  try { st.msgs = await invoke("conversation", { name }); } catch (e) { toast(String(e)); }
  render(); scrollEnd();
  const c = st.ov.contacts.find((x) => x.name === name);
  if (c && c.unread) { try { await invoke("mark_read", { name }); await refresh(); } catch (_) {} }
}
// A message that arrives in the conversation you are looking at is read.
async function markOpenRead() {
  if (st.section !== "chats" || st.screen || !st.chat || document.hidden) return;
  const c = st.ov && st.ov.contacts.find((x) => x.name === st.chat);
  if (!c || !c.unread) return;
  try { await invoke("mark_read", { name: st.chat }); st.ov = await invoke("overview"); st.msgs = await invoke("conversation", { name: st.chat }); render(); scrollEnd(); } catch (_) {}
}
function scrollEnd() { const s = $("main").querySelector(".scroll"); if (s) s.scrollTop = s.scrollHeight; }
function chatMain() {
  const c = st.ov.contacts.find((x) => x.name === st.chat);
  if (!c) return [h("div", { class: "head" }, h("h1", {}, "Chats")), h("p", { class: "empty" }, st.ov.contacts.length ? "Choose a conversation." : "Add a contact to start.")];
  const head = h("div", { class: "head" }, tile(c, true), h("span", { class: "who" }, h("b", {}, c.name), h("span", { class: c.agent ? "verified" : null }, c.agent ? [icon("shield"), "Agent"] : `End-to-end encrypted · ${c.devices} device${c.devices > 1 ? "s" : ""}`)),
    h("button", { class: c.verified ? "btn quiet" : "btn", type: "button", onclick: () => { st.screen = "verify"; st.verifyDone = null; render(); } }, c.verified ? "Safety number" : "Verify"));
  const items = []; let day = "";
  for (const m of st.msgs) {
    const d = dayOf(m.ts); if (d !== day) { day = d; items.push(h("div", { class: "day" }, d)); }
    items.push(h("div", { class: "msg" + (m.out ? " out" : "") },
      m.deleted ? h("div", { class: "bubble" }, h("i", {}, "Message deleted")) : (m.body ? h("div", { class: "bubble" }, m.body) : null),
      m.file ? h("div", { class: "file" }, h("span", { class: "fi" }, icon("file")), h("span", { style: "flex:1;min-width:0" }, h("div", { style: "font-weight:600" }, m.file.name), h("div", { class: "note" }, fmtSize(m.file.size))), h("button", { class: "btn", type: "button", onclick: () => saveFile(m) }, "Save")) : null,
      h("span", { class: "meta" }, clock(m.ts), m.edited ? " · edited" : "")));
  }
  const picker = h("input", { type: "file", hidden: true, onchange: async (e) => { const f = e.target.files[0]; e.target.value = ""; if (f) await sendFile(c.name, f, ta.value); } });
  const ta = h("textarea", { "aria-label": "Message to " + c.name, placeholder: "Message", rows: "1", onkeydown: (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } } });
  async function send() {
    const text = ta.value; if (!text.trim()) return; ta.disabled = true;
    try { const n = await invoke("send", { name: c.name, text }); ta.value = ""; if (!n) toast("Saved, but no device could be reached yet. It will be delivered when they connect."); await refresh(); scrollEnd(); }
    catch (e) { toast(String(e)); }
    finally { const t = $("main").querySelector("textarea"); if (t) { t.disabled = false; t.focus(); } }
  }
  return [head, h("div", { class: "scroll" }, items.length ? h("div", { class: "thread" }, items) : h("p", { class: "empty" }, c.verified ? "No messages yet." : "No messages yet. Verify this contact first: compare their safety number over a call or in person.")),
    h("div", { class: "composer" }, h("div", { class: "bar" }, h("button", { class: "iconbtn", type: "button", "aria-label": "Attach a file", title: "Attach a file", onclick: () => picker.click() }, icon("clip")), picker, ta, h("button", { class: "iconbtn", type: "button", "aria-label": "Send", style: "background:var(--accent);color:var(--accent-ink);border:0", onclick: send }, icon("send"))), h("span", { class: "note" }, `Encrypted separately for each of ${c.name}'s devices.`))];
}

/* ---------- verify + add ---------- */
function verifyScreen() {
  const c = st.ov.contacts.find((x) => x.name === st.chat); const first = c.name.split(" ")[0];
  const done = st.verifyDone;
  return [header(`Verify ${c.name}`, () => { st.screen = null; render(); }), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("p", { class: "lead" }, `Compare this with ${first} in person or on a call you trust, reading it aloud. If it matches on both sides, nobody is in the middle.`),
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Safety number"), h("div", { class: "mono", style: "font-size:20px;overflow-wrap:anywhere;padding:10px;background:var(--panel-2);border-radius:12px;text-align:center" }, c.fingerprint)),
    c.verified && !done ? h("div", { class: "status soft" }, h("b", {}, `${c.name} is verified.`), h("span", {}, "If this number ever changes, you will be warned.")) : null,
    done === "match" ? h("div", { class: "status good", role: "status" }, h("b", {}, `${c.name} is verified.`)) : null,
    done === "nomatch" ? h("div", { class: "status bad", role: "status" }, h("b", {}, "Do not trust this conversation yet."), h("span", {}, `Someone may be in the middle. Ask ${first} to show their number in person.`)) : null,
    !c.verified && !done ? h("div", { style: "display:grid;grid-template-columns:1fr 1fr;gap:10px" },
      h("button", { class: "btn big", type: "button", onclick: () => { st.verifyDone = "nomatch"; render(); } }, "They don't match"),
      h("button", { class: "btn primary big", type: "button", onclick: async () => { try { await invoke("mark_verified", { name: c.name, fingerprint: c.fingerprint }); st.verifyDone = "match"; await refresh(); } catch (e) { toast(String(e)); } } }, "They match")) : null))];
}
function addScreen() {
  const name = h("input", { "aria-label": "Name", placeholder: "Name, e.g. Bob Chen" }), card = h("textarea", { class: "cardbox", "aria-label": "Identity card", placeholder: "Paste their identity card (starts with spike2:)" });
  return [header("Add a contact", () => { st.screen = null; render(); }), h("div", { class: "scroll" }, h("div", { class: "page narrow" },
    h("p", { class: "lead" }, "There is no phone-number or email lookup. You swap cards directly, so no server learns who knows whom."),
    h("label", { style: "font-weight:600" }, "Name"), h("div", { class: "field" }, name),
    h("label", { style: "font-weight:600" }, "Their identity card"), card,
    h("button", { class: "btn primary big", type: "button", onclick: async () => { try { st.addMsg = await invoke("add_contact", { name: name.value, card: card.value }); const n = name.value.trim(); await refresh(); st.screen = null; st.section = "chats"; await openChat(n); toast(st.addMsg); } catch (e) { toast(String(e)); } } }, "Add contact"),
    h("p", { class: "note" }, "They arrive as not verified until you compare safety numbers. Your own card is in Settings.")))];
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
        d.this ? h("span", { class: "pill neutral" }, "You are here") : null))),
      h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Your identity"), h("span", { class: "mono", style: "font-size:16px;overflow-wrap:anywhere" }, me.fingerprint), h("span", { class: "note" }, "Contacts compare this when they verify you. It signs your device list; it never signs messages.")),
      h("div", { class: "status soft" }, h("b", {}, "Encrypted backup is not available yet."), h("span", {}, "Keep at least one other device linked: if you lose every device and the passphrase, the identity is gone. Removing a device is not in the app yet.")))) ];
}
// NEW device: show a code, wait for the other device, compare, receive.
async function startLinkNew() {
  st.linking = true; st.linkStep = 1; st.linkErr = ""; st.linkSafety = "";
  try { st.linkCode = await invoke("link_offer"); } catch (e) { st.linkErr = String(e); st.linking = false; return render(); }
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
function settingsMain() {
  const me = st.ov.me, relay = h("input", { "aria-label": "Relay address", value: st.ov.relay, class: "mono" });
  return [h("div", { class: "head" }, h("h1", {}, "Settings")), h("div", { class: "scroll" }, h("div", { class: "page" },
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Your identity"), h("span", { class: "mono", style: "font-size:16px;overflow-wrap:anywhere" }, me.fingerprint),
      h("span", { class: "note" }, `${me.devices} device${me.devices > 1 ? "s" : ""}${me.approver ? " · this device approves agent requests" : ""}. Contacts compare this when they verify you.`),
      h("h2", { class: "eyebrow", style: "margin-top:8px" }, "Your identity card"), h("textarea", { class: "cardbox mono", readonly: true, "aria-label": "Your identity card", onfocus: (e) => e.target.select() }, me.card),
      h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: async () => { try { await navigator.clipboard.writeText(me.card); toast("Card copied. Send it to your contact."); } catch (_) { toast("Select the text above and copy it."); } } }, "Copy card")),
    h("section", { class: "card" }, h("h2", { class: "eyebrow" }, "Home relay"), h("div", { class: "field" }, relay),
      h("span", { class: "note" }, "wss:// for a real relay; ws:// only for one on this computer. A relay sees which devices connect and message sizes, never text or files."),
      h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: async () => { try { await invoke("set_relay", { url: relay.value }); toast("Relay saved."); await refresh(); syncNow(); } catch (e) { toast(String(e)); } } }, "Save relay")),
    h("div", { class: "status soft" }, h("b", {}, "Not built in the app yet:"), h("span", {}, "edit and delete, removing a device, groups and calls, message requests (some are in the CLI, some only planned). This is an unreviewed prototype.")),
    h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: lockNow }, icon("lock"), "Lock now")))];
}

const SECTIONS = { chats: { list: chatsList, main: chatMain }, agents: { main: agentsMain }, devices: { main: devicesMain }, settings: { main: settingsMain } };
const SCREENS = { verify: verifyScreen, add: addScreen, grant: grantScreen, linkold: linkOldScreen, linknew: linkNewScreen };

document.addEventListener("keydown", (e) => {
  if (!e.ctrlKey || e.altKey || $("app").hidden) return;
  const k = { "1": "chats", "2": "agents", "3": "devices", "4": "settings" }[e.key];
  if (k) { e.preventDefault(); go(k); } else if (e.key.toLowerCase() === "l") { e.preventDefault(); lockNow(); }
});
window.addEventListener("DOMContentLoaded", () => gate());
