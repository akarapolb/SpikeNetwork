// Message previews in Android notifications (opt-in): with the setting on, a message that arrives while the app is
// in the background shows who wrote and what, and the notification's 👍 and Reply buttons work without opening the
// app. With the setting off (the default), the notification says only "New message". The desktop app is the other
// person.
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)   (or SPIKE_DESKTOP_EXE=<other build>)
//   node android/drive_previews.mjs [screenshot-dir]
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const RELAY_PORT = 8799;
const RELAY = `ws://127.0.0.1:${RELAY_PORT}`;
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, screenText, waitText, tapText, shot, debug, nodes, tap } = createPhone(D);

async function until(fn, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try { const v = await fn(); if (v) return v; } catch (_) {}
    await sleep(500);
  }
  return null;
}
/** Start (or restart) the app with these settings, unlocked, in the background. */
async function startWith(previews) {
  sh(`am force-stop ${PKG}`);
  sh(`run-as ${PKG} sh -c 'mkdir -p shared_prefs && echo "<map><boolean name=\\"notify_background\\" value=\\"true\\" /><boolean name=\\"notification_previews\\" value=\\"${previews}\\" /></map>" > shared_prefs/spike_app.xml'`);
  debug("unlock", { passphrase: PHONE_PASS });
  sh(`am start -n ${PKG}/.MainActivity`);
  await waitText("Chats", 30000);
  sh("input keyevent KEYCODE_HOME");
  await sleep(2500);
}
const shade = () => { sh("cmd statusbar expand-notifications"); };
/** Open the notification shade until it shows `text` (it may still be closing after a button was pressed). */
async function inShade(text, ms = 30000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    shade();
    await sleep(1500);
    if (screenText().includes(text)) return true;
  }
  console.log(`   (the shade shows: ${screenText().slice(0, 300)})`);
  return false;
}
const closeShade = () => { sh("cmd statusbar collapse"); };
/** Open the top notification (Android shows its buttons only when it is expanded, as for any messaging app). */
async function expandTop(text) {
  const row = nodes().find((n) => (n.text || n.desc).includes(text));
  const arrow = nodes().filter((n) => /expand/i.test(n.desc) && row && Math.abs(n.cy - row.cy) < 200).sort((a, b) => a.cy - b.cy)[0];
  if (arrow) tap(arrow.cx, arrow.cy);
  await sleep(1200);
}
const fromPhone = async (desk, body) => {
  await desk.invoke("sync");
  return (await desk.invoke("conversation", { name: "Phone" })).find((m) => !m.out && m.body === body);
};

let relay = null;
try {
  console.log("-- setting up: a relay of our own, clean install on the emulator");
  relay = D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${RELAY_PORT}`], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  sh(`pm grant ${PKG} android.permission.POST_NOTIFICATIONS`);
  adb("reverse", `tcp:${RELAY_PORT}`, `tcp:${RELAY_PORT}`);
  sh("svc power stayon true; settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);
  await sleep(6000);
  debug("create_identity", { passphrase: PHONE_PASS });
  debug("set_relay", { url: RELAY });
  const desk = await D.launch("desktop", 9371);
  const phoneCard = debug("overview").me.card;
  const deskCard = (await desk.invoke("overview")).me.card;
  await desk.invoke("add_contact", { name: "Phone", card: phoneCard });
  debug("add_contact", { name: "Alice", card: deskCard });
  debug("sync");
  await desk.invoke("sync");

  console.log("-- previews off (the default): the notification does not say what");
  await startWith(false);
  await desk.invoke("send", { name: "Phone", text: "the secret plan" });
  check(await inShade("New message"), "a notification says a message arrived");
  check(!screenText().includes("the secret plan"), "with previews off it never shows the text");
  shot("1-private");
  closeShade();

  console.log("-- previews on: who and what, with reactions and Reply");
  await startWith(true);
  await desk.invoke("send", { name: "Phone", text: "lunch at 1?" });
  const sent = (await desk.invoke("conversation", { name: "Phone" })).filter((m) => m.out && m.body === "lunch at 1?").pop();
  check(await inShade("lunch at 1?"), "the notification shows the message");
  check(screenText().includes("Alice"), "and who wrote it");
  for (let i = 0; i < 4 && !nodes().some((n) => n.text === "Reply"); i++) await expandTop("lunch at 1?");
  shot("2-preview");
  await tapText("👍", true);
  const reacted = await until(async () => {
    await desk.invoke("sync");
    const m = (await desk.invoke("conversation", { name: "Phone" })).find((x) => x.id === sent.id);
    return m && m.reactions.some((r) => r.emoji === "👍");
  }, 30000);
  check(reacted, "👍 on the notification reacts to that message (the desktop sees it)");
  closeShade();

  await desk.invoke("send", { name: "Phone", text: "and dinner?" });
  const sent2 = (await desk.invoke("conversation", { name: "Phone" })).filter((m) => m.out && m.body === "and dinner?").pop();
  check(await inShade("and dinner?"), "a second message shows in the notification");
  for (let i = 0; i < 4 && !nodes().some((n) => n.text === "Reply"); i++) await expandTop("and dinner?");
  await tapText("Reply", true);
  await sleep(1500);
  const field = nodes().find((n) => n.cls.endsWith("EditText"));
  check(!!field, "Reply opens a text box on the notification");
  if (field) {
    tap(field.cx, field.cy);
    await sleep(1200);
    // The emulator's keyboard sometimes opens a stylus tutorial over the box: close it and tap the box again.
    if (screenText().includes("stylus")) { await tapText("Cancel", true); await sleep(800); const f = nodes().find((n) => n.cls.endsWith("EditText")); if (f) tap(f.cx, f.cy); await sleep(800); }
    sh("input text 'yes%splease'");
    await sleep(500);
    shot("3-replying");
    const send = nodes().find((n) => /send/i.test(n.desc));
    if (send) tap(send.cx, send.cy); else sh("input keyevent KEYCODE_ENTER");
  }
  const answer = await until(() => fromPhone(desk, "yes please"), 30000);
  check(answer && answer.reply && answer.reply.id === sent2.id, `the reply reaches the desktop, as a reply to that message (${answer ? JSON.stringify(answer.reply) : "nothing"})`);
  closeShade();
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  try { adb("reverse", "--remove", `tcp:${RELAY_PORT}`); } catch (_) {}
  await D.stop();
  console.log(`${D.failures() ? `${D.failures()} FAILED` : "all passed"} · screenshots in ${D.shots}`);
  process.exit(D.failures() ? 1 : 0);
}
