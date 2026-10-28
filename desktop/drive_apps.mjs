// App channels (ADR 0030): two real desktop windows over a real relay, and a "program" (this script)
// that asks Alice's window for access through the local API, is allowed in the window, and then sends
// and receives its own data as Alice. Bob's side is a second program on Bob's window.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_apps.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const RELAY = "ws://127.0.0.1:18799";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;
const APP = "work.center";
const ALICE_API = 18897, BOB_API = 18898;

async function until(fn, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(300); }
  return null;
}
/** A program talking to a window's local API (Node's own fetch, as a real program would). */
async function api(port, method, path, { token, body } = {}) {
  const headers = { "Content-Type": "application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;
  const r = await fetch(`http://127.0.0.1:${port}${path}`, { method, headers, body: body ? JSON.stringify(body) : undefined });
  return { status: r.status, json: await r.json() };
}
const b64 = (s) => Buffer.from(s).toString("base64");
const unb64 = (s) => Buffer.from(s, "base64").toString();

/** Turn the local API on in Settings, ask from a program, allow it in the window; returns the token. */
async function connectProgram(p, port, who) {
  await p.click("Settings");
  await until(() => p.eval(`return !!document.getElementById('apps-card')`));
  check((await p.eval(`return document.getElementById('apps-none')?.innerText || ''`)).includes("No apps"), `${who}: Settings says no apps are set up yet`);
  await p.eval(`document.getElementById('apps-api-toggle').click()`);
  const status = await until(async () => { const t = await p.eval(`return document.getElementById('apps-api-status')?.innerText || ''`); return t.includes("Listening") ? t : null; });
  check((status || "").includes(`127.0.0.1:${port}`), `${who}: turning it on shows where it listens (${(status || "").replace(/\n/g, " ")})`);
  const ask = await api(port, "POST", "/v1/connect", { body: { app: APP, name: "Center Work" } });
  check(ask.status === 200 && ask.json.id, `${who}: a program asks to connect`);
  const banner = await until(() => p.eval(`return document.querySelector('.ask-banner')?.innerText || ''`));
  check((banner || "").includes("Center Work asks to use Spike as you") && banner.includes("cannot read your chats"), `${who}: the window asks the person, saying what the program can and cannot do`);
  check((await api(port, "GET", `/v1/connect/${ask.json.id}`)).json.status === "waiting", `${who}: the program waits for the answer`);
  if (who === "alice") await p.shot("app-ask");
  await p.click("Allow", ".ask-banner");
  const got = await until(async () => { const r = await api(port, "GET", `/v1/connect/${ask.json.id}`); return r.json.status === "allowed" ? r.json : null; });
  check(!!got?.token, `${who}: after Allow, the program gets its token`);
  check(await until(() => p.eval(`return !document.querySelector('.ask-banner')`)), `${who}: the prompt goes away`);
  const row = await until(() => p.eval(`return document.querySelector('.app-row')?.innerText || ''`));
  check((row || "").includes("Center Work") && row.includes("connected"), `${who}: Settings lists Center Work as connected`);
  return got?.token;
}

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18799"], {});
  await sleep(1000);
  process.env.SPIKE_LOCAL_API_PORT = String(ALICE_API);
  const alice = await launch("alice", 9411);
  process.env.SPIKE_LOCAL_API_PORT = String(BOB_API);
  const bob = await launch("bob", 9412);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  for (const p of [alice, bob]) await p.eval(`window.confirm = () => true;`);

  console.log("-- off until turned on");
  check((await api(ALICE_API, "POST", "/v1/connect", { body: { app: APP, name: "Center Work" } }).catch(() => ({ status: 0 }))).status !== 200, "nothing answers before the person turns it on");

  console.log("-- connecting programs");
  const aliceToken = await connectProgram(alice, ALICE_API, "alice");
  const bobToken = await connectProgram(bob, BOB_API, "bob");

  console.log("-- a page in a browser is kept out");
  const page = await fetch(`http://127.0.0.1:${ALICE_API}/v1/targets`, { headers: { Authorization: `Bearer ${aliceToken}`, Origin: "https://evil.example" } });
  check(page.status === 403, "a request with a web page's Origin is refused");

  console.log("-- data between the two programs");
  const targets = await api(ALICE_API, "GET", "/v1/targets", { token: aliceToken });
  check(targets.json.contacts?.some((c) => c.name === "Bob"), "Alice's program can see who it may send to");
  const sent = await api(ALICE_API, "POST", "/v1/send", { token: aliceToken, body: { to: { contact: "Bob" }, channel: "sheet-1", mode: "stored", data: b64("B12=42") } });
  check(sent.status === 200 && sent.json.id, `Alice's program sends stored data (${sent.status})`);
  const bobGot = await until(async () => { const r = await api(BOB_API, "GET", "/v1/events?after=0&wait=2", { token: bobToken }); return r.json.events?.length ? r.json.events : null; }, 20000);
  check(bobGot && bobGot[0].from === "Alice" && unb64(bobGot[0].data) === "B12=42", "Bob's program receives it, from Alice, through his window's instant delivery");
  check(!(await bob.text()).includes("B12=42"), "it never shows as chat in Bob's window");
  const seq = bobGot ? bobGot[0].seq : 0;
  check((await api(BOB_API, "GET", `/v1/events?after=${seq}`, { token: bobToken })).json.events.length === 0, "taking events after it acknowledges it");

  const wait = api(ALICE_API, "GET", "/v1/events?after=0&wait=20", { token: aliceToken });
  await sleep(300);
  const t0 = Date.now();
  await api(BOB_API, "POST", "/v1/send", { token: bobToken, body: { to: { contact: "Alice" }, channel: "sheet-1", mode: "live", data: b64("cursor at C3") } });
  const live = await wait;
  check(live.json.events?.[0]?.mode === "live" && unb64(live.json.events[0].data) === "cursor at C3", `a live update wakes Alice's waiting program (${Date.now() - t0} ms)`);

  console.log("-- removing the app");
  await alice.refresh();
  await alice.eval(`document.getElementById('apps-card').scrollIntoView({ block: "center" })`);
  const counts = await alice.eval(`return document.querySelector('.app-row')?.innerText || ''`);
  check(counts.includes("received 1") && counts.includes("sent 1"), `Settings counts what the app sent and received (${counts.replace(/\n/g, " ")})`);
  await sleep(300);
  await alice.shot("apps-settings");
  await alice.eval(`document.querySelector('[aria-label="Remove Center Work"]').click()`);
  check(await until(() => alice.eval(`return !!document.getElementById('apps-none')`)), "Remove takes Center Work off the list");
  check((await api(ALICE_API, "GET", "/v1/targets", { token: aliceToken })).status === 401, "the program's token stops working");
  await api(BOB_API, "POST", "/v1/send", { token: bobToken, body: { to: { contact: "Alice" }, channel: "sheet-1", mode: "stored", data: b64("A1=1") } });
  const dropped = await until(async () => { await alice.invoke("sync"); await alice.eval("await refreshApps();"); return alice.eval(`return document.getElementById('apps-dropped')?.innerText || ''`); }, 20000);
  check((dropped || "").includes(APP), `data for it that still arrives is dropped and Settings says so (${(dropped || "").replace(/\n/g, " ")})`);

  console.log("-- turning the local API off");
  await alice.eval(`document.getElementById('apps-api-toggle').click()`);
  await sleep(800);
  check((await api(ALICE_API, "GET", "/v1/me").catch(() => ({ status: 0 }))).status === 0, "turning it off closes the port");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
