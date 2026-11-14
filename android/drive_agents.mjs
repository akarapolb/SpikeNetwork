// The Agents tab on the real Android app (in the emulator): a real agent (the command-line client) asks the phone to
// pay. The grant approves up to 100.00 alone and refuses above 500.00; 286.00 is in between, so it asks the human.
// Checks the tab (waiting, allowed, decided), the approval screen, approving, the limits screen, and the signed log.
// The grant is given and changed on the phone's limits screen; then the agent directory (ADR 0036): listings that
// nobody you trust signed are "not verified", trusting a lister makes its listings verified, and "Set limits" on one
// adds the agent and opens its empty form.
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
/** Replace what a field holds: put the cursor at its end and delete what is there first. */
async function setField(index, text) {
  await tapField(index);
  sh("input keyevent KEYCODE_MOVE_END");
  sh(`input keyevent ${Array(14).fill("KEYCODE_DEL").join(" ")}`);
  if (text) typeText(text);
  hideKeyboard();
  await sleep(300);
}
const LIMIT_FIELDS = { each: 0, auto: 1, daily: 2, perHour: 3, days: 4 };
/** Another command-line identity (a lister, or another agent), with its own home. */
function cliAt(name) {
  const home = join(mkdtempSync(join(tmpdir(), `spike-${name}-`)), "home");
  return (...a) => execFileSync(join(D.root, "target", "debug", "spike.exe"), ["--home", home, "--allow-insecure-transport", ...a], {
    encoding: "utf8", env: { ...process.env, SPIKE_PASSPHRASE: `${name} test passphrase`, SPIKE_RELAY: RELAY },
  });
}
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

  console.log("-- giving the agent a grant on the phone");
  await tapText("Give an agent a grant", true);
  check(await waitText("Then set what it may do"), "the tab offers to give an agent a grant, and asks which contact");
  await tapText("Harbor Travel", true);
  check(await waitText("Not allowed yet"), "choosing the contact opens an empty limits form");
  t = screenText();
  check(["Each request", "Without asking", "In any 24 hours", "Requests an hour", "Typed actions only", "How long"].every((x) => t.includes(x)),
    "the form has every limit of a grant");
  check(t.includes("Allow Harbor Travel"), "and ends in Allow");
  await tapText("Allow Harbor Travel", true);
  check(await waitText("Choose at least one thing it may do"), "allowing with nothing chosen is refused in words");
  await tapText("Pay", true);
  await setField(LIMIT_FIELDS.each, "500");
  await setField(LIMIT_FIELDS.auto, "600");
  await setField(LIMIT_FIELDS.perHour, "5");
  await tapText("Allow Harbor Travel", true);
  check(await waitText("Without asking cannot be more than each request"), "an approve-alone amount above the cap is refused in words");
  check((debug("agents").grants || []).length === 0, "and nothing was signed");
  await setField(LIMIT_FIELDS.auto, "100");
  shot("agents-0-new-grant");
  await tapText("Allow Harbor Travel", true);
  check(await waitText("Allowed until", 15000), "allowing signs the grant and the screen shows how long");
  let gr = (debug("agents").grants || [])[0];
  check(gr && gr.max_cents === 50000 && gr.auto_below_cents === 10000 && gr.per_hour === 5 && gr.daily_cents == null && gr.typed_only === true
    && gr.scopes.join() === "payments.send", "the engine holds exactly the limits typed on the phone");
  check(gr && Math.abs(gr.expires - Date.now() - 30 * 86400000) < 3600000, "for 30 days");
  check(!screenText().includes("Save limits"), "an unchanged grant offers nothing to save");
  await tapText("Back", true); await sleep(600);

  console.log("-- a request it must ask about");
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

  console.log("-- changing the limits on the phone");
  await setField(LIMIT_FIELDS.each, "250.50");
  await setField(LIMIT_FIELDS.daily, "800");
  check(await waitText("Save limits"), "changing a limit offers to save");
  await tapText("Save limits", true);
  check(await until(async () => (debug("agents").grants || [])[0]?.max_cents === 25050, 15000), "saving signs a new grant with the new cap");
  gr = (debug("agents").grants || [])[0];
  check(gr.auto_below_cents === 10000 && gr.daily_cents === 80000 && gr.per_hour === 5 && gr.typed_only === true, "the other limits are kept and the 24-hour cap is added");
  check(await until(async () => !screenText().includes("Save limits"), 8000), "after saving there is nothing left to save");
  t = screenText();
  check(t.includes("250.50") && t.includes("800.00"), "the screen shows the saved amounts");
  await setField(LIMIT_FIELDS.daily, "100");
  await tapText("Save limits", true);
  check(await waitText("In any 24 hours cannot be less than each request"), "a 24-hour cap below the cap is refused in words");
  check((debug("agents").grants || [])[0].daily_cents === 80000, "and the signed grant is unchanged");
  shot("agents-4b-limits-changed");
  await tapText("Back", true); await sleep(600);
  t = await reopenAgents();
  check(t.includes("Pay up to 250.50 · 5 an hour"), "the tab shows the changed grant");
  await tapText("Back", true); await sleep(600);
  await tapText("Activity log", true);
  check(await waitText("Hash-chained, no message content"), "the activity log opens");
  t = screenText();
  check(t.includes("Harbor Travel was allowed") && t.includes("Harbor Travel asked") && t.includes("Harbor Travel was given a grant"), "it lists the grant, the request and the approval");
  const a = debug("agents");
  check(a.log_ok === true && a.log.filter((e) => e.event === "grant").length === 2, "the signed log checks out end to end, with both grants");
  shot("agents-5-log");
  await tapText("Denied", true); await sleep(500);
  check(!screenText().includes("was allowed"), "the Denied filter hides the approval");
  await tapText("Back", true); await sleep(600);

  console.log("-- stopping the agent");
  await tapText("Pay up to 250.50 · 5 an hour");
  await waitText("Stop this agent");
  await tapText("Stop this agent");
  check(await waitText("Stop Harbor Travel?"), "stopping asks first");
  await tapText("Stop", true);
  check(await until(async () => (debug("agents").grants || []).length === 0, 10000), "the grant is gone");

  console.log("-- the agent directory");
  const lister = cliAt("lister"), ledger = cliAt("ledger"), quick = cliAt("quick");
  for (const c of [lister, ledger, quick]) c("init");
  const ledgerCard = ledger("card").match(/spike2:[0-9a-f]+/)[0];
  const vouched = JSON.parse(lister("agent-listing", "--card", ledgerCard, "--name", "Ledger Desk", "--maker", "Ledger Desk Ltd",
    "--about", "Files receipts and asks before sending an invoice.", "--scope", "files.read", "--scope", "payments.send"));
  const own = JSON.parse(quick("agent-listing", "--name", "Quickbook Helper", "--maker", "Quickbook", "--about", "Pays small bills.", "--scope", "payments.send"));
  const forged = { ...vouched, body: { ...vouched.body, name: "Ledger Desk Pro" } };
  check(String(debug("import_listings", { text: JSON.stringify([vouched, own, forged]) })).includes("1 did not check out"), "a tampered listing is refused on import");
  await sleep(400);
  t = await reopenAgents();
  await tapText("Give an agent a grant", true);
  await tapText("Find an agent", true);
  check(await waitText("An agent can do nothing until you set its limits"), "the directory opens from giving a grant");
  t = screenText();
  check(t.includes("Ledger Desk") && t.includes("Quickbook Helper"), "it lists the imported agents");
  check(!t.includes("Identity verified") && (t.match(/Identity not verified/g) || []).length === 2, "with no lister trusted, nobody is verified");
  await tapText("Trusted listers", true);
  check(await waitText("Nobody vouches for an agent until you add them"), "the listers screen opens");
  check(screenText().includes("None yet"), "and trusts nobody by default");
  await fill(0, "Northwind IT");
  await fill(1, vouched.body.lister);
  await tapText("Trust this lister", true);
  check(await until(async () => debug("directory").listers.length === 1, 10000), "trusting a lister is kept by the engine");
  check(await waitText("Remove"), "the lister is listed with a way to remove it");
  await tapText("Back", true); await sleep(600);
  check(await waitText("Identity verified · by Northwind IT"), "its listing now shows as verified, and by whom");
  t = screenText();
  check((t.match(/Identity not verified/g) || []).length === 1 && t.includes("Review first"), "the self-listed agent stays not verified, to review first");
  await setField(0, "ledger");
  check(!screenText().includes("Quickbook Helper"), "search narrows the list");
  await setField(0, "");
  shot("agents-6-directory");
  await tapText("Review first", true);
  check(await waitText("Listed by its maker"), "reviewing says who listed it");
  check(screenText().includes("Add it and set limits"), "and offers to add it after the warning");
  await tapText("Not now", true); await sleep(500);
  await tapText("Set limits", true);
  check(await waitText("Not allowed yet"), "Set limits adds the agent and opens its empty form");
  check(debug("overview").contacts.some((c) => c.name === "Ledger Desk"), "the agent is now a contact");
  check((debug("agents").grants || []).length === 0, "adding it granted nothing");
  await tapText("Read files", true);
  await tapText("Allow Ledger Desk", true);
  check(await waitText("Allowed until", 15000), "allowing it signs a grant");
  gr = (debug("agents").grants || [])[0];
  check(gr && gr.scopes.join() === "files.read" && gr.max_cents == null, "for reading files and no money");
  await tapText("Back", true); await sleep(600);
  t = await reopenAgents();
  check(t.includes("Ledger Desk") && t.includes("files.read · no payments"), "the tab lists it under Allowed");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
