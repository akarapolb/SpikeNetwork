// Drives the real Android app (in the emulator) through backup and recovery (ADR 0029): chat with a
// contact, make a backup from Settings and save it through the system file picker, lose the phone's
// whole state, restore from the file on the create screen (through the picker again), and chat on.
// The contact is the command-line client. Needs Node 22+, Windows, the Android SDK with a booted
// emulator, the debug APK, and debug builds of the relay and the CLI:
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay -p spike-cli
//   node android/drive_backup.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, ADB, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const NEW_PASS = "a brand new phone passphrase";
// Its own relay port and (via ANDROID_SERIAL) its own emulator, so other drivers on this PC are left alone.
const PORT = 8797;
const RELAY = `ws://127.0.0.1:${PORT}`;
const FILE = "spike-identity-backup.spikebackup";

const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tap, tapText, tapField, typeText, shot, back, hideKeyboard, debug } = createPhone(D);

// Bob, the contact, is the command-line client with a home of its own.
const bobHome = join(mkdtempSync(join(tmpdir(), "spike-bob-")), "home");
const cli = (...a) =>
  execFileSync(join(D.root, "target", "debug", "spike.exe"), ["--home", bobHome, "--allow-insecure-transport", ...a], {
    encoding: "utf8", env: { ...process.env, SPIKE_PASSPHRASE: "bob test passphrase", SPIKE_RELAY: RELAY },
  });

/** Type into a field of the phone's screen and close the keyboard. */
async function fill(index, text) { await tapField(index); typeText(text); hideKeyboard(); }

try {
  console.log("-- setting up: relay on this PC, clean install on the emulator");
  D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  sh(`rm -f /sdcard/Download/${FILE}`);
  adb("reverse", `tcp:${PORT}`, `tcp:${PORT}`);
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);

  console.log("-- 1 · identity, and a contact to talk to");
  check(await waitText("Your identity lives on this phone"), "the phone shows the create screen");
  for (let attempt = 0; attempt < 4; attempt++) {
    await fill(0, PHONE_PASS);
    await fill(1, PHONE_PASS);
    const filled = nodes().filter((n) => n.cls.endsWith("EditText") && (n.text || "").length > 0).length;
    if (filled === 2) break;
    sh(`am start -n ${PKG}/.MainActivity`);
    await sleep(1500);
  }
  await tapText("Create my identity");
  check(await waitText("Chats"), "the identity is created and the chat list shows");
  debug("set_relay", { url: RELAY });
  const phoneCard = debug("overview").me.card;
  const phoneFp = debug("overview").me.fingerprint;
  cli("init");
  cli("add", "Phone", phoneCard);
  cli("keys");
  const bobCard = cli("card").match(/spike2:[0-9a-f]+/)[0];
  debug("add_contact", { name: "Bob", card: bobCard });
  debug("sync"); // the first sync publishes the phone's prekeys
  cli("send", "Phone", "hello before the loss");
  debug("sync");
  debug("send", { name: "Bob", text: "reply from the phone" });
  check(cli("recv").includes("reply from the phone"), "Bob and the phone have talked");

  console.log("-- 2 · make a backup from Settings");
  await tapText("Settings", true);
  check(await waitText("Backup and recovery"), "Settings shows Backup and recovery");
  check(await waitText("No backup yet"), "and says there is no backup yet");
  shot("backup-1-settings");
  // The card is below the relay settings: scroll to it.
  for (let i = 0; i < 6 && !screenText().includes("Make a backup key"); i++) { sh("input swipe 540 1700 540 700 300"); await sleep(400); }
  await tapText("Make a backup key");
  await sleep(600);
  const key = (screenText().match(/[0-9A-Z]{5}(?:-[0-9A-Z]{5}){4}/) || [])[0];
  check(!!key, `a backup key is shown (${key})`);
  shot("backup-2-key");
  for (let i = 0; i < 3 && !screenText().includes("Save backup file"); i++) { sh("input swipe 540 1700 540 900 300"); await sleep(400); }
  await tapText("Save backup file");
  await sleep(500);
  check(!screenText().includes("Save to"), "saving without ticking 'I wrote the key down' does nothing");
  await tapText("I wrote the key down");
  await tapText("Save backup file");
  check(await waitText("Your backup is sealed", 30000), "the backup is sealed (Argon2id, 256 MiB)");
  shot("backup-3-sealed");
  await tapText("Save to");
  // The system "create document" screen: its name field is pre-filled; Save puts it in Downloads.
  check(await waitText("Save", 15000), "the system file picker opens");
  shot("backup-4-picker");
  const saveBtn = nodes().filter((n) => (n.text || "").toUpperCase() === "SAVE").pop();
  if (saveBtn) { tap(saveBtn.cx, saveBtn.cy); await sleep(2000); }
  let saved = "";
  try { saved = sh(`ls -l /sdcard/Download/${FILE}`); } catch (_) {}
  check(saved.includes(FILE), "the file is in the phone's Downloads");
  check(await waitText("Last backup", 10000), "Settings now shows the date of the last backup");
  shot("backup-5-saved");
  const local = join(D.tmp, FILE);
  adb("pull", `/sdcard/Download/${FILE}`, local);
  check(statSync(local).size > 1000, `the file can be taken off the phone (${statSync(local).size} bytes)`);

  console.log("-- 3 · total loss, then restore from the file");
  // Messages that arrive while the phone is gone wait in the old device's mailbox; Bob sends one.
  sh(`pm clear ${PKG}`);
  sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Restore from a backup file", 20000), "a clean install offers 'Restore from a backup file'");
  await tapText("Restore from a backup file");
  check(await waitText("Choose backup file"), "the restore screen opens");
  await tapText("Choose backup file");
  // The system picker: open Downloads and choose the file.
  await sleep(2000);
  shot("backup-6-open-picker");
  let pickedFile = false;
  for (let i = 0; i < 6 && !pickedFile; i++) {
    const n = nodes().find((x) => (x.text || "").includes("spike-identity-backup"));
    if (n) { tap(n.cx, n.cy); pickedFile = true; break; }
    const dl = nodes().find((x) => (x.text || x.desc) === "Downloads");
    if (dl) { tap(dl.cx, dl.cy); await sleep(1200); continue; }
    // Open the drawer (the hamburger) to reach Downloads.
    const menu = nodes().find((x) => (x.desc || "").includes("Show roots"));
    if (menu) { tap(menu.cx, menu.cy); await sleep(1000); }
    else { sh("input swipe 10 1200 600 1200 300"); await sleep(800); }
  }
  check(pickedFile, "the backup file is chosen in the system picker");
  check(await waitText("File:", 15000), "the restore screen shows the chosen file");
  await fill(0, "WRONG-KEY-WRONG-KEY-WRONG");
  await tapText("Open backup");
  check(await waitText("wrong backup passphrase", 20000), "a wrong key is refused with a plain message");
  await tapField(0);
  // Clear the field and type the real key, lower case and without dashes (as a person might).
  sh("input keyevent KEYCODE_MOVE_END"); for (let i = 0; i < 30; i++) sh("input keyevent KEYCODE_DEL");
  typeText(key.toLowerCase().replaceAll("-", "")); hideKeyboard();
  await tapText("Open backup");
  check(await waitText("1 contact", 30000), "the summary shows what the backup holds");
  shot("backup-7-summary");
  await fill(1, NEW_PASS);
  await fill(2, NEW_PASS);
  await tapText("Restore my identity");
  check(await waitText("Bob", 40000), "the restored phone opens with Bob in the chat list");
  shot("backup-8-restored");
  const after = debug("overview").me;
  check(after.fingerprint === phoneFp && after.devices === 1, "same identity (fingerprint), exactly one device");
  check(debug("conversation", { name: "Bob" }).some((m) => m.text === "hello before the loss" || (m.body || "").includes("hello before the loss")), "the history came back");

  console.log("-- 4 · the contact and the restored phone talk");
  cli("send", "Phone", "welcome back");
  debug("sync");
  check(JSON.stringify(debug("conversation", { name: "Bob" })).includes("welcome back"), "the restored phone received Bob's message");
  debug("send", { name: "Bob", text: "I am back" });
  const got = cli("recv");
  check(got.includes("I am back") && !/KEY CHANGE/i.test(got), "Bob received it, with no key-change warning");
  console.log(D.failures() ? `${D.failures()} FAILED` : "all passed");
} catch (e) {
  console.error(e);
  D.fail();
  try { shot("backup-failure"); console.log("screen:", screenText().slice(0, 400)); } catch (_) {}
} finally {
  await D.stop();
  process.exit(D.failures() ? 1 : 0);
}
