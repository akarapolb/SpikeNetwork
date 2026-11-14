// Organisations on the real Android app (in the emulator, ADR 0035): Northwind's admin console runs in a real
// desktop window; the phone joins Northwind's relay (enrolled mode) by pasting the token, sees the join sheet and the
// rules the console publishes, learns when the console removes it, and leaves.
// Needs Node 22+, Windows, the Android SDK with a booted emulator, the debug APK, debug builds of the relay and desktop:
//   bash android/build-native.sh && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   ANDROID_SERIAL=emulator-5554 node android/drive_org.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const PORT = 18821, ORG_PORT = 18822;
const RELAY = `ws://127.0.0.1:${PORT}`, ORG_RELAY = `ws://127.0.0.1:${ORG_PORT}`;

if (!process.env.ANDROID_SERIAL) {
  // This driver wipes the app's data: never let it pick a real phone by accident.
  console.error("Set ANDROID_SERIAL to the emulator to drive (for example emulator-5554).");
  process.exit(2);
}
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, waitText, tapText, tapField, typeText, shot, hideKeyboard, debug, screenText } = createPhone(D);
const relayExe = join(D.root, "target", "debug", "spike-relay.exe");

async function until(fn, ms = 25000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(500); }
  return null;
}
async function fill(index, text) { await tapField(index); typeText(text); hideKeyboard(); }
async function openSettings(section) {
  await tapText("Settings", true); await sleep(600);
  await tapText(section, true); await sleep(800);
}

try {
  console.log("-- setting up: two relays on this PC, the console in a desktop window, a clean install on the emulator");
  D.start(relayExe, ["--listen", `127.0.0.1:${PORT}`], {});
  await sleep(1000);
  const ann = await D.launch("ann", 9441);
  const admin = await ann.invoke("org_admin_create", { tenant: "northwind", name: "Northwind", relay: ORG_RELAY });
  const db = join(D.tmp, "org-relay.db");
  execFileSync(relayExe, ["--db", db, "tenant", "create", "northwind", "--name", "Northwind", "--admin-pub", admin.admin_pub]);
  D.start(relayExe, ["--listen", `127.0.0.1:${ORG_PORT}`, "--db", db, "--enrolled"], {});
  await sleep(1000);
  const { token } = await ann.invoke("org_issue_token", { label: "Phone", hours: 24 });

  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  for (const p of [PORT, ORG_PORT]) adb("reverse", `tcp:${p}`, `tcp:${p}`);
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

  console.log("-- joining from Settings, Home relay");
  await openSettings("Home relay");
  check(await waitText("Join an organisation"), "Home relay offers to join an organisation");
  await tapText("Join…", true);
  check(await waitText("Nothing is sent until you choose to use the relay"), "the join page says nothing is sent yet");
  // The token is several hundred characters: `adb input text` drops some on a busy emulator, so put it on the
  // clipboard and use the page's own Paste button.
  debug("_clip", token);
  await tapText("Paste", true);
  await tapText("Continue", true);
  check(await waitText("Join Northwind's relay"), "the sheet names the organisation");
  const sheet = screenText();
  check(sheet.includes("This phone only") && sheet.includes("Token valid until"), "the sheet says what the token enrols and until when");
  check(sheet.includes("That this device exists") && sheet.includes("Your name, contacts, messages"), "the sheet says what Northwind can and cannot see");
  shot("org-1-join");
  await tapText("Use this relay", true);
  check(await until(() => debug("overview").relay === ORG_RELAY), "the phone's home relay is now Northwind's");
  check(await waitText("Managed by Northwind"), "Home relay says who manages the phone");

  console.log("-- rules from the console");
  await ann.invoke("org_publish_rules", { rules: { version: 0, lock_idle_minutes: 5, keep_messages_days: 90, backups: "key_required", agents: "verified_only" } });
  debug("sync");
  await tapText("See rules", true);
  check(await waitText("Rules your organisation set for this phone"), "the rules page opens");
  const rules = screenText();
  check(rules.includes("5 minutes") && rules.includes("90 days") && rules.includes("Allowed, key required") && rules.includes("Verified only"), "the four rules reach the phone");
  check(rules.includes("These rules are enforced by the app on your device. Northwind cannot read your messages or see who you talk to."), "the page says how they are enforced");
  check(rules.includes("Leave Northwind's relay"), "the page offers to leave");
  shot("org-2-rules");

  console.log("-- the console removes the phone");
  const devices = (await ann.invoke("org_console")).devices;
  check(devices.length === 1 && devices[0].activity === "active", "the console sees one active device");
  await ann.invoke("org_revoke", { device: devices[0].device });
  check(await until(() => { try { debug("sync"); } catch (_) {} return debug("org_status").removed; }), "the phone learns it was removed");
  sh("input keyevent KEYCODE_BACK"); await sleep(600);
  await openSettings("Home relay");
  check(await waitText("Northwind removed this phone"), "Home relay says so in words");
  shot("org-3-removed");
  await tapText("Leave Northwind's relay", true);
  await tapText("Leave Northwind's relay", true).catch(() => {});
  check(await until(() => debug("overview").relay === RELAY && !debug("org_status")), "leaving goes back to the relay used before");
} catch (e) {
  console.log("FAIL", e.message || e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all passed");
  process.exit(D.failures() ? 1 : 0);
}
