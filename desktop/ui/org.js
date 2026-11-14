/* Organisations (ADR 0035): the console for the computer that holds an organisation's admin key, joining an
   organisation's relay, and the rules shown to members. The console sees device ids, the day each enrolled, a
   coarse activity bucket and tenant totals; there are no names, contacts or message content to read. */

const ORG_NAV = [["overview", "Overview"], ["devices", "Devices"], ["enrolment", "Enrolment"], ["limits", "Limits"], ["usage", "Plan and usage"], ["keys", "Admin keys"]];
const ACTIVITY = { active: "active this week", idle: "idle 1 to 4 weeks", dormant: "idle 30+ days" };
const LOCK_RULES = [[0, "No rule"], [1, "1 minute"], [5, "5 minutes"], [15, "15 minutes"], [30, "30 minutes"], [60, "1 hour"]];
const KEEP_RULES = [[0, "No rule"], [7, "7 days"], [30, "30 days"], [90, "90 days"], [365, "1 year"]];
const BACKUP_RULES = [["", "No rule"], ["allowed", "Allowed"], ["key_required", "Allowed, key required"], ["off", "Not allowed"]];
const AGENT_RULES = [["", "No rule"], ["any", "Any"], ["verified_only", "Verified only"], ["off", "Not allowed"]];
const ruleWord = (list, v) => (list.find(([k]) => k === (v ?? (typeof list[0][0] === "number" ? 0 : ""))) || [null, String(v)])[1];

function bytesText(n) {
  const G = 1024 ** 3, M = 1024 ** 2;
  if (n >= G) return [(n / G).toFixed(1).replace(/\.0$/, ""), "GB"];
  if (n >= M) return [(n / M).toFixed(1).replace(/\.0$/, ""), "MB"];
  if (n >= 1024) return [Math.round(n / 1024), "KB"];
  return [n, "bytes"];
}
const dayText = (d) => new Date(d * 86400000).toLocaleDateString(undefined, { day: "numeric", month: "short" });
const devGroups = (id) => id.slice(0, 10).replace(/(.{5})/, "$1 ");
function untilText(secs) {
  const d = new Date(secs * 1000), now = new Date();
  const tomorrow = new Date(now); tomorrow.setDate(now.getDate() + 1);
  const t = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === now.toDateString()) return `Today ${t}`;
  if (d.toDateString() === tomorrow.toDateString()) return `Tomorrow ${t}`;
  return `${d.toLocaleDateString(undefined, { day: "numeric", month: "short" })} ${t}`;
}

/* Loaded with refresh(): the admin state (local) and this device's membership (local). */
async function refreshOrg() {
  try { st.orgAdmin = await invoke("org_admin"); } catch (_) { st.orgAdmin = null; }
  try { st.org = await invoke("org_status"); } catch (_) { st.org = null; }
}
async function loadConsole() {
  const o = st.oc || (st.oc = {});
  o.busy = true;
  try { o.data = await invoke("org_console"); o.err = ""; } catch (e) { o.err = String(e); }
  o.busy = false; o.loaded = true; render();
}

/* The rules in force on this computer (none outside an organisation or once removed). */
function orgRules() { return st.org && !st.org.removed ? st.org.rules : null; }

/* ---------- the console ---------- */
function orgList() { return []; }
function orgMain() {
  const a = st.orgAdmin;
  if (!a) return [h("div", { class: "scroll" }, h("div", { class: "page" }, emptyState("This computer holds no organisation admin key. Create one in Settings, Organisation console.", "settings")))];
  const o = st.oc || (st.oc = {});
  if (!o.loaded && !o.busy) loadConsole();
  const cur = ORG_NAV.some(([k]) => k === st.orgSec) ? st.orgSec : "overview";
  const nav = h("nav", { class: "setnav", "aria-label": "Organisation console" }, h("strong", {}, a.org_name),
    ORG_NAV.map(([k, label]) => h("a", { href: "#", "aria-current": k === cur ? "true" : null, onclick: (e) => { e.preventDefault(); st.orgSec = k; render(); } }, label)));
  const pane = { overview: orgOverview, devices: orgDevices, enrolment: orgEnrolment, limits: orgLimits, usage: orgUsage, keys: orgKeys }[cur];
  return [h("div", { class: "setwrap org" }, nav, h("div", { class: "scroll setpane" }, h("div", { class: "page flat orgpage" }, pane(a, o))))];
}

function orgWhere(a) {
  let host = a.relay; try { host = new URL(a.relay).host; } catch (_) {}
  return h("p", { class: "orgsub" }, `${host} · this month`);
}
function orgErr(o) {
  if (!o.err) return o.busy && !o.data ? h("p", { class: "note" }, "Asking the relay…") : null;
  return h("div", { class: "status warn", role: "alert" }, h("b", {}, "Cannot reach the organisation's relay."), h("span", {}, o.err),
    h("button", { class: "btn", type: "button", onclick: loadConsole }, "Try again"));
}
function meter(used, limit) {
  if (!limit) return null;
  const pct = Math.min(100, Math.round((used / limit) * 100));
  return h("div", { class: "um" + (pct >= 80 ? " hot" : "") }, h("i", { style: `width:${pct}%` }));
}

function orgOverview(a, o) {
  const d = o.data;
  const stat = (label, big, small, bar) => h("div", {}, h("span", {}, label), h("b", {}, big, small ? h("small", {}, " " + small) : null), bar);
  const [sv, su] = d ? bytesText(d.stored_bytes) : ["–", ""];
  const [lv, lu] = d && d.storage_limit ? bytesText(d.storage_limit) : [null, null];
  const left = d && d.device_limit ? d.device_limit - d.device_count : null;
  const up = d ? Math.floor(d.relay_uptime_secs / 86400) : 0;
  return [h("h1", {}, "Overview"), orgWhere(a), orgErr(o),
    h("div", { class: "stats" },
      stat("Devices enrolled", d ? String(d.device_count) : "–", d && d.device_limit ? `of ${d.device_limit}` : d ? "no limit" : "", d ? meter(d.device_count, d.device_limit) : null),
      stat("Stored for offline devices", String(sv), lv != null ? `${su === lu ? "" : su + " "}of ${lv} ${lu}` : su, d ? meter(d.stored_bytes, d.storage_limit) : null),
      stat("Group call minutes", "–", "not available yet"),
      stat("Delivered within target", "–", "not measured yet")),
    h("div", { class: "oset" }, h("span", { class: "okdot" + (o.err ? " bad" : d && d.state !== "active" ? " w" : "") }),
      h("div", { class: "t" }, h("b", {}, "Relay"), h("span", {}, o.err ? "Not reachable from this computer" : d ? (d.state === "active" ? `Healthy · answered in ${d.rtt_ms} ms` : "Suspended by the relay operator: devices are refused") : "…")),
      d ? h("span", { class: "v" }, up ? `Up ${up} day${up > 1 ? "s" : ""}` : "Up today") : null),
    d && d.device_limit ? h("div", { class: "oset" }, h("span", { class: "okdot" + (left <= d.device_limit * 0.2 ? " w" : "") }),
      h("div", { class: "t" }, h("b", {}, "Device limit"), h("span", {}, `${left} place${left === 1 ? "" : "s"} left. New devices are refused at ${d.device_limit}.`)),
      h("button", { class: "kbtn", type: "button", onclick: () => toast("The relay operator sets the limit. On a hosted relay, ask us; on your own, run spike-relay tenant limits.") }, "Raise limit")) : null,
    h("div", { class: "oset" }, h("div", { class: "t" }, h("b", {}, "What this console can see"), h("span", {}, "Device ids, counters and limits. There are no names, contacts or message content to read.")))];
}

function orgDevices(a, o) {
  const d = o.data;
  const cols = "grid-template-columns:1.3fr 1fr 1fr 70px";
  const confirm = st.orgRevoke;
  const revoke = async (id) => {
    try { await invoke("org_revoke", { device: id }); toast("Device removed. The relay refuses it and dropped what was waiting for it."); st.orgRevoke = null; await loadConsole(); }
    catch (e) { toast(String(e)); }
  };
  const rows = d ? d.devices.map((x) => h("div", { class: "trow", style: cols },
    h("span", { class: "mono", title: x.device }, devGroups(x.device)),
    h("span", {}, dayText(x.enrolled_day)),
    h("span", { class: x.activity === "active" ? "" : "mu" }, ACTIVITY[x.activity] || x.activity),
    confirm === x.device
      ? h("button", { class: "kbtn decl", type: "button", onclick: () => revoke(x.device) }, "Confirm")
      : h("button", { class: "kbtn decl", type: "button", "aria-label": `Revoke ${devGroups(x.device)}`, onclick: () => { st.orgRevoke = x.device; render(); } }, "Revoke"))) : [];
  return [h("div", { class: "orgtwo" },
    h("div", {}, h("h1", {}, "Devices"), h("p", { class: "orgsub" }, "Enrolment is about devices, never people."), orgErr(o),
      h("div", { class: "trow h cap", style: cols }, h("span", {}, "Device id"), h("span", {}, "Enrolled"), h("span", {}, "Activity"), h("span", {})),
      ...rows,
      d && !d.devices.length ? h("div", { class: "trow mu", style: "grid-template-columns:1fr" }, "No devices yet. Enrol one with a token.") : null,
      confirm ? h("p", { class: "note" }, "Revoking refuses that device from now on and deletes what is waiting for it on the relay. Its messages on the device stay.") : null),
    enrolWizard(a))];
}

function enrolWizard(a) {
  const w = st.orgTok || (st.orgTok = { hours: 24, label: "" });
  const make = async () => { try { w.made = await invoke("org_issue_token", { label: w.label || "", hours: Number(w.hours) }); st.orgAdmin = await invoke("org_admin"); render(); } catch (e) { toast(String(e)); } };
  const copy = async () => { try { await navigator.clipboard.writeText(w.made.token); toast("Token copied. Send it to one person; it works once."); } catch (_) { toast("Select the token text and copy it."); } };
  const hoursSel = h("select", { class: "ssel", "aria-label": "Valid for", onchange: (e) => { w.hours = Number(e.target.value); w.made = null; render(); } },
    [[1, "1 hour"], [24, "24 hours"], [72, "3 days"], [168, "7 days"]].map(([v, l]) => h("option", { value: String(v), selected: v === w.hours || null }, l)));
  return h("div", { class: "wiz orgwiz" },
    h("b", { class: "wh" }, "Enrol a device"),
    w.made ? cardQr(w.made.token) : h("div", { class: "qrph", "aria-hidden": "true" }),
    h("span", { class: "note" }, "Signed by your admin key. The person scans it in Settings, Home relay."),
    h("div", { class: "dv" }, "Valid for", w.made ? h("span", {}, untilText(w.made.not_after)) : hoursSel),
    h("div", { class: "dv" }, "Uses", h("span", {}, "1 device")),
    h("input", { class: "orglabel", "aria-label": "Label, kept on this computer", placeholder: "Label, kept on this computer", value: w.label, oninput: (e) => { w.label = e.target.value; } }),
    w.made ? h("div", { class: "row2" },
      h("button", { class: "kbtn pri", type: "button", onclick: copy }, "Copy token"),
      h("button", { class: "kbtn", type: "button", onclick: () => { w.made = null; w.label = ""; render(); } }, "New token"))
      : h("button", { class: "kbtn pri", type: "button", style: "align-self:flex-start", onclick: make }, "Make token"));
}

function orgEnrolment(a, o) {
  const now = Date.now() / 1000;
  const withdraw = async (id) => { try { await invoke("org_withdraw_token", { tokenId: id }); st.orgAdmin = await invoke("org_admin"); toast("Token withdrawn. The relay refuses it."); render(); } catch (e) { toast(String(e)); } };
  const cols = "grid-template-columns:1.4fr 1fr 1fr 80px";
  return [h("div", { class: "orgtwo" },
    h("div", {}, h("h1", {}, "Enrolment"), h("p", { class: "orgsub" }, "Each token enrols one device and works once. Labels stay on this computer; the relay never sees them."),
      h("div", { class: "trow h cap", style: cols }, h("span", {}, "Token"), h("span", {}, "Made"), h("span", {}, "Valid until"), h("span", {})),
      ...a.tokens.map((t) => {
        const expired = t.not_after < now;
        return h("div", { class: "trow", style: cols },
          h("span", {}, t.label || h("span", { class: "mono mu" }, t.token_id.slice(0, 10))),
          h("span", {}, new Date(t.issued_ms).toLocaleDateString(undefined, { day: "numeric", month: "short" })),
          h("span", { class: expired || t.withdrawn ? "mu" : "" }, t.withdrawn ? "withdrawn" : expired ? "expired" : untilText(t.not_after)),
          !t.withdrawn && !expired ? h("button", { class: "kbtn decl", type: "button", onclick: () => withdraw(t.token_id) }, "Withdraw") : h("span", {}));
      }),
      !a.tokens.length ? h("div", { class: "trow mu", style: "grid-template-columns:1fr" }, "No tokens yet.") : null,
      h("p", { class: "note" }, "A token that was used cannot be withdrawn: revoke the device instead.")),
    enrolWizard(a))];
}

function orgLimits(a, o) {
  const d = o.data;
  const r = st.orgRulesEdit || (st.orgRulesEdit = { ...(a.rules || {}) });
  const sel = (label, list, key) => h("select", { class: "ssel", "aria-label": label, onchange: (e) => { const v = e.target.value; r[key] = typeof list[0][0] === "number" ? (Number(v) || null) : (v || null); } },
    list.map(([v, l]) => h("option", { value: String(v), selected: (r[key] ?? (typeof v === "number" ? 0 : "")) === v || null }, l)));
  const publish = async () => {
    try { const v = await invoke("org_publish_rules", { rules: { version: 0, lock_idle_minutes: r.lock_idle_minutes || null, keep_messages_days: r.keep_messages_days || null, backups: r.backups || null, agents: r.agents || null } });
      st.orgAdmin = await invoke("org_admin"); st.orgRulesEdit = null; toast(`Rules version ${v} published. Members' devices take them at their next sync.`); await loadConsole(); }
    catch (e) { toast(String(e)); }
  };
  const fetched = d ? d.devices.filter((x) => x.rules_version >= d.rules_version).length : 0;
  return [h("h1", {}, "Limits"), orgWhere(a), orgErr(o),
    h("section", { class: "card" }, ...secHead("Relay limits", "Set by the relay operator. On a hosted relay they come with the plan."),
      srow("Devices", d ? (d.device_limit ? `${d.device_count} of ${d.device_limit}` : `${d.device_count}, no limit`) : "…"),
      srow("Stored for offline devices", d ? (d.storage_limit ? `${bytesText(d.stored_bytes).join(" ")} of ${bytesText(d.storage_limit).join(" ")}` : `${bytesText(d.stored_bytes).join(" ")}, no limit`) : "…"),
      srow("Who may sign in", d ? (d.enrolled_mode ? "Enrolled devices only. Others may only deliver messages to your members." : "Any device. Enrolment adds it to your organisation.") : "…")),
    h("section", { class: "card" }, ...secHead("Rules for members' devices", "Carried out by the app on each member's device. Signed with your admin key, so the relay cannot change them."),
      srow("Lock when idle", null, sel("Lock when idle", LOCK_RULES, "lock_idle_minutes")),
      srow("Messages kept on each device", "Older messages are deleted in every conversation on the device.", sel("Messages kept", KEEP_RULES, "keep_messages_days")),
      srow("Backups", "Backups are always sealed with a key; the organisation never receives them.", sel("Backups", BACKUP_RULES, "backups")),
      srow("Agents", "Verified only: agents whose safety code the person compared.", sel("Agents", AGENT_RULES, "agents")),
      h("div", { class: "row2" }, h("button", { class: "btn primary", type: "button", onclick: publish }, "Publish rules"),
        d && d.rules_version ? h("span", { class: "note" }, `Version ${d.rules_version} is in force. ${fetched} of ${d.devices.length} device${d.devices.length === 1 ? " has" : "s have"} it.`) : h("span", { class: "note" }, "No rules published yet.")))];
}

function orgUsage(a, o) {
  const d = o.data;
  return [h("h1", {}, "Plan and usage"), orgWhere(a), orgErr(o),
    h("div", { class: "oset" }, h("div", { class: "t" }, h("b", {}, "Plan"), h("span", {}, "Plans and billing are not available yet. The relay operator sets your limits."))),
    d ? h("div", { class: "oset" }, h("div", { class: "t" }, h("b", {}, "This month"), h("span", {}, `${d.device_count} devices enrolled · ${bytesText(d.stored_bytes).join(" ")} stored for offline devices`))) : null,
    h("div", { class: "oset" }, h("div", { class: "t" }, h("b", {}, "Group call minutes"), h("span", {}, "Counted once group calls through the relay exist.")))];
}

function orgKeys(a) {
  const copy = async () => { try { await navigator.clipboard.writeText(a.admin_pub); toast("Public key copied."); } catch (_) { toast("Select the key and copy it."); } };
  return [h("h1", {}, "Admin keys"), h("p", { class: "orgsub" }, "The admin key signs tokens and rules. It never leaves this computer."),
    h("section", { class: "card" },
      srow("Fingerprint", h("span", { class: "note mono" }, a.fingerprint)),
      srow("Public key", h("span", { class: "note mono orgkey" }, a.admin_pub), h("button", { class: "btn", type: "button", onclick: copy }, "Copy")),
      srow("Organisation id", h("span", { class: "note mono" }, a.tenant)),
      srow("Relay", h("span", { class: "note mono" }, a.relay)),
      srow("A lost or new key", "The relay operator registers a new public key (spike-relay tenant rotate-admin). Devices then trust rules from the new key only after they join again.")),
    h("p", { class: "note" }, "Anyone with this computer unlocked can enrol devices into your organisation. They still cannot read anyone's messages.")];
}

/* ---------- Settings: creating the admin key, and membership ---------- */
function orgAdminCard() {
  const a = st.orgAdmin;
  if (a) return h("section", { class: "card", id: "set-orgadmin" }, ...secHead("Organisation console", `This computer holds the admin key for ${a.org_name}.`),
    srow(a.org_name, h("span", { class: "note mono" }, a.relay), h("button", { class: "btn", type: "button", onclick: () => go("org") }, "Open console")),
    srow("Public key", h("span", { class: "note mono orgkey" }, a.admin_pub)));
  const f = st.orgNew || (st.orgNew = { tenant: "", name: "", relay: st.ov.relay });
  const inp = (key, label, mono) => h("div", { class: "field" }, h("label", { class: "lbl" }, label), h("input", { class: mono ? "mono" : null, "aria-label": label, value: f[key], oninput: (e) => { f[key] = e.target.value; } }));
  const create = async () => { try { st.orgAdmin = await invoke("org_admin_create", { tenant: f.tenant, name: f.name, relay: f.relay }); st.orgNew = null; toast("Admin key made. Give the public key to the relay operator."); render(); } catch (e) { toast(String(e)); } };
  return h("section", { class: "card", id: "set-orgadmin" }, ...secHead("Organisation console", "For the person who runs an organisation's devices. Members do not need this."),
    inp("name", "Organisation name"), inp("tenant", "Organisation id (a-z, 0-9 and -, from the relay operator)", true), inp("relay", "Organisation relay", true),
    h("button", { class: "btn primary", type: "button", style: "align-self:flex-start", onclick: create }, "Make admin key"),
    h("p", { class: "note" }, "The key stays on this computer. The relay operator registers only its public half."));
}

function orgMemberRows() {
  const o = st.org;
  if (!o) return [srow("Join an organisation", "Paste the token your organisation sent you. It enrols this computer only.",
    h("button", { class: "btn", type: "button", onclick: () => { st.screen = "orgjoin"; st.orgJoin = null; render(); } }, "Join…"))];
  if (o.removed) return [h("div", { class: "status warn", role: "alert" }, h("b", {}, `${o.org_name} removed this device.`), h("span", {}, "Its rules no longer apply. Choose another relay below, or leave to go back to the one you used before.")),
    srow(`Leave ${o.org_name}'s relay`, null, h("button", { class: "btn danger", type: "button", onclick: () => orgLeave() }, "Leave"))];
  return [srow(`Managed by ${o.org_name}`, "Rules your organisation set for this computer.", h("button", { class: "btn", type: "button", onclick: () => { st.setSec = "set-org"; render(); } }, "See rules"))];
}

async function orgLeave() {
  const o = st.org; if (!o) return;
  try { await invoke("org_leave", { next: null }); toast(`Left ${o.org_name}'s relay. This computer uses ${o.previous_relay || "the default relay"} again.`); await refresh(); syncNow(); }
  catch (e) { toast(String(e)); }
}

function orgRulesCard() {
  const o = st.org; if (!o) return null;
  const r = o.rules || {};
  const row = (label, value) => h("div", { class: "oset" }, h("div", { class: "t" }, h("b", {}, label)), h("span", { class: "v" }, value));
  return h("section", { class: "card", id: "set-org" }, ...secHead(`Managed by ${o.org_name}`, "Rules your organisation set for this computer"),
    o.removed ? h("div", { class: "status warn", role: "alert" }, h("b", {}, `${o.org_name} removed this device.`), h("span", {}, "Its rules no longer apply.")) : null,
    row("Lock when idle", r.lock_idle_minutes ? ruleWord(LOCK_RULES, r.lock_idle_minutes) : "No rule"),
    row("Messages kept on this computer", r.keep_messages_days ? ruleWord(KEEP_RULES, r.keep_messages_days) : "No rule"),
    row("Backups", ruleWord(BACKUP_RULES, r.backups || "")),
    row("Agents", ruleWord(AGENT_RULES, r.agents || "")),
    h("p", { class: "note" }, `These rules are enforced by the app on your device. ${o.org_name} cannot read your messages or see who you talk to.`),
    h("p", { class: "note mono" }, `Signed by ${o.admin_fingerprint}${o.rules ? ` · version ${o.rules.version}` : ""}`),
    h("button", { class: "btn danger", type: "button", style: "align-self:flex-start", onclick: () => orgLeave() }, `Leave ${o.org_name}'s relay`));
}

/* ---------- joining ---------- */
function orgJoinScreen() {
  const j = st.orgJoin || (st.orgJoin = { token: "", info: null, busy: false, err: "" });
  const back = () => { st.screen = null; st.orgJoin = null; render(); };
  if (!j.info) {
    const read = async () => { j.err = ""; try { j.info = await invoke("org_inspect", { token: j.token }); } catch (e) { j.err = String(e); } render(); };
    return [header("Join an organisation", back), h("div", { class: "scroll" }, h("div", { class: "page orgjoin" },
      h("p", { class: "lead" }, "Paste the token your organisation sent you. Nothing is sent until you choose to use the relay."),
      h("textarea", { class: "cardbox mono", "aria-label": "Organisation token", oninput: (e) => { j.token = e.target.value; } }, j.token),
      j.err ? h("div", { class: "err", role: "alert" }, j.err) : null,
      h("div", { class: "row2" }, h("button", { class: "btn primary", type: "button", onclick: read }, "Continue"), h("button", { class: "btn", type: "button", onclick: back }, "Cancel"))))];
  }
  const i = j.info;
  const join = async () => {
    j.busy = true; j.err = ""; render();
    try { st.org = await invoke("org_join", { token: j.token }); toast(`This computer now uses ${i.org_name}'s relay.`); st.screen = null; st.orgJoin = null; st.setSec = "set-org"; await refresh(); syncNow(); }
    catch (e) { j.err = String(e); j.busy = false; render(); }
  };
  let host = i.relay; try { host = new URL(i.relay).host; } catch (_) {}
  const ini = i.org_name.split(/\s+/).map((w) => w[0]).join("").slice(0, 2).toUpperCase();
  return [header("", back), h("div", { class: "scroll" }, h("div", { class: "page orgjoin" },
    h("div", { class: "orgprof" }, h("span", { class: "tile sq c-neutral lg" }, ini), h("b", {}, `Join ${i.org_name}'s relay`), h("span", { class: "note mono" }, host)),
    h("div", { class: "okv" },
      h("div", {}, h("span", {}, "Enrols"), h("span", {}, "This computer only")),
      h("div", {}, h("span", {}, "Token valid until"), h("span", {}, untilText(i.valid_until))),
      h("div", {}, h("span", {}, `${i.org_name} can see`), h("span", {}, "That this device exists")),
      h("div", {}, h("span", {}, `${i.org_name} cannot see`), h("span", {}, "Your name, contacts, messages"))),
    h("p", { class: "note" }, `The name is the one the organisation gave itself. Its key: ${i.admin_fingerprint}.`),
    j.err ? h("div", { class: "err", role: "alert" }, j.err) : null,
    h("button", { class: "btn primary push", type: "button", disabled: j.busy || null, onclick: join }, j.busy ? "Joining…" : "Use this relay"),
    h("button", { class: "btn link", type: "button", onclick: back }, "Not now")))];
}

SECTIONS.org = { list: orgList, main: orgMain };
SCREENS.orgjoin = orgJoinScreen;
