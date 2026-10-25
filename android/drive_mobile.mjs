// Drives the real Android app (in the emulator) and the real desktop app against a real relay on this PC:
// onboarding, adding each other, a one-to-one chat both ways, and a large group both ways, with
// instant delivery (nobody syncs by hand). Needs Node 22+, Windows, the Android SDK with a booted
// emulator, the debug APK, and debug builds of the relay and the desktop app:
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node android/drive_mobile.mjs [screenshot-dir]
import { execFileSync, spawn } from "node:child_process";
import { writeFileSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";

import { createPhone, ADB, APK, PKG } from "./phone_lib.mjs";
const PHONE_PASS = "phone test passphrase";
const RELAY = "ws://127.0.0.1:8787";

const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });

const { adb, sh, nodes, screenText, waitText, tap, tapText, tapField, typeText, shot, back, hideKeyboard, touch, debug } = createPhone(D);

// ---- the run ---------------------------------------------------------------------------------------------

try {
  console.log("-- setting up: relay on this PC, clean install on the emulator");
  try { execFileSync("taskkill", ["/F", "/IM", "spike-relay.exe"], { stdio: "ignore" }); } catch (_) {}
  D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:8787"], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", "tcp:8787", "tcp:8787"); // the phone reaches the PC's relay as its own localhost
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);

  console.log("-- 1 · create identity (the phone's own screens)");
  check(await waitText("Your identity lives on this phone", 60000), "the phone shows the create-identity screen");
  shot("1-onboarding");
  // The emulator's keyboard now and then drops or corrects a keystroke, and Back can leave the app when the keyboard
  // was already going away. Each try clears both fields, types them, checks both hold the whole passphrase (as dots),
  // and presses Create; when the app answers "The two passphrases differ", it tries again.
  for (let attempt = 0; attempt < 4; attempt++) {
    if (attempt > 0) {
      sh(`am start -n ${PKG}/.MainActivity`);
      await waitText("Passphrase for this phone");
      for (const i of [0, 1]) { await tapField(i); sh("input keyevent KEYCODE_MOVE_END " + "67 ".repeat(40)); hideKeyboard(); }
    }
    await tapField(0); typeText(PHONE_PASS);
    hideKeyboard();
    await tapField(1); typeText(PHONE_PASS);
    hideKeyboard();
    const lens = nodes().filter((n) => n.cls.endsWith("EditText")).map((n) => (n.text || "").length);
    if (lens.length !== 2 || lens.some((l) => l !== PHONE_PASS.length)) { console.log(`   (the gate fields hold ${lens.join(", ")} characters; typing them again)`); continue; }
    await tapText("Create my identity");
    if (!(await waitText("The two passphrases differ", 3000))) break;
    console.log("   (the keyboard changed a character; typing them again)");
    await tapText("OK", true);
  }
  // With a fingerprint enrolled the create screen offers fingerprint unlock (on by default): confirm it.
  const offered = await waitText("Turn on fingerprint unlock", 6000);
  if (offered) { shot("1b-bio-at-create"); touch(); }
  check(await waitText("Chats", 30000), "the phone creates its identity and shows Chats");
  await sleep(1500);
  shot("2-chats-empty");

  console.log("-- the desktop app, on the same relay");
  const desk = await D.launch("desktop", 9341);
  const phoneCard = debug("overview").me.card;
  const deskCard = (await desk.invoke("overview")).me.card;
  await desk.invoke("add_contact", { name: "Phone", card: phoneCard });
  await desk.refresh();

  console.log("-- 10 · add a contact on the phone (paste the desktop's card)");
  debug("_clip", deskCard);
  await tapText("Add a contact");
  check(await waitText("Add a contact"), "the add-a-contact screen opens");
  await tapText("My code");
  await sleep(800);
  shot("3-add-my-code");
  await tapText("Paste a card");
  await tapField(0); typeText("Alice"); hideKeyboard();
  await tapText("Paste", true);
  await sleep(500);
  shot("4-add-pasted");
  await tapText("Add contact", true);
  check(await waitText("Alice"), "Alice is in the phone's chat list");
  shot("5-chats-with-alice");

  console.log("-- 3 · a one-to-one chat, desktop to phone and back, nobody syncing");
  await desk.invoke("send", { name: "Phone", text: "hello from the desktop" });
  check(await waitText("hello from the desktop", 20000), "the phone shows the desktop's message in its list without being asked");
  shot("6-chats-unread");
  await tapText("Alice");
  check(await waitText("hello from the desktop"), "the phone's conversation shows it");
  await sleep(500);
  await tapField(0); typeText("hi from the phone"); hideKeyboard();
  await tapText("Send", true);
  await sleep(1000);
  shot("7-conversation");
  let seen = false;
  for (let i = 0; i < 40 && !seen; i++) {
    const c = await desk.invoke("conversation", { name: "Phone" });
    seen = c.some((m) => m.body === "hi from the phone");
    if (!seen) await sleep(500);
  }
  check(seen, "the desktop received the phone's reply");

  console.log("-- 4 · verify (the phone's fingerprint screen)");
  await tapText("Details", true);
  check(await waitText("fingerprint"), "the verify screen shows fingerprints");
  shot("8-verify");
  back(); await sleep(500); back(); await sleep(800);

  console.log("-- a large group made on the desktop, joined on the phone");
  await desk.invoke("group_create_large", { name: "Org", members: ["Phone"] });
  check(await waitText("Invite", 25000), "the phone shows the invitation in its chat list");
  shot("9-chats-invite");
  await tapText("Org");
  check(await waitText("Join the group"), "the invite screen asks for consent");
  shot("10-invite");
  await tapText("Join the group");
  check(await waitText("Org", 25000), "the phone is in the group");
  await sleep(2500); // let the phone's listener subscribe to the new mailbox
  const groups = (await desk.invoke("overview")).groups;
  const gid = groups.find((g) => g.name === "Org").id;
  await desk.invoke("group_send", { id: gid, text: "welcome to the org" });
  check(await waitText("welcome to the org", 25000), "the phone shows the group message without being asked");
  shot("11-group");
  await tapField(0); typeText("thanks, glad to be here"); hideKeyboard();
  await tapText("Send", true);
  seen = false;
  for (let i = 0; i < 40 && !seen; i++) {
    const c = await desk.invoke("group_conversation", { id: gid });
    seen = c.some((m) => m.body === "thanks, glad to be here");
    if (!seen) await sleep(500);
  }
  check(seen, "the desktop received the phone's group message");
  await tapText("Details", true);
  await sleep(800);
  check((await waitText("Large")) || (await waitText("MLS")), "the members screen says it is a large group");
  shot("12-members");
  back(); await sleep(500); back(); await sleep(500);

  console.log("-- files, both ways (one-to-one)");
  await tapText("Alice", true);
  await waitText("Message Alice");
  // phone to desktop: a file inside the app's cache, sent by path (what the attach button does after copying the pick)
  sh(`run-as ${PKG} sh -c 'echo phone file contents > cache/phone-note.txt'`);
  debug("send_file", { name: "Alice", file_name: "phone-note.txt", path: `/data/data/${PKG}/cache/phone-note.txt` });
  let got = null;
  for (let i = 0; i < 40 && !got; i++) {
    const c = await desk.invoke("conversation", { name: "Phone" });
    got = c.find((m) => m.file && m.file.name === "phone-note.txt");
    if (!got) await sleep(500);
  }
  check(!!got, "the desktop received the phone's file");
  if (got) {
    const saved = await desk.invoke("save_attachment", { id: got.id });
    check(readFileSync(saved, "utf8").trim() === "phone file contents", "the desktop saved the phone's file intact");
  }
  // desktop to phone
  await desk.eval(`const b=new TextEncoder().encode("hello from the desktop file"); const e=encodeURIComponent; return await window.__TAURI__.core.invoke("send_file", b, {headers:{"x-contact":e("Phone"),"x-filename":e("desk-note.txt"),"x-caption":e("a file for you")}});`);
  check(await waitText("desk-note.txt", 25000), "the phone shows the desktop's file in the conversation");
  shot("14-file");
  await tapText("desk-note.txt");
  await sleep(2500);
  const kept = sh(`run-as ${PKG} sh -c 'cat files/received/desk-note.txt'`).trim();
  check(kept === "hello from the desktop file", "tapping the file saves it intact in the app's folder");
  back(); await sleep(800); // the "open with" chooser
  if (!screenText().includes("Message Alice")) { back(); await sleep(800); }
  back(); await sleep(800);

  console.log("-- the other tabs");
  for (const t of ["Agents", "Devices", "Settings"]) {
    await tapText(t, true);
    await sleep(900);
    shot(`13-${t.toLowerCase()}`);
  }
  check(screenText().includes("Network and privacy"), "the Settings tab is the network and privacy screen");

  console.log("-- link a second device FROM the phone (the phone is the existing device)");
  const desk2 = await D.launch("desktop2", 9342);
  const code = await desk2.invoke("link_offer");
  await tapText("Devices", true);
  await sleep(600);
  await tapText("Link a new device");
  check(await waitText("Scan the code"), "the phone's link screen asks for the new device's code");
  shot("15-link-step1");
  // type the code in chunks (it is long); the field accepts any text
  await tapField(0);
  typeText(code);
  hideKeyboard();
  const waiting = desk2.invoke("link_wait");
  await sleep(300);
  await tapText("Continue", true);
  const safetyDesk = await waiting;
  check(await waitText("Same code on both screens"), "the phone shows a safety code to compare");
  check(screenText().replace(/\s/g, "").includes(safetyDesk.replace(/\s/g, "")), "the phone and the new device show the same safety code");
  shot("16-link-compare");
  await tapText("Same code", true);
  check(await waitText("What should the new device bring"), "the phone offers what history to bring");
  shot("17-link-history");
  const received = desk2.invoke("link_receive");
  await tapText("Link device", true);
  const installed = await received;
  check(installed.contacts >= 1, "the new device received the identity and contacts");
  await sleep(1500);
  await tapText("Devices", true);
  await sleep(800);
  check((debug("overview").me.device_list || []).length === 2, "the phone's device list now has two devices");
  shot("18-devices-two");

  console.log("-- alerts while the app is closed (foreground service)");
  await tapText("Settings", true);
  await sleep(600);
  // Settings is longer than one screen: scroll until the row shows.
  let row;
  for (let i = 0; i < 6 && !row; i++) {
    sh("input swipe 540 1800 540 600 300");
    await sleep(500);
    row = nodes().find((n) => (n.text || "").includes("Alerts while the app is closed"));
  }
  check(!!row, "the settings screen has the alerts switch");
  tap(920, row.cy + 100);
  await sleep(1500);
  if ((await waitText("Allow", 4000)) && !screenText().includes("Alerts while")) { shot("19a-permission"); await tapText("Allow", true); await sleep(1500); }
  shot("19-alerts-on");
  check(sh(`dumpsys activity services ${PKG}`).includes("ListenerService"), "the foreground service is running");
  sh("input keyevent KEYCODE_HOME");
  await sleep(1500);
  await desk.invoke("send", { name: "Phone", text: "are you there?" });
  let notified = false;
  for (let i = 0; i < 40 && !notified; i++) {
    notified = /New message/.test(sh("dumpsys notification --noredact")) && sh("dumpsys notification --noredact").includes(`pkg=${PKG}`);
    if (!notified) await sleep(500);
  }
  check(notified, "a notification says a message arrived while the app was closed");
  const dump = sh("dumpsys notification --noredact");
  check(!dump.includes("are you there?"), "the notification does not carry the message text");

  if (/"count":[1-9]/.test(sh("dumpsys fingerprint")) && !process.env.NO_BIOMETRIC) {
    console.log("-- unlock with fingerprint");
    sh(`am start -n ${PKG}/.MainActivity`);
    await sleep(1500);
    await tapText("Settings", true);
    await sleep(600);
    sh("input swipe 540 1800 540 600 300");
    await sleep(500);
    const brow = nodes().find((n) => (n.text || "").includes("Unlock with fingerprint or face"));
    check(!!brow, "Settings offers fingerprint unlock (a fingerprint is enrolled)");
    tap(920, brow.cy + 40); // it was turned on at creation: turn it off ...
    await sleep(800);
    check(!screenText().includes("Type it once"), "the switch turns fingerprint unlock off");
    tap(920, brow.cy + 40); // ... and on again
    await sleep(800);
    await tapField(0); typeText(PHONE_PASS); hideKeyboard();
    if (screenText().includes(" | OK")) await tapText("OK", true);
    sh("input swipe 540 1800 540 900 300");
    await sleep(500);
    await tapText("Turn on", true);
    check(await waitText("Turn on fingerprint unlock", 5000), "the system fingerprint prompt appears");
    shot("20-bio-prompt");
    touch();
    check(await waitText("Fingerprint unlock is on", 8000), "touching the sensor turns fingerprint unlock on");
    await tapText("Lock now");
    check(await waitText("Unlock Spike Connect", 5000), "locking brings the unlock screen, and the fingerprint prompt comes up by itself");
    shot("21-bio-unlock");
    touch();
    check(await waitText("Chats", 15000), "touching the sensor unlocks the app without typing the passphrase");
    // a wrong finger must not unlock
    await tapText("Settings", true);
    await sleep(500);
    await tapText("Lock now");
    await waitText("Unlock Spike Connect", 5000);
    execFileSync(ADB, ["emu", "finger", "touch", "2"]);
    await sleep(2500);
    check(!screenText().includes("Chats |") && !screenText().includes("Message requests"), "an unknown finger does not unlock");
    shot("22-bio-wrong-finger");
    touch(); // the right one, to leave the app unlocked
    await waitText("Chats", 15000);
  }
} catch (e) {
  console.log("ERROR", e.message);
  try { shot("error"); } catch (_) {}
  D.fail();
} finally {
  await D.stop();
  const failures = D.failures();
  console.log(failures ? `${failures} FAILED` : "all passed", `· screenshots in ${D.shots}`);
  process.exit(failures ? 1 : 0);
}
