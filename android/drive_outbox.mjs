// ADR 0040 on the real Android app (in the emulator): a message written while the relay is down shows
// "Waiting to send" in amber on the message and in the chat list, and goes by itself once the relay is back.
// The contact is the command-line client. The relay keeps a database so it can be stopped and started again.
// Needs Node 22+, Windows, the Android SDK with a booted emulator, the debug APK, debug builds of the relay and CLI:
//   bash android/build-native.sh && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay -p spike-cli
//   ANDROID_SERIAL=emulator-5554 node android/drive_outbox.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const PORT = 18851;
const RELAY = `ws://127.0.0.1:${PORT}`;
const TEXT = "written while the relay was down";

if (!process.env.ANDROID_SERIAL) {
  // This driver wipes the app's data: never let it pick a real phone by accident.
  console.error("Set ANDROID_SERIAL to the emulator to drive (for example emulator-5554).");
  process.exit(2);
}
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tapText, tapField, typeText, setField, shot, back, hideKeyboard, debug } = createPhone(D);

const bobHome = join(mkdtempSync(join(tmpdir(), "spike-bob-")), "home");
const cli = (...a) =>
  execFileSync(join(D.root, "target", "debug", "spike.exe"), ["--home", bobHome, "--allow-insecure-transport", ...a], {
    encoding: "utf8", env: { ...process.env, SPIKE_PASSPHRASE: "bob test passphrase", SPIKE_RELAY: RELAY },
  });
const db = join(D.tmp, "relay.db");
const relay = () => D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`, "--db", db], {});
async function fill(index, text) { await tapField(index); typeText(text); hideKeyboard(); }
async function until(fn, ms = 25000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(500); }
  return null;
}

try {
  console.log("-- setting up: relay on this PC, clean install on the emulator");
  let r = relay();
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
  const bobCard = cli("card").match(/spike2:[0-9a-f]+/)[0];
  debug("add_contact", { name: "Bob", card: bobCard });
  debug("sync");
  const first = debug("send", { name: "Bob", text: "hello first" });
  check(first.delivered === 1 && !first.waiting, `with the relay up a send is delivered (${JSON.stringify(first)})`);
  check(cli("recv").includes("hello first"), "Bob has the first message");

  console.log("-- the relay stops");
  r.kill();
  await sleep(1500);
  sh(`am start -n ${PKG}/.MainActivity`);
  await sleep(800);
  if (!(await waitText("Bob", 20000))) { sh("input keyevent KEYCODE_BACK"); await sleep(500); sh(`am start -n ${PKG}/.MainActivity`); await waitText("Bob", 20000); }
  await tapText("Bob");
  check(await waitText("hello first"), "the conversation opens");
  if (setField) await setField(0, TEXT); else { await tapField(0); typeText(TEXT); }
  hideKeyboard();
  await tapText("Send", true);
  check(await waitText("Waiting to send", 30000), "the message shows Waiting to send");
  const conv = debug("conversation", { name: "Bob" });
  const m = conv.find((x) => x.body === TEXT);
  check(!!m && m.waiting === true, "the history marks it waiting");
  check(conv.filter((x) => x.body !== TEXT).every((x) => !x.waiting), "the delivered message is not marked");
  check(debug("overview").contacts.find((c) => c.name === "Bob").unsent === 1, "the overview counts one unsent message");
  shot("outbox-1-waiting");
  hideKeyboard();
  back();
  await sleep(800);
  // Back can leave the app when the keyboard was already closing: bring it to the front again (state is kept).
  if (!screenText().includes("Add a contact")) { sh(`am start -n ${PKG}/.MainActivity`); await sleep(1000); }
  const list = await until(() => { const t = screenText(); return t.includes("Add a contact") && t.includes("Waiting to send") ? t : null; }, 15000);
  check(!!list, `the chat list says so too (${(list || screenText()).slice(0, 160)})`);
  shot("outbox-2-list");

  console.log("-- the relay comes back");
  r = relay();
  // Nobody touches anything: the phone's listener reconnects and sends what waits.
  const gone = await until(() => debug("overview").contacts.find((c) => c.name === "Bob").unsent === 0, 90000);
  check(!!gone, "the phone sends it by itself once the relay is back");
  check(!!(await until(() => !screenText().includes("Waiting to send"), 15000)), "the mark goes from the screen");
  const got = cli("recv");
  check(got.includes(TEXT), "Bob receives it");
  check(!cli("recv").includes(TEXT), "exactly once: a second read has nothing more");
  shot("outbox-3-sent");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
