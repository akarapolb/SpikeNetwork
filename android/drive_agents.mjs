// The Agents tab on the real Android app (in the emulator): a real agent (the command-line client) asks the phone to
// pay. The grant approves up to 100.00 alone and refuses above 500.00; 286.00 is in between, so it asks the human.
// Checks the tab (waiting, allowed, decided), the approval screen, approving, the limits screen, and the signed log.
// Needs Node 22+, Windows, the Android SDK with a booted emulator, the debug APK, debug builds of the relay and CLI:
//   bash android/build-native.sh && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay -p spike-cli
//   ANDROID_SERIAL=emulator-5554 node android/drive_agents.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const PORT = 18802;
const RELAY = `ws://127.0.0.1:${PORT}`;

if (!process.env.ANDROID_SERIAL) {
  // This driver wipes the app's data: never let it pick a real phone by accident.
  console.error("Set ANDROID_SERIAL to the emulator to drive (for example emulator-5554).");
  process.exit(2);
}
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tapText, tapField, typeText, shot, hideKeyboard, debug } = createPhone(D);

const agentHome = join(mkdtempSync(join(tmpdir(), "spike-agent-")), "home");
const cli = (...a) =>
  execFileSync(join(D.root, "target", "debug", "spike.exe"), ["--home", agentHome, "--allow-insecure-transport", ...a], {
    encoding: "utf8", env: { ...process.env, SPIKE_PASSPHRASE: "agent test passphrase", SPIKE_RELAY: RELAY },
  });
async function fill(index, text) { await tapField(index); typeText(text); hideKeyboard(); }
async function until(fn, ms = 25000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(500); }
  return null;
}
/** Leave the Agents tab and come back, so it reads the engine again. */
async function reopenAgents() { await tapText("Chats", true); await sleep(400); await tapText("Agents", true); await sleep(900); return screenText(); }

try {
  console.log("-- setting up: relay on this PC, clean install on the emulator");
  D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", `tcp:${PORT}`, `tcp:${PORT}`);
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Your identity lives on this phone", 60000), "the phone shows the create screen");
  await tapText("Create my identity"); await waitText("Passphrase for this phone");
  for (let attempt = 0; attempt < 4; attempt++) {
    await fill(0, PHONE_PASS);
    await fill(1, PHONE_PASS);
    if (nodes().filter((n) => n.cls.endsWith("EditText") && (n.text || "").length > 0).length === 2) break;
    sh(`am start -n ${PKG}/.MainActivity`);
    await sleep(1500);
  }
  await tapText("Create my identity");
  if (await waitText("Turn on fingerprint unlock", 6000)) await tapText("Not now").catch(() => {});
  check(await waitText("Chats"), "the identity is created");
  debug("set_relay", { url: RELAY });
  cli("init");
  cli("add", "Phone", debug("overview").me.card);
  cli("keys");
  const agentCard = cli("card").match(/spike2:[0-9a-f]+/)[0];
  debug("add_contact", { name: "Harbor Travel", card: agentCard });
  debug("sync");

  console.log("-- the tab before anything is allowed");
  let t = await reopenAgents();
  check(t.includes("No agents yet"), "with no grant the tab says so");

  console.log("-- a grant, then a request it must ask about");
  debug("grant", { request: { agent: "Harbor Travel", scopes: ["payments.send"], days: 30, max_cents: 50000, auto_below_cents: 10000, per_hour: 5, daily_cents: null, typed_only: true } });
  cli("agent-request", "Phone", "--scope", "payments.send", "--action", "Hotel Lumen, Lisbon, 2 nights", "--amount-cents", "28600", "--field", "payee=harbor-travel", "--field", "currency=EUR");
  const waiting = await until(async () => { debug("sync"); return (debug("agents").requests || []).find((r) => r.status === "waiting"); });
  check(!!waiting, "the phone receives the agent's request");
  t = await reopenAgents();
  check(/WAITING FOR YOU/.test(t) && t.includes("Pay 286.00 EUR to harbor-travel"), "the tab lists the request under Waiting for you");
  check(/ALLOWED/.test(t) && t.includes("Pay up to 500.00 · 5 an hour"), "the tab lists the grant under Allowed");
  check(t.includes("Activity log"), "the tab offers the activity log");
  shot("agents-1-tab");

  console.log("-- the approval screen");
  await tapText("Pay 286.00 EUR to harbor-travel");
  check(await waitText("Harbor Travel asks to"), "the request opens");
  t = screenText();
  check(t.includes("Pay 286.00 EUR") && t.includes("to harbor-travel"), "it leads with the signed action, and to whom underneath");
  check(t.includes("Asks you above") && t.includes("100.00 EUR"), "it says why it asks: above the approve-alone amount");
  check(t.includes("AGENT SAYS · NOT VERIFIED") && t.includes("Hotel Lumen"), "the agent's own words are set apart as unverified");
  shot("agents-2-approval");
  await tapText("Approve 286.00 EUR", true);
  check(await waitText("Approved by you", 20000), "approving shows the decision on the same screen");
  check((debug("agents").requests || []).some((r) => r.status === "approved"), "the engine has it approved");
  shot("agents-3-approved");

  console.log("-- the tab after, the limits, and the log");
  await tapText("Back", true); await sleep(600);
  t = await reopenAgents();
  check(t.includes("Nothing waiting") && /DECIDED/.test(t), "the request moves from Waiting to Decided");
  await tapText("Pay up to 500.00 · 5 an hour");
  check(await waitText("Allowed until"), "the grant opens its limits screen");
  t = screenText();
  check(t.includes("Each request") && t.includes("500.00") && t.includes("Up to 100.00"), "the limits screen shows the amounts");
  check(t.includes("Stop this agent"), "and offers to stop the agent");
  shot("agents-4-limits");
  await tapText("Back", true); await sleep(600);
  await tapText("Activity log", true);
  check(await waitText("Hash-chained, no message content"), "the activity log opens");
  t = screenText();
  check(t.includes("Harbor Travel was allowed") && t.includes("Harbor Travel asked") && t.includes("Harbor Travel was given a grant"), "it lists the grant, the request and the approval");
  const a = debug("agents");
  check(a.log_ok === true && a.log.length >= 3, "the signed log checks out end to end");
  shot("agents-5-log");
  await tapText("Denied", true); await sleep(500);
  check(!screenText().includes("was allowed"), "the Denied filter hides the approval");
  await tapText("Back", true); await sleep(600);

  console.log("-- stopping the agent");
  await tapText("Pay up to 500.00 · 5 an hour");
  await waitText("Stop this agent");
  await tapText("Stop this agent");
  check(await waitText("Stop Harbor Travel?"), "stopping asks first");
  await tapText("Stop", true);
  check(await until(async () => (debug("agents").grants || []).length === 0, 10000), "the grant is gone");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
