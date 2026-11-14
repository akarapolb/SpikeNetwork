// Settings > Your identity on the real Android app (in the emulator): the name you offer people (ADR 0038) and
// changing the passphrase (ADR 0003), with fingerprint unlock kept up to date or turned off, as the app says.
// Two desktop windows are the other people. Needs Node 22+, Windows, the Android SDK with a booted emulator, the
// debug APK, and debug builds of the relay and the desktop app:
//   bash android/build-native.sh && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   ANDROID_SERIAL=emulator-5554 node android/drive_identity.mjs [screenshot-dir]
// The fingerprint steps run when a fingerprint is enrolled in the emulator; NO_BIOMETRIC=1 skips them.
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const NEW_PASS = "phone passphrase two";
const NEWER_PASS = "phone passphrase three";
const PORT = 18803;
const RELAY = `ws://127.0.0.1:${PORT}`;

if (!process.env.ANDROID_SERIAL) {
  // This driver wipes the app's data: never let it pick a real phone by accident.
  console.error("Set ANDROID_SERIAL to the emulator to drive (for example emulator-5554).");
  process.exit(2);
}
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tapText, tapField, typeText, shot, hideKeyboard, touch, debug } = createPhone(D);

async function fill(index, text) { await tapField(index); typeText(text); hideKeyboard(); }
const fields = () => nodes().filter((n) => n.cls.endsWith("EditText")).sort((a, b) => a.y1 - b.y1);
async function until(fn, ms = 25000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(500); }
  return null;
}
const scrollDown = () => sh("input swipe 540 1700 540 700 300");
async function openIdentity() {
  await tapText("Settings", true); await sleep(600);
  if (!screenText().includes("Your identity")) { sh("input keyevent KEYCODE_BACK"); await sleep(500); }
  await tapText("Your identity", true); await sleep(800);
}
/** Clear the field at `index` and type `text` (the emulator's keyboard can drop keys: check what it holds). */
async function setField(index, text, secret = false) {
  for (let attempt = 0; attempt < 3; attempt++) {
    await tapField(index);
    sh("input keyevent KEYCODE_MOVE_END " + "67 ".repeat(40));
    typeText(text); hideKeyboard();
    const f = fields()[index];
    if (f && (secret ? (f.text || "").length === text.length : f.text === text)) return true;
  }
  return false;
}
const bio = () => /"count":[1-9]/.test(sh("dumpsys fingerprint")) && !process.env.NO_BIOMETRIC;

try {
  console.log("-- setting up: relay on this PC, clean install on the emulator");
  D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", `tcp:${PORT}`, `tcp:${PORT}`);
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh("svc power stayon true");
  sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Your identity lives on this phone", 60000), "the phone shows the create screen");
  await tapText("Create my identity"); await waitText("Passphrase for this phone");
  for (let attempt = 0; attempt < 4; attempt++) {
    await fill(0, PHONE_PASS);
    await fill(1, PHONE_PASS);
    if (fields().filter((n) => (n.text || "").length === PHONE_PASS.length).length === 2) break;
    sh(`am start -n ${PKG}/.MainActivity`);
    await sleep(1500);
    for (const i of [0, 1]) { await tapField(i); sh("input keyevent KEYCODE_MOVE_END " + "67 ".repeat(40)); hideKeyboard(); }
  }
  await tapText("Create my identity");
  const withBio = bio();
  if (await waitText("Turn on fingerprint unlock", 6000)) { if (withBio) touch(); else await tapText("Not now").catch(() => {}); }
  check(await waitText("Chats", 30000), "the identity is created");
  debug("set_relay", { url: RELAY });
  const phoneCard = debug("overview").me.card;

  console.log("-- someone with a name writes to the phone first");
  const dana = await D.launch("dana", 9421);
  await dana.invoke("set_profile_name", { name: "Dana Okafor" });
  await dana.invoke("add_contact", { name: "Phone", card: phoneCard });
  await dana.invoke("send", { name: "Phone", text: "Hello from Dana" });
  const req = await until(() => { debug("sync"); const r = debug("message_requests"); return r.length ? r[0] : null; });
  check(req && req.name === "Dana Okafor", `the phone's request carries the offered name (${req && req.name})`);
  sh(`am start -n ${PKG}/.MainActivity`);
  await tapText("Chats", true); await sleep(800);
  await until(async () => screenText().includes("message request"), 10000);
  await tapText("message request"); await sleep(800);
  check(await waitText("Calls themselves Dana Okafor. Not checked."), "the request says what they call themselves");
  await tapText("Accept", true); await sleep(600);
  check(fields()[0]?.text === "Dana Okafor", `Accept suggests the offered name (${fields()[0]?.text})`);
  check(screenText().includes("Accept as Dana Okafor"), "the button names who is added");
  shot("identity-1-request");
  check(await setField(0, "Dana from work"), "the suggestion can be replaced");
  await tapText("Accept as Dana from work"); await sleep(1500);
  const names = debug("overview").contacts.map((c) => c.name);
  check(names.includes("Dana from work") && !names.includes("Dana Okafor"), `the phone's own name for her wins (${JSON.stringify(names)})`);
  sh("input keyevent KEYCODE_BACK"); await sleep(500);

  console.log("-- the phone's name, in Settings > Your identity");
  await openIdentity();
  let t = screenText();
  check(t.includes("Name") && t.includes("Shown to people you add. Each contact can rename you."), "the identity page has the Name row");
  check(t.includes("Passphrase") && t.includes("Protects the keys on this phone.") && t.includes("Change"), "the identity page has the Passphrase row with Change");
  check(await setField(0, "Pat Lee"), "a name can be typed");
  await tapText("Save name", true); await sleep(1200);
  check(debug("overview").me.name === "Pat Lee", "Save name stores it");
  check(await waitText("Name saved."), "the phone says it is saved");
  shot("identity-2-name");
  const erin = await D.launch("erin", 9422);
  debug("add_contact", { name: "Erin", card: (await erin.invoke("overview")).me.card });
  debug("send", { name: "Erin", text: "Hi Erin, this is Pat" });
  const er = await until(async () => { await erin.invoke("sync"); const r = await erin.invoke("message_requests"); return r.length ? r[0] : null; });
  check(er && er.name === "Pat Lee", `the name reaches someone the phone writes to first (${er && er.name})`);

  console.log("-- change the passphrase");
  await openIdentity();
  await tapText("Change", true); await sleep(600);
  t = screenText();
  check(t.includes("Current passphrase") && t.includes("New passphrase again"), "Change opens the form");
  // A wrong current passphrase is refused by the engine.
  await setField(1, "not the passphrase", true);
  await setField(2, NEW_PASS, true);
  await setField(3, NEW_PASS, true);
  scrollDown(); await sleep(400);
  await tapText("Change passphrase", true);
  check(await waitText("wrong passphrase", 8000), "a wrong current passphrase is refused");
  await setField(1, PHONE_PASS, true);
  scrollDown(); await sleep(400);
  shot("identity-3-passphrase");
  await tapText("Change passphrase", true);
  if (withBio) {
    check(await waitText("Turn on fingerprint unlock", 8000), "with fingerprint unlock on, the phone asks for the fingerprint to keep it");
    touch();
    check(await waitText("Fingerprint unlock uses the new one", 8000), "the phone says fingerprint unlock now uses the new passphrase");
    debug("lock");
    sh(`am force-stop ${PKG}`); sh(`am start -n ${PKG}/.MainActivity`);
    check(await waitText("Unlock Spike Connect", 15000), "after a restart the fingerprint prompt comes up");
    touch();
    check(await waitText("Chats", 15000), "the fingerprint unlocks with the new passphrase");

    console.log("-- change again and refuse the fingerprint: fingerprint unlock is turned off, and the phone says so");
    await openIdentity();
    await tapText("Change", true); await sleep(600);
    await setField(1, NEW_PASS, true); await setField(2, NEWER_PASS, true); await setField(3, NEWER_PASS, true);
    scrollDown(); await sleep(400);
    await tapText("Change passphrase", true);
    check(await waitText("Turn on fingerprint unlock", 8000), "the fingerprint prompt comes up again");
    await tapText("Use passphrase", true).catch(() => sh("input keyevent KEYCODE_BACK"));
    check(await waitText("Fingerprint unlock is now off", 8000), "declining it turns fingerprint unlock off and says so");
    shot("identity-4-bio-off");
  } else {
    check(await waitText("Passphrase changed on this phone.", 8000), "the phone confirms the change");
  }
  const latest = withBio ? NEWER_PASS : NEW_PASS;
  check(debug("overview").contacts.length === 2, "the phone stays unlocked and working");

  console.log("-- restart: only the new passphrase opens it, and everything is there");
  debug("lock");
  sh(`am force-stop ${PKG}`); sh(`am start -n ${PKG}/.MainActivity`);
  await waitText("Unlock", 15000);
  check(!screenText().includes("Unlock with fingerprint or face") || !withBio, "fingerprint unlock is no longer offered once it is off");
  await setField(0, PHONE_PASS, true);
  await tapText("Unlock", true);
  check(await waitText("wrong passphrase", 10000), "the old passphrase no longer unlocks");
  if (screenText().includes(" | OK")) await tapText("OK", true);
  await setField(0, latest, true);
  await tapText("Unlock", true);
  check(await waitText("Chats", 20000), "the new passphrase unlocks");
  const ov = debug("overview");
  check(ov.contacts.length === 2 && ov.me.name === "Pat Lee", "contacts and the name are still there");
  await dana.invoke("send", { name: "Phone", text: "Still reaching you?" });
  const got = await until(() => { debug("sync"); return debug("conversation", { name: "Dana from work" }).some((x) => x.body.includes("Still reaching")); });
  check(!!got, "messages still arrive after the change");
  shot("identity-5-after");
} catch (e) {
  console.log("ERROR", e.message);
  try { shot("error"); } catch (_) {}
  D.fail();
} finally {
  await D.stop();
  const failures = D.failures();
  console.log(`${failures ? `${failures} FAILED` : "all passed"} · screenshots in ${D.shots}`);
  process.exit(failures ? 1 : 0);
}
