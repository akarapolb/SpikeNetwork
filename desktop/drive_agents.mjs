// The Agents page: a real agent (the command-line app) asks the real desktop app to pay more than its limit.
// The grant approves up to 100.00 alone and refuses above 500.00; 286.00 is in between, so it asks the human.
// Checks the list (waiting, allowed), the detail pane, approving, and the signed activity log.
//   cargo build -p spike-relay -p spike-cli && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_agents.mjs [screenshot-dir]
import { join } from "node:path";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { createDrive, sleep } from "./drive_lib.mjs";

const PORT = 18799;
const RELAY = `ws://127.0.0.1:${PORT}`;
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

async function until(fn, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(400); }
  return null;
}
// The agent: the command-line app with its own state directory.
const agentHome = mkdtempSync(join(tmpdir(), "spike-agent-"));
const spike = (...args) => execFileSync(join(root, "target", "debug", "spike.exe"), args, {
  env: { ...process.env, SPIKE_HOME: agentHome, SPIKE_PASSPHRASE: "agent test passphrase", SPIKE_RELAY: RELAY }, encoding: "utf8",
});

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`], {});
  await sleep(1000);
  const alice = await launch("alice", 9411);
  await alice.invoke("sync");
  spike("init");
  const agentCard = spike("card").split(/\s+/).find((w) => w.startsWith("spike"));
  check(!!agentCard, "the agent has an identity card");
  await alice.invoke("add_contact", { name: "Harbor Travel", card: agentCard });
  spike("add", "Alice", (await alice.invoke("overview")).me.card);
  spike("sync");

  console.log("-- the agents page before anything is allowed");
  await alice.eval(`go('agents')`); await sleep(500);
  check(/No agents yet/.test(await alice.text()), "with no grant the page says so");

  console.log("-- a grant, then a request it must ask about");
  await alice.invoke("grant", { request: { agent: "Harbor Travel", scopes: ["payments.send"], days: 30, max_cents: 50000, auto_below_cents: 10000, per_hour: 5, daily_cents: null, typed_only: true } });
  spike("agent-request", "Alice", "--scope", "payments.send", "--action", "Hotel Lumen, Lisbon, 2 nights", "--amount-cents", "28600", "--field", "payee=harbor-travel", "--field", "currency=EUR");
  const waiting = await until(async () => { await alice.invoke("sync"); const a = await alice.invoke("agents"); return a.requests.find((r) => r.status === "waiting"); });
  check(!!waiting, "the desktop receives the agent's request");
  await alice.eval(`go('agents')`); await alice.refresh(); await sleep(800);
  const list = await alice.eval(`return document.querySelector('#list').innerText`);
  check(/Waiting for you/i.test(list) && list.includes("Harbor Travel"), "the list shows the request under Waiting for you");
  check(/Allowed/i.test(list) && /Pay up to 500\.00/.test(list), "the list shows the grant under Allowed");
  const pane = await alice.eval(`return document.querySelector('#main').innerText`);
  check(/Pay 286\.00 EUR\s+to harbor-travel/.test(pane), `the detail pane leads with the signed action (${(pane.match(/Pay[^\n]*/) || [""])[0]})`);
  check(/Asks you above\s+100\.00 EUR/.test(pane), "the detail pane says why it asks: above the approve-alone amount");
  check(/Hotel Lumen/.test(pane), "the agent's own words are shown, marked as not verified");
  check(/Activity/i.test(pane) && /asked/.test(pane), "the activity log lists the request");
  await alice.shot("agents-waiting");

  console.log("-- approve, then the log and the list follow");
  await alice.eval(`[...document.querySelectorAll('#main button')].find(b=>b.innerText.startsWith('Approve')).click()`);
  const decided = await until(async () => { const a = await alice.invoke("agents"); return a.requests.find((r) => r.status === "approved"); });
  check(!!decided, "approving records the decision");
  await alice.refresh(); await sleep(800);
  const list2 = await alice.eval(`return document.querySelector('#list').innerText`);
  check(/Nothing waiting/.test(list2) && /Decided/i.test(list2), "the request moves from Waiting to Decided");
  const a2 = await alice.invoke("agents");
  check(a2.log_ok === true && a2.log.some((e) => e.event === "approved"), "the signed log checks out and has the approval");
  const pane2 = await alice.eval(`return document.querySelector('#main').innerText`);
  check(/Approved by you/.test(pane2), "the pane stays on the request and shows the decision");
  await alice.shot("agents-decided");

  console.log("-- the grant on its own");
  await alice.eval(`[...document.querySelectorAll('#list .row')].find(b=>b.innerText.includes('Pay up to')).click()`); await sleep(500);
  const pane3 = await alice.eval(`return document.querySelector('#main').innerText`);
  check(/Harbor Travel may/.test(pane3) && /Revoke/.test(pane3), "choosing the grant shows its limits with Change and Revoke");
  check(!/(^|\n)\s*null\s*(\n|$)/.test(await alice.text()), `no stray "null" on the screen`);
  await alice.shot("agents-grant");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(`${D.failures() ? `${D.failures()} FAILED` : "all passed"} · screenshots in ${D.shots}`);
  process.exit(D.failures() ? 1 : 0);
}
