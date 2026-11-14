// The tablet layout on the real Android app (in the emulator): the screen is set to a tablet in landscape
// (`wm size 2560x1600`, `wm density 320`, so 1280dp wide), where the app shows a rail of tabs, the chat list and the
// open conversation side by side, as in the prototype's "Tablet, landscape". Two real peers (the command-line client)
// write to the phone; the driver opens one, answers it from the conversation pane, switches to the other, visits a
// tab on the rail, and then puts the screen back to phone size and checks the single-pane flow is unchanged.
// The screen size and density are always reset at the end, so other drivers see a phone again.
// Needs Node 22+, Windows, the Android SDK with a booted emulator, the debug APK, debug builds of the relay and CLI:
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay -p spike-cli
//   ANDROID_SERIAL=emulator-5554 node android/drive_tablet.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const PORT = 18803;
const RELAY = `ws://127.0.0.1:${PORT}`;

if (!process.env.ANDROID_SERIAL) {
  // This driver wipes the app's data and changes the screen size: never let it pick a real phone by accident.
  console.error("Set ANDROID_SERIAL to the emulator to drive (for example emulator-5554).");
  process.exit(2);
}
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tapText, tapField, typeText, shot, hideKeyboard, debug } = createPhone(D);

const peer = (name) => {
  const home = join(mkdtempSync(join(tmpdir(), `spike-${name.toLowerCase()}-`)), "home");
  return (...a) => execFileSync(join(D.root, "target", "debug", "spike.exe"), ["--home", home, "--allow-insecure-transport", ...a], {
    encoding: "utf8", env: { ...process.env, SPIKE_PASSPHRASE: `${name} test passphrase`, SPIKE_RELAY: RELAY },
  });
};
async function fill(index, text) { await tapField(index); typeText(text); hideKeyboard(); }
async function until(fn, ms = 25000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(500); }
  return null;
}
const has = (sub) => nodes().some((n) => (n.text || n.desc).includes(sub));
/** The x centre of the first node that says [sub], to tell which pane it is in. */
const xOf = (sub) => nodes().find((n) => (n.text || n.desc).includes(sub))?.cx ?? -1;
const screenWidth = () => { const w = sh("wm size"); return Number((w.match(/Override size: (\d+)x/) || w.match(/Physical size: (\d+)x/) || [])[1] || 0); };
function resetSize() { try { sh("wm size reset"); sh("wm density reset"); } catch (_) {} }

try {
  console.log("-- setting up: relay on this PC, clean install on the emulator, two peers");
  resetSize();
  D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", `tcp:${PORT}`, `tcp:${PORT}`);
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);
  // A busy emulator can show "… isn't responding" over everything: wait it out.
  for (let i = 0; i < 6; i++) { const n = nodes().find((x) => x.text === "Wait"); if (!n || !screenText().includes("isn't responding")) break; sh(`input tap ${Math.round(n.cx)} ${Math.round(n.cy)}`); await sleep(2000); }
  check(await waitText("Your identity lives on this phone", 60000), "the phone shows the create screen");
  await tapText("Create my identity");
  // Back (to close the keyboard) can step the create screen back to the welcome screen when the keyboard was already
  // going away, so the fields are typed one after the other without closing it; each try starts from a clean screen.
  for (let attempt = 0; attempt < 4; attempt++) {
    if (!(await waitText("Passphrase for this phone", 5000))) { await tapText("Create my identity").catch(() => {}); await waitText("Passphrase for this phone"); }
    try {
      for (const i of [0, 1]) { await tapField(i); sh("input keyevent KEYCODE_MOVE_END " + "67 ".repeat(40)); typeText(PHONE_PASS); await sleep(300); }
    } catch (e) { console.log(`   (${e.message}; trying again)`); hideKeyboard(); continue; }
    const lens = nodes().filter((n) => n.cls.endsWith("EditText")).map((n) => (n.text || "").length);
    if (lens.length !== 2 || lens.some((l) => l !== PHONE_PASS.length)) { console.log(`   (the gate fields hold ${lens.join(", ")} characters; typing them again)`); continue; }
    const create = nodes().filter((n) => n.text === "Create my identity").pop();
    sh(`input tap ${Math.round(create.cx)} ${Math.round(create.cy)}`); await sleep(500);
    if (!(await waitText("The two passphrases differ", 3000))) break;
    await tapText("OK", true);
  }
  if (await waitText("Turn on fingerprint unlock", 6000)) await tapText("Not now").catch(() => {});
  check(await waitText("Chats"), "the identity is created");
  debug("set_relay", { url: RELAY });
  const phoneCard = debug("overview").me.card;
  const bob = peer("Bob"), dana = peer("Dana");
  for (const [name, cli] of [["Bob Tanaka", bob], ["Dana Kim", dana]]) {
    cli("init");
    cli("add", "Phone", phoneCard);
    cli("keys");
    debug("add_contact", { name, card: cli("card").match(/spike2:[0-9a-f]+/)[0] });
  }
  // The phone publishes its prekeys when it first reaches the new relay; until then a first message cannot start a session.
  for (const [cli, text] of [[bob, "Hi! Are we still on for Thursday?"], [dana, "Lunch on Friday?"]]) {
    check(await until(() => { debug("sync"); cli("send", "Phone", text); return true; }, 30000), `a peer writes to the phone: ${text}`);
  }
  check(await until(() => { debug("sync"); const c = debug("overview").contacts; return c.find((x) => x.name === "Bob Tanaka")?.last && c.find((x) => x.name === "Dana Kim")?.last; }),
    "both peers' messages reach the phone");

  console.log("-- tablet, landscape: 2560x1600 at 320dpi (1280dp wide)");
  sh("wm size 2560x1600"); sh("wm density 320");
  await sleep(2500);
  check(screenWidth() === 2560, "the emulator reports the tablet size");
  check(await waitText("Choose a chat to read it here.", 15000), "with no chat open the right pane says to choose one");
  let t = screenText();
  check(t.includes("Bob Tanaka") && t.includes("Dana Kim"), "the chat list is on screen beside it");
  check(["Chats", "Agents", "Devices", "Settings"].every((x) => has(x)), "the tabs are a rail on the left");
  check(xOf("Agents") < xOf("Bob Tanaka") && xOf("Bob Tanaka") < xOf("Choose a chat"), "rail, list and conversation sit left to right");
  shot("tablet-1-empty");

  await tapText("Bob Tanaka");
  check(await waitText("Are we still on for Thursday?"), "tapping a chat opens it in the right pane");
  t = screenText();
  check(t.includes("Dana Kim") && t.includes("Lunch on Friday?"), "the list stays beside the open conversation");
  check(!has("Back to chats"), "the conversation has no back button beside the list");
  check(xOf("Are we still on") > xOf("Dana Kim"), "the conversation is to the right of the list");
  check(nodes().filter((n) => n.cls.endsWith("EditText")).length >= 1 && has("Message Bob Tanaka"), "the composer is in the conversation pane");
  shot("tablet-2-open");

  console.log("-- answering from the conversation pane");
  const fields = nodes().filter((n) => n.cls.endsWith("EditText")).sort((a, b) => b.cx - a.cx);
  const composer = fields[0];
  sh(`input tap ${Math.round(composer.cx)} ${Math.round(composer.cy)}`); await sleep(400);
  typeText("Yes, Thursday works.");
  hideKeyboard();
  await tapText("Send", true);
  check(await waitText("Yes, Thursday works."), "the sent message shows in the pane");
  check(await until(() => bob("recv").includes("Yes, Thursday works."), 20000), "Bob receives it");

  console.log("-- switching chats, and a tab on the rail");
  await tapText("Dana Kim");
  check(await waitText("Lunch on Friday?") && await until(() => !has("Are we still on for Thursday?"), 8000), "tapping another chat swaps the right pane");
  check(has("Bob Tanaka"), "and the list is still there");
  shot("tablet-3-switched");
  await tapText("Settings", true);
  check(await until(() => !has("Lunch on Friday?") && has("Settings"), 8000), "the Settings tab fills the space beside the rail");
  shot("tablet-4-settings");
  await tapText("Chats", true);
  check(await waitText("Choose a chat to read it here."), "back on Chats the right pane is empty again");

  console.log("-- back to phone size: one screen at a time");
  resetSize();
  await sleep(2500);
  check(screenWidth() !== 2560, "the emulator is back to its own size");
  check(await waitText("Bob Tanaka"), "the chat list fills the phone");
  check(!has("Choose a chat to read it here."), "no empty conversation pane on a phone");
  await sleep(1500); // the activity is recreated for the new size: let it settle before tapping
  await until(() => has("Bob Tanaka"), 10000);
  await tapText("Bob Tanaka");
  check(await waitText("Yes, Thursday works."), "a chat opens full screen");
  check(has("Back to chats") && !has("Dana Kim"), "with a back button and no list beside it");
  shot("phone-after-tablet");
  await tapText("Back to chats", true);
  check(await waitText("Dana Kim"), "back returns to the list");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  resetSize();
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
