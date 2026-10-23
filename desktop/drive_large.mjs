// Drives three real desktop app windows (alice, bob, carol) through a large group, over the Chrome
// DevTools protocol. Needs Node 22+, Windows (WebView2), and the debug builds of the app and relay:
//   cargo build -p spike-relay            (repository root)
//   cargo build                           (desktop/src-tauri)
//   node desktop/drive_large.mjs [screenshot-dir]
import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = process.cwd();
const shots = process.argv[2] || join(tmpdir(), "spike-large-shots");
mkdirSync(shots, { recursive: true });
const tmp = mkdtempSync(join(tmpdir(), "spike-drive-"));
const RELAY = "ws://127.0.0.1:18790";
const PASS = "drive test passphrase";
const procs = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;
const check = (ok, what) => { console.log(`${ok ? "ok  " : "FAIL"} ${what}`); if (!ok) failures++; };

function start(cmd, args, env) {
  const p = spawn(cmd, args, { env: { ...process.env, ...env }, stdio: "ignore" });
  procs.push(p);
  return p;
}

class Page {
  constructor(name, port) { this.name = name; this.port = port; this.id = 0; this.pending = new Map(); }
  async connect() {
    for (let i = 0; i < 80; i++) {
      try {
        const list = await (await fetch(`http://127.0.0.1:${this.port}/json`)).json();
        const t = list.find((x) => x.type === "page");
        if (t) { this.ws = new WebSocket(t.webSocketDebuggerUrl); break; }
      } catch (_) {}
      await sleep(250);
    }
    if (!this.ws) throw new Error(`${this.name}: no page`);
    await new Promise((res, rej) => { this.ws.onopen = res; this.ws.onerror = rej; });
    this.ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && this.pending.has(d.id)) { this.pending.get(d.id)(d); this.pending.delete(d.id); } };
  }
  send(method, params = {}) { return new Promise((res) => { const id = ++this.id; this.pending.set(id, res); this.ws.send(JSON.stringify({ id, method, params })); }); }
  async eval(expr) {
    const r = await this.send("Runtime.evaluate", { expression: `(async()=>{${expr}})()`, awaitPromise: true, returnByValue: true });
    if (r.result.exceptionDetails) throw new Error(`${this.name}: ${JSON.stringify(r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text)}`);
    return r.result.result.value;
  }
  invoke(cmd, args = {}) { return this.eval(`return await window.__TAURI__.core.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)});`); }
  // Click the first button/element whose text includes `text`, within an optional css scope.
  async click(text, scope = "body") {
    const ok = await this.eval(`const els=[...document.querySelectorAll(${JSON.stringify(scope)} + ' button, ' + ${JSON.stringify(scope)} + ' [role=radio]')].filter(e=>!e.hidden&&e.offsetParent!==null); const e=els.find(x=>(x.innerText||x.getAttribute('aria-label')||'').includes(${JSON.stringify(text)})); if(!e) return false; e.click(); return true;`);
    if (!ok) throw new Error(`${this.name}: no button "${text}"`);
    await sleep(250);
  }
  async text() { return this.eval("return document.body.innerText"); }
  async shot(label) {
    const r = await this.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(shots, `${this.name}-${label}.png`), Buffer.from(r.result.data, "base64"));
  }
  async reload() { await this.send("Page.reload"); await sleep(1500); }
  // Redraw from the backend without leaving the screen (a reload would show the unlock screen).
  async refresh() { await this.eval("await refresh(); render();"); await sleep(300); }
}

async function launch(name, port) {
  const exe = join(root, "desktop", "src-tauri", "target", "debug", "spike-desktop.exe");
  const home = join(tmp, name);
  start(exe, [], {
    SPIKE_DESKTOP_HOME: home,
    WEBVIEW2_USER_DATA_FOLDER: join(tmp, `${name}-wv`),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  });
  const p = new Page(name, port);
  await p.connect();
  await sleep(1500);
  await p.invoke("create_identity", { passphrase: PASS });
  await p.invoke("set_relay", { url: RELAY });
  await p.reload();
  // The window always starts at the unlock screen: sign in through it, as a person would.
  await p.eval(`const i=document.querySelector('input'); i.value=${JSON.stringify(PASS)}; i.dispatchEvent(new Event('input',{bubbles:true}));`);
  await p.click("Unlock");
  await sleep(2500);
  return p;
}

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18790"], {});
  await sleep(1000);
  const alice = await launch("alice", 9331);
  const bob = await launch("bob", 9332);
  const carol = await launch("carol", 9333);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob, carol]) await p.invoke("sync"); // the first sync publishes key packages
  for (const [n, p] of [["Bob", bob], ["Carol", carol]]) {
    await alice.invoke("add_contact", { name: n, card: await card(p) });
    await p.invoke("add_contact", { name: "Alice", card: await card(alice) });
  }
  await alice.refresh();

  if (process.env.DEBUG_DRIVE) { console.log(await alice.text()); await alice.shot("0-debug"); }
  // Alice makes a large group from the screen.
  await alice.click("New group");
  await alice.shot("1-new-group");
  await alice.click("Large");
  await alice.eval(`const i=document.querySelector('input[aria-label="Group name"]'); i.value="Org"; i.dispatchEvent(new Event('input',{bubbles:true}));`);
  const boxes = await alice.eval(`return [...document.querySelectorAll('label.pick input')].length`);
  check(boxes === 2, "alice sees her two contacts to pick");
  await alice.eval(`document.querySelectorAll('label.pick input').forEach(i=>{i.checked=true;i.dispatchEvent(new Event('change',{bubbles:true}));})`);
  await alice.shot("2-new-large-group");
  await alice.click("Create group");
  await sleep(2500);
  let t = await alice.text();
  check(t.includes("Org") && t.includes("Large"), "alice's list shows Org tagged Large");
  await alice.shot("3-created");

  // Bob and Carol see an invitation; nothing is joined until they accept.
  for (const p of [bob, carol]) {
    await p.invoke("sync");
    await p.refresh();
    t = await p.text();
    check(t.includes("Invite") && t.includes("Org"), `${p.name} sees an invitation`);
    await p.click("Org");
    await sleep(500);
    t = await p.text();
    check(t.includes("Nothing from the group is shown until you accept") && t.includes("large group"), `${p.name} gets the consent screen`);
    check(!t.includes("Block"), `${p.name}: no Block button for a large group`);
    if (p === bob) await p.shot("4-invite");
    await p.click("Join the group");
    await sleep(2000);
  }
  for (const p of [alice, bob, carol]) await p.invoke("sync");

  // A message from each way round.
  const say = async (p, text) => {
    await p.eval(`const ta=document.querySelector('textarea'); ta.value=${JSON.stringify(text)}; ta.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true,cancelable:true}));`);
    await sleep(1500);
  };
  await alice.refresh(); await alice.click("Org"); await sleep(500);
  await say(alice, "welcome to the org");
  await bob.refresh(); await carol.refresh();
  for (const p of [bob, carol]) { await p.invoke("sync"); await p.refresh(); }
  t = await bob.text();
  check(t.includes("Alice: welcome to the org") || t.includes("welcome to the org"), "bob received alice's message");
  await bob.click("Org"); await sleep(800);
  check((await bob.text()).includes("welcome to the org"), "bob reads it in the thread");
  await bob.shot("5-thread");
  await say(bob, "thanks alice");
  await carol.invoke("sync"); await carol.refresh(); await carol.click("Org"); await sleep(800);
  t = await carol.text();
  check(t.includes("thanks alice") && /~[0-9a-f]/.test(t), "carol reads bob's message under a name she can recognise, not as a contact");

  // Members screen: alice is admin and can remove; make-admin / rename / leave are not offered.
  await alice.invoke("sync"); await alice.refresh(); await alice.click("Org"); await sleep(500);
  await alice.click("Members"); await sleep(700);
  t = await alice.text();
  check(t.toLowerCase().includes("members (3)") && t.includes("up to 500"), "alice's members screen counts 3 of up to 500");
  check(!t.includes("Make admin") && !t.includes("Leave group") && !t.includes("Group settings"), "unsupported controls are not offered");
  await alice.shot("6-members");

  // Alice removes Carol. (confirm() is auto-accepted for the run.)
  await alice.eval("window.confirm = () => true;");
  await alice.eval(`const rows=[...document.querySelectorAll('.member')]; const r=rows.find(x=>x.innerText.includes('Carol')); [...r.querySelectorAll('button')].find(b=>b.innerText==='Remove').click();`);
  await sleep(3000);
  t = await alice.text();
  check(t.toLowerCase().includes("members (2)"), "alice's member list shrinks to 2");
  await bob.invoke("sync");
  const carolSync = await carol.invoke("sync");
  check(carolSync.notes.some((n) => n.includes("removed")), "carol is told she was removed");
  await carol.refresh();
  t = await carol.text();
  check(t.includes("left"), "carol's list marks the group as left");
  await carol.click("Org"); await sleep(600);
  t = await carol.text();
  check(t.includes("You are not in this group"), "carol cannot write any more");
  await carol.shot("7-removed");
  // Bob and Alice carry on in the new mailbox.
  await bob.refresh(); await bob.click("Org"); await sleep(500);
  await say(bob, "just us now");
  await alice.invoke("sync");
  const aliceMsgs = await alice.invoke("group_conversation", { id: (await alice.invoke("overview")).groups[0].id });
  check(aliceMsgs.some((m) => m.body === "just us now"), "alice reads bob's message in the new mailbox");
} catch (e) {
  console.log("ERROR", e.message);
  failures++;
} finally {
  for (const p of procs) { try { p.kill(); } catch (_) {} }
  await sleep(500);
  try { rmSync(tmp, { recursive: true, force: true }); } catch (_) {}
  console.log(failures ? `${failures} FAILED` : "all passed", `· screenshots in ${shots}`);
  process.exit(failures ? 1 : 0);
}
