// Drives the real Android app (in the emulator) through app channels (ADR 0030): data for an app that is
// not set up on the phone is dropped and Settings says so; once the app is set up its data is taken in,
// never shows as chat, and Settings counts it; Remove takes the app off the phone. The other side is the
// command-line client acting for the app. Needs Node 22+, Windows, the Android SDK with a booted
// emulator, the debug APK, and debug builds of the relay and the CLI:
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay -p spike-cli
//   ANDROID_SERIAL=emulator-5554 node android/drive_apps.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
// Its own relay port, so other drivers and the relay already running on this PC are left alone.
const PORT = 18801;
const RELAY = `ws://127.0.0.1:${PORT}`;
const APP = "work.center";

if (!process.env.ANDROID_SERIAL) {
  // This driver wipes the app's data: never let it pick a real phone by accident.
  console.error("Set ANDROID_SERIAL to the emulator to drive (for example emulator-5554).");
  process.exit(2);
}
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tapText, tapField, typeText, shot, hideKeyboard, debug } = createPhone(D);

const bobHome = join(mkdtempSync(join(tmpdir(), "spike-bob-")), "home");
const cli = (...a) =>
  execFileSync(join(D.root, "target", "debug", "spike.exe"), ["--home", bobHome, "--allow-insecure-transport", ...a], {
    encoding: "utf8", env: { ...process.env, SPIKE_PASSPHRASE: "bob test passphrase", SPIKE_RELAY: RELAY },
  });
async function fill(index, text) { await tapField(index); typeText(text); hideKeyboard(); }
/** Leave Settings and come back, so it reads the engine again; then scroll to the Apps card. */
async function openApps() {
  await tapText("Chats", true);
  await sleep(500);
  await tapText("Settings", true);
  await waitText("Network and privacy");
  for (let i = 0; i < 8 && !screenText().includes("Programs on a computer connect"); i++) { sh("input swipe 540 1700 540 900 300"); await sleep(400); }
  return screenText();
}

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
  for (let attempt = 0; attempt < 4; attempt++) {
    await fill(0, PHONE_PASS);
    await fill(1, PHONE_PASS);
    const filled = nodes().filter((n) => n.cls.endsWith("EditText") && (n.text || "").length > 0).length;
    if (filled === 2) break;
    sh(`am start -n ${PKG}/.MainActivity`);
    await sleep(1500);
  }
  await tapText("Create my identity");
  if (await waitText("Turn on fingerprint unlock", 6000)) await tapText("Not now").catch(() => {});
  check(await waitText("Chats"), "the identity is created");
  debug("set_relay", { url: RELAY });
  const phoneCard = debug("overview").me.card;
  cli("init");
  cli("add", "Phone", phoneCard);
  cli("keys");
  const bobCard = cli("card").match(/spike2:[0-9a-f]+/)[0];
  debug("add_contact", { name: "Bob", card: bobCard });
  debug("sync");
  cli("app", "register", APP, "--name", "Center Work");

  console.log("-- 1 · data for an app that is not set up on the phone");
  cli("app", "send", APP, "--channel", "sheet-1", "--to", "Phone", "B12=42");
  debug("sync");
  let t = await openApps();
  check(t.includes("No apps are set up on this phone"), "Settings: no apps set up");
  check(t.includes(`Dropped: data arrived for apps that are not set up on this phone: ${APP} (1)`), "Settings says the data was dropped, and for which app");
  shot("apps-1-dropped");

  console.log("-- 2 · once the app is set up, its data is taken in");
  debug("app_register", { app: APP, name: "Center Work" });
  cli("app", "send", APP, "--channel", "sheet-1", "--to", "Phone", "C3=7");
  debug("sync");
  t = await openApps();
  check(t.includes("Center Work") && t.includes(APP) && t.includes("Received 1"), "Settings lists Center Work with what it received");
  check(!t.includes("Dropped:"), "the dropped notice is gone once the app is set up");
  shot("apps-2-listed");
  await tapText("Chats", true);
  await sleep(500);
  check(!screenText().includes("C3=7") && !screenText().includes("B12=42"), "app data never shows in the chat list");

  console.log("-- 3 · remove it");
  await openApps();
  await tapText("Remove", true);
  check(await waitText("Remove Center Work? Its data stops arriving"), "Remove asks first, saying what happens");
  await tapText("Remove Center Work", true);
  check(await waitText("No apps are set up on this phone"), "Center Work is gone from the list");
  shot("apps-3-removed");
  check(debug("apps").apps.length === 0, "and from the engine");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
