"use strict";
/* Spike Connect desktop UI. No framework, no build step, no remote content (see the CSP in tauri.conf.json).
   Every value from the backend is inserted as text (createTextNode), never as HTML.
   Keys never reach this file: the backend only returns names, message text and request summaries. */

const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
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
const st = { section: "chats", chat: null, screen: null, ov: null, msgs: [], agents: null, offline: false, verifyDone: null, addMsg: null, polling: null, busy: false };

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
    h("p", { class: "note" }, "Unreviewed prototype software: do not use it for real secrets yet.")));
  pass.focus();
}
async function openApp() { $("gate").hidden = true; $("gate").replaceChildren(); $("app").hidden = false; st.chat = null; st.screen = null; await refresh(); syncNow(); st.polling = setInterval(() => { if (!document.hidden) syncNow(); }, 15000); }
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
  if (st.busy) return; st.busy = true;
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
  const keys = { chats: 1, agents: 2, settings: 3 };
  const item = (k, label, n, warn) => h("button", { type: "button", "aria-current": st.section === k ? "page" : null, "aria-label": n ? `${label}, ${n} ${warn ? "waiting" : "unread"}` : label, onclick: () => go(k) },
    icon(k), h("span", { class: "tip", "aria-hidden": "true" }, `${label}  Ctrl+${keys[k]}`), n ? h("span", { class: "badge" + (warn ? " warn" : ""), "aria-hidden": "true" }, n) : null);
  $("rail").replaceChildren(
    h("span", { class: "tile sm c-neutral me", title: "This device", "aria-hidden": "true" }, icon("shield")),
    item("chats", "Chats", unread), item("agents", "Agents", w, true), item("settings", "Settings"),
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
      m.file ? h("div", { class: "file" }, h("span", { class: "fi" }, icon("file")), h("span", { style: "flex:1;min-width:0" }, h("div", { style: "font-weight:600" }, m.file.name), h("div", { class: "note" }, `${Math.max(1, Math.round(m.file.size / 1024))} KB · attachments open from the command line for now`))) : null,
      h("span", { class: "meta" }, clock(m.ts), m.edited ? " · edited" : "")));
  }
  const ta = h("textarea", { "aria-label": "Message to " + c.name, placeholder: "Message", rows: "1", onkeydown: (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } } });
  async function send() {
    const text = ta.value; if (!text.trim()) return; ta.disabled = true;
    try { const n = await invoke("send", { name: c.name, text }); ta.value = ""; if (!n) toast("Saved, but no device could be reached yet. It will be delivered when they connect."); await refresh(); scrollEnd(); }
    catch (e) { toast(String(e)); }
    finally { const t = $("main").querySelector("textarea"); if (t) { t.disabled = false; t.focus(); } }
  }
  return [head, h("div", { class: "scroll" }, items.length ? h("div", { class: "thread" }, items) : h("p", { class: "empty" }, c.verified ? "No messages yet." : "No messages yet. Verify this contact first: compare their safety number over a call or in person.")),
    h("div", { class: "composer" }, h("div", { class: "bar" }, ta, h("button", { class: "iconbtn", type: "button", "aria-label": "Send", style: "background:var(--accent);color:var(--accent-ink);border:0", onclick: send }, icon("send"))), h("span", { class: "note" }, `Encrypted separately for each of ${c.name}'s devices.`))];
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
      h("dt", {}, "Typed only"), h("dd", {}, g.typed_only ? "Yes: free text is refused" : "No: free text is allowed but marked unchecked"), h("dt", {}, "Ends"), h("dd", {}, new Date(g.expires).toLocaleDateString())));
  return [h("div", { class: "head" }, h("h1", { style: "flex:none" }, "Agent requests"), h("span", { class: "tag " + (w.length ? "waiting" : "neutral") }, w.length ? `${w.length} waiting` : "nothing waiting"), h("span", { class: "spacer" }), a.approver ? null : h("span", { class: "note" }, "This is not your main device: decisions are forwarded to it")),
    h("div", { class: "scroll" }, h("div", { class: "page" },
      w.length || done.length ? [w.map(approvalCard), done.length ? h("h2", { class: "eyebrow", style: "margin:8px 0 0" }, "Decided") : null, done.map(approvalCard)] : h("div", { class: "status plain" }, h("b", {}, "No requests yet"), h("span", { class: "note" }, "Give an agent a standing permission with `spike agent-grant`; its requests appear here. Creating grants from this window comes later.")),
      a.grants.map(grant)))];
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
    h("div", { class: "status soft" }, h("b", {}, "Not built in the app yet:"), h("span", {}, "devices and linking, attachments, edit and delete, groups and calls (all in the CLI or planned). This is an unreviewed prototype.")),
    h("button", { class: "btn", style: "align-self:flex-start", type: "button", onclick: lockNow }, icon("lock"), "Lock now")))];
}

const SECTIONS = { chats: { list: chatsList, main: chatMain }, agents: { main: agentsMain }, settings: { main: settingsMain } };
const SCREENS = { verify: verifyScreen, add: addScreen };

document.addEventListener("keydown", (e) => {
  if (!e.ctrlKey || e.altKey || $("app").hidden) return;
  const k = { "1": "chats", "2": "agents", "3": "settings" }[e.key];
  if (k) { e.preventDefault(); go(k); } else if (e.key.toLowerCase() === "l") { e.preventDefault(); lockNow(); }
});
window.addEventListener("DOMContentLoaded", () => gate());
