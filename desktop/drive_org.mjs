// Organisations (ADR 0035): three real desktop windows. Ann holds Northwind's admin key and uses the console;
// Alice joins Northwind's relay (enrolled mode) with a token from it; Bob stays on the ordinary relay and still
// reaches her. Then rules are published and reach Alice's computer, the console shows her device, and the admin
// revokes it.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_org.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const RELAY = "ws://127.0.0.1:18811";
const ORG_RELAY = "ws://127.0.0.1:18812";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, tmp, start, launch, check } = D;
const relayExe = join(root, "target", "debug", "spike-relay.exe");

async function until(fn, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(300); }
  return null;
}
const typeInto = (p, sel, v) => p.eval(`const i=document.querySelector(${JSON.stringify(sel)}); i.value=${JSON.stringify(v)}; i.dispatchEvent(new Event('input',{bubbles:true}));`);
const pickIn = (p, label, v) => p.eval(`const s=document.querySelector('select[aria-label=${JSON.stringify(label)}]'); s.value=${JSON.stringify(String(v))}; s.dispatchEvent(new Event('change',{bubbles:true}));`);
const settings = async (p, sec) => { await p.eval(`st.section='settings'; st.screen=null; st.setSec=${JSON.stringify(sec)}; render();`); await sleep(300); };
// The error text of a refused command ("" if it worked).
const refusal = (p, cmd, args) => p.eval(`try { await window.__TAURI__.core.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)}); return ""; } catch (e) { return String(e); }`);
const noNull = async (p, where) => check(!/(^|\n)\s*null\s*(\n|$)/.test(await p.text()), `no stray "null" on the screen (${where})`);

try {
  start(relayExe, ["--listen", "127.0.0.1:18811"], {});
  await sleep(1000);
  const ann = await launch("ann", 9431);
  const alice = await launch("alice", 9432);
  const bob = await launch("bob", 9433);

  console.log("-- the admin key, made in Settings");
  await settings(ann, "set-orgadmin");
  check((await ann.text()).includes("Members do not need this"), "Settings offers to make an organisation admin key");
  await typeInto(ann, 'input[aria-label="Organisation name"]', "Northwind");
  await typeInto(ann, 'input[aria-label^="Organisation id"]', "northwind");
  await typeInto(ann, 'input[aria-label="Organisation relay"]', ORG_RELAY);
  await ann.click("Make admin key");
  const admin = await ann.invoke("org_admin");
  check(admin && admin.admin_pub.length === 64, "the console holds a key and shows its public half");
  await ann.refresh();
  check(await ann.eval(`return [...document.querySelectorAll('#rail button')].some(b=>b.getAttribute('aria-label')==='Organisation')`), "the rail shows the organisation console");

  // The relay operator registers the public key and starts the organisation's relay in enrolled mode.
  const db = join(tmp, "org-relay.db");
  execFileSync(relayExe, ["--db", db, "tenant", "create", "northwind", "--name", "Northwind", "--admin-pub", admin.admin_pub, "--max-devices", "50"]);
  start(relayExe, ["--listen", "127.0.0.1:18812", "--db", db, "--enrolled"], {});
  await sleep(1000);

  console.log("-- the console");
  await ann.eval(`st.oc=null; go('org');`);
  const over = await until(async () => { const t = await ann.text(); return t.includes("Healthy") ? t : null; });
  check(!!over, "Overview: the relay is healthy");
  check((over || "").includes("Devices enrolled") && (over || "").includes("of 50"), "Overview: devices enrolled against the limit");
  check((over || "").includes("There are no names, contacts or message content to read"), "Overview says what the console can see");
  check((over || "").includes("not measured yet"), "Overview does not invent a delivery figure");
  await noNull(ann, "console overview");
  await ann.shot("console-overview");

  await ann.eval(`st.orgSec='devices'; render();`);
  await sleep(300);
  check((await ann.text()).includes("Enrolment is about devices, never people."), "Devices: the line from the design");
  await typeInto(ann, ".orglabel", "Alice's laptop");
  await ann.click("Make token");
  const token = await until(() => ann.eval(`return st.orgTok && st.orgTok.made && st.orgTok.made.token`));
  check(token && token.startsWith("spikeorg1:"), "a token is made");
  check(await until(() => ann.eval(`return !!document.querySelector('.orgwiz img.qrimg[src^="data:"]')`)), "the token is shown as a QR code");
  await ann.shot("console-token");

  console.log("-- Alice joins");
  await settings(alice, "set-relay");
  await alice.click("Join…");
  await typeInto(alice, ".orgjoin textarea", token);
  await alice.click("Continue");
  const sheet = await alice.text();
  check(sheet.includes("Join Northwind's relay"), "the join sheet names the organisation");
  check(sheet.includes("This computer only") && sheet.includes("That this device exists") && sheet.includes("Your name, contacts, messages"), "the sheet says what Northwind can and cannot see");
  await alice.shot("join-sheet");
  await alice.click("Use this relay");
  const joined = await until(async () => (await alice.invoke("overview")).relay === ORG_RELAY);
  check(joined, "Alice's home relay is now Northwind's");
  const ms = await alice.invoke("org_status");
  check(ms && ms.tenant === "northwind" && !ms.removed, "Alice's computer records the membership");

  // A second computer cannot use the same token.
  const reused = await refusal(bob, "org_join", { token });
  check(reused.includes("already used"), `the token works once ("${reused.slice(0, 80)}")`);

  console.log("-- Bob, on the other relay, still reaches Alice");
  const card = async (p) => (await p.invoke("overview")).me.card;
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  await bob.invoke("send", { name: "Alice", text: "hello at Northwind" });
  const got = await until(async () => { await alice.invoke("sync"); return (await alice.invoke("conversation", { name: "Bob" })).some((m) => m.body === "hello at Northwind"); });
  check(got, "a message from a contact on another relay arrives on the organisation's relay");

  console.log("-- rules");
  await ann.eval(`st.orgSec='limits'; render();`);
  await sleep(300);
  await pickIn(ann, "Lock when idle", 5);
  await pickIn(ann, "Messages kept", 90);
  await pickIn(ann, "Backups", "key_required");
  await pickIn(ann, "Agents", "verified_only");
  await ann.click("Publish rules");
  check(await until(async () => (await ann.text()).includes("Version 1 is in force")), "the console publishes version 1");
  await ann.shot("console-limits");
  await alice.invoke("sync");
  await alice.refresh();
  await settings(alice, "set-org");
  const rules = await alice.text();
  check(rules.includes("Managed by Northwind"), "Alice's Settings shows who manages the computer");
  check(rules.includes("5 minutes") && rules.includes("90 days") && rules.includes("Allowed, key required") && rules.includes("Verified only"), "the four rules reach Alice's computer");
  check(rules.includes("These rules are enforced by the app on your device. Northwind cannot read your messages or see who you talk to."), "the rules screen says how they are enforced");
  check((await alice.eval("return autoLockMinutes()")) === 5, "the lock rule caps auto-lock at 5 minutes");
  await noNull(alice, "organisation rules");
  await alice.shot("rules");
  const grant = await refusal(alice, "grant", { request: { agent: "Bob", scopes: ["pay"], days: 30, max_cents: null, auto_below_cents: null, per_hour: 10, daily_cents: null, typed_only: false } });
  check(grant.includes("only verified agents"), `"verified only" refuses a grant to an unverified contact ("${grant.slice(0, 70)}")`);

  console.log("-- the console sees one device, and revokes it");
  await ann.eval(`st.orgSec='devices'; st.oc=null; render();`);
  const devText = await until(async () => { const t = await ann.text(); return t.includes("active this week") ? t : null; });
  check(!!devText, "Devices lists Alice's computer as active this week");
  check(!(devText || "").includes("Alice"), "the console shows no name for the device");
  await ann.shot("console-devices");
  await ann.click("Revoke");
  await ann.click("Confirm");
  check(await until(async () => (await ann.text()).includes("No devices yet")), "the device is gone from the console");
  const removed = await until(async () => { await refusal(alice, "sync", {}); const o = await alice.invoke("org_status"); return o && o.removed; });
  check(removed, "Alice's computer learns it was removed");
  await alice.refresh();
  await settings(alice, "set-relay");
  check((await alice.text()).includes("Northwind removed this device."), "Settings says so in words");
  await alice.shot("removed");
  await alice.click("Leave");
  check(await until(async () => (await alice.invoke("overview")).relay === RELAY && !(await alice.invoke("org_status"))), "leaving goes back to the relay used before");
} catch (e) {
  console.log("FAIL", e.message || e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
