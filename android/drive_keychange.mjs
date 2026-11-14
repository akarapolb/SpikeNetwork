// A verified contact's safety number changes, on the phone: Bob is re-created with a new identity under the same
// name. Pasting his new card is refused as an add and taken only through "Replace their key"; then the chat list
// row and the conversation warn, as in the design, and "Verify again" leads to the verify screen, where comparing
// the new number clears the warning. The two Bobs are desktop windows (only their cards are used).
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   ANDROID_SERIAL=emulator-5554 node android/drive_keychange.mjs [screenshot-dir]
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const RELAY_PORT = 8798;
const RELAY = `ws://127.0.0.1:${RELAY_PORT}`;

const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tap, tapText, tapField, typeText, shot, hideKeyboard, debug } = createPhone(D);
const bobRow = () => debug("overview").contacts.find((c) => c.name === "Bob");

try {
  console.log("-- setting up: a relay of our own, clean install on the emulator, two Bobs");
  D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${RELAY_PORT}`], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", `tcp:${RELAY_PORT}`, `tcp:${RELAY_PORT}`);
  sh("svc power stayon true; settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);
  await sleep(6000);
  debug("create_identity", { passphrase: PHONE_PASS });
  sh(`am force-stop ${PKG}`);
  debug("unlock", { passphrase: PHONE_PASS });
  sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Chats", 30000), "the phone has an identity and shows Chats");
  debug("set_relay", { url: RELAY });

  const bob = await D.launch("bob", 9467);
  const bob2 = await D.launch("bob2", 9468); // Bob after reinstalling: a new identity
  const bobCard = (await bob.invoke("overview")).me.card, bob2Card = (await bob2.invoke("overview")).me.card;
  debug("add_contact", { name: "Bob", card: bobCard });
  const fp1 = bobRow().fingerprint;
  debug("mark_verified", { name: "Bob", fingerprint: fp1 });
  sh(`am force-stop ${PKG}`); debug("unlock", { passphrase: PHONE_PASS }); sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Bob", 30000), "verified Bob is in the chat list");
  check(!screenText().includes("Safety number changed"), "no warning while the key is the one verified");

  console.log("-- paste Bob's new card under the same name");
  debug("_clip", bob2Card);
  await tapText("Add a contact");
  await waitText("Add a contact");
  await tapText("Paste", true);
  await tapField(0); typeText("Bob"); hideKeyboard();
  { const ps = nodes().filter((n) => n.text === "Paste" || n.desc === "Paste"); const p = ps[ps.length - 1]; tap(p.cx, p.cy); }
  await sleep(500);
  await tapText("Add contact", true);
  check(await waitText("different safety number", 10000), "a different identity under Bob's name is refused, with the choice to replace");
  check(bobRow().verified, "still verified until replaced");
  shot("keychange-refused");
  await tapText("Replace their key");

  check(await waitText("Bob's safety number changed.", 15000), "the conversation shows the red banner");
  const c = bobRow();
  check(!c.verified && c.key_changed > 0 && c.fingerprint !== fp1, "the changed key drops Bob to not verified");
  check(screenText().includes("Not verified"), "the header says not verified");
  shot("keychange-banner");
  await tapText("Back to chats");
  check(await waitText("Safety number changed", 10000), "the chat list row says Safety number changed");
  shot("keychange-list");

  console.log("-- Verify again: the verify screen with the new number");
  await tapText("Bob");
  await waitText("Verify again");
  await tapText("Verify again", true);
  check(await waitText("Verify Bob", 10000), "Verify again opens the verify screen");
  const t = screenText();
  check(t.includes("Bob's new key") && t.includes("Was verified"), "the verify screen says the number is new and when it was verified");
  check(t.includes(c.fingerprint.split(/[-\s]/)[0]), "it shows the new number");
  shot("keychange-verify");
  await tapText("They match", true);
  check(await waitText("You verified Bob", 10000), "comparing again verifies Bob");
  const after = bobRow();
  check(after.verified && !after.key_changed, "and clears the warning");
  sh("input keyevent KEYCODE_BACK"); await sleep(800);
  check(!screenText().includes("safety number changed"), "the banner is gone");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  const f = D.failures();
  console.log(f ? `${f} check(s) failed` : "all checks passed", `· screenshots in ${D.shots}`);
  process.exit(f ? 1 : 0);
}
