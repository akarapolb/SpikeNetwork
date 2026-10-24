// Drives the real Android app (in the emulator) and the real desktop app against a real relay on this PC:
// onboarding, adding each other, a one-to-one chat both ways, and a large group both ways, with
// instant delivery (nobody syncs by hand). Needs Node 22+, Windows, the Android SDK with a booted
// emulator, the debug APK, and debug builds of the relay and the desktop app:
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node android/drive_mobile.mjs [screenshot-dir]
import { execFileSync, spawn } from "node:child_process";
import { writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";

const SDK = process.env.ANDROID_HOME || "D:/Android/sdk";
const ADB = `${SDK}/platform-tools/adb.exe`;
const APK = "android/app/build/outputs/apk/debug/app-debug.apk";
const PKG = "network.spike.connect";
const PHONE_PASS = "phone test passphrase";
const RELAY = "ws://127.0.0.1:8787";

const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });

const adb = (...a) => execFileSync(ADB, a, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
const sh = (cmd) => adb("shell", cmd);

// ---- the phone's screen, read through uiautomator ---------------------------------------------------

function nodes() {
  let xml = "";
  for (let i = 0; i < 5 && !xml.includes("<node "); i++) {
    // uiautomator now and then answers "null root node" while a screen is changing: ask again.
    try { sh("uiautomator dump /sdcard/ui.xml"); xml = adb("exec-out", "cat", "/sdcard/ui.xml"); } catch (_) {}
    if (!xml.includes("<node ")) execFileSync("node", ["-e", "setTimeout(()=>{},600)"]);
  }
  const out = [];
  for (const m of xml.matchAll(/<node [^>]*>/g)) {
    const a = Object.fromEntries([...m[0].matchAll(/([\w-]+)="([^"]*)"/g)].map((x) => [x[1], x[2]]));
    const b = (a.bounds || "").match(/\[(\d+),(\d+)\]\[(\d+),(\d+)\]/);
    if (!b) continue;
    const [x1, y1, x2, y2] = b.slice(1).map(Number);
    out.push({
      text: (a.text || "").replace(/&amp;/g, "&").replace(/&quot;/g, '"').replace(/&#10;/g, "\n"),
      desc: a["content-desc"] || "", cls: a.class || "", cx: (x1 + x2) / 2, cy: (y1 + y2) / 2, y1,
    });
  }
  return out;
}
const screenText = () => nodes().map((n) => n.text || n.desc).filter(Boolean).join(" | ");

async function waitText(sub, ms = 20000) {
  const end = Date.now() + ms;
  let last = "";
  while (Date.now() < end) {
    last = screenText();
    if (last.includes(sub)) return true;
    await sleep(500);
  }
  console.log(`   (waited for "${sub}"; the phone shows: ${last.slice(0, 300)})`);
  return false;
}

function tap(x, y) { sh(`input tap ${Math.round(x)} ${Math.round(y)}`); }
async function tapText(sub, exact = false) {
  const n = nodes().find((x) => (exact ? x.text === sub || x.desc === sub : (x.text || x.desc).includes(sub)));
  if (!n) throw new Error(`phone: nothing says "${sub}"`);
  tap(n.cx, n.cy);
  await sleep(500);
}
async function tapField(index) {
  const f = nodes().filter((n) => n.cls.endsWith("EditText")).sort((a, b) => a.y1 - b.y1)[index];
  if (!f) throw new Error(`phone: no text field #${index}`);
  tap(f.cx, f.cy);
  await sleep(400);
}
function typeText(text) {
  // `input text` takes %s for a space and trips over shell characters; ours are letters, digits and punctuation.
  for (let i = 0; i < text.length; i += 40) {
    sh(`input text '${text.slice(i, i + 40).replace(/ /g, "%s")}'`);
  }
}
function shot(label) {
  writeFileSync(join(D.shots, `phone-${label}.png`), execFileSync(ADB, ["exec-out", "screencap", "-p"], { maxBuffer: 64 * 1024 * 1024 }));
}
function back() { sh("input keyevent KEYCODE_BACK"); }
/** Close the soft keyboard if it is showing (Back would otherwise leave the screen). The emulator must
 *  not be in hardware-keyboard mode (hw.keyboard=no in its config), or Gboard shows a toolbar over the app. */
function hideKeyboard() {
  if (sh("dumpsys input_method").includes("mInputShown=true")) sh("input keyevent KEYCODE_BACK");
}

/** Run a bridge command inside the app (debug builds) and return its JSON answer. */
function debug(cmd, args = {}) {
  const tag = `t${Math.random().toString(36).slice(2, 8)}`;
  adb("logcat", "-c");
  const raw = typeof args === "string" ? args : JSON.stringify(args);
  sh(`am broadcast -n ${PKG}/.DebugReceiver --es cmd '${cmd}' --es args '${raw.replace(/'/g, "")}' --es tag ${tag}`);
  for (let i = 0; i < 60; i++) {
    const log = adb("logcat", "-d", "-s", "SpikeDebug:I");
    if (log.includes(`${tag}#end`)) {
      const parts = [...log.matchAll(new RegExp(`${tag}#(\\d+) (.*)`, "g"))].sort((a, b) => a[1] - b[1]).map((m) => m[2].replace(/\r$/, ""));
      const answer = JSON.parse(parts.join(""));
      if (answer.err) throw new Error(`phone ${cmd}: ${answer.err}`);
      return answer.ok;
    }
    execFileSync("node", ["-e", "setTimeout(()=>{},250)"]);
  }
  throw new Error(`phone ${cmd}: no answer`);
}

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
  check(await waitText("Your identity lives on this phone"), "the phone shows the create-identity screen");
  shot("1-onboarding");
  await tapField(0); typeText(PHONE_PASS);
  hideKeyboard();
  await tapField(1); typeText(PHONE_PASS);
  hideKeyboard();
  await tapText("Create my identity");
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

  console.log("-- the other tabs");
  for (const t of ["Agents", "Devices", "Settings"]) {
    await tapText(t, true);
    await sleep(900);
    shot(`13-${t.toLowerCase()}`);
  }
  check(screenText().includes("Network and privacy"), "the Settings tab is the network and privacy screen");
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
