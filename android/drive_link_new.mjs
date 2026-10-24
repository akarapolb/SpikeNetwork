// The phone as the NEW device: it picks "I already use Spike on another device", shows a code, a desktop that
// already has the identity enters it, both show the same safety code, and the phone ends up with the desktop's
// identity, contacts and history. Same setup as drive_mobile.mjs.
//   node android/drive_link_new.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, ADB, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const D = createDrive({ relayUrl: "ws://127.0.0.1:8787", shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { touch, adb, sh, nodes, screenText, waitText, tap, tapText, tapField, typeText, shot, hideKeyboard, debug } = createPhone(D);

try {
  console.log("-- setting up: relay, a desktop with an identity, contacts and history");
  try { execFileSync("taskkill", ["/F", "/IM", "spike-relay.exe"], { stdio: "ignore" }); } catch (_) {}
  D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:8787"], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", "tcp:8787", "tcp:8787");
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  const desk = await D.launch("desktop", 9341);
  const friend = await D.launch("friend", 9342);
  await desk.invoke("add_contact", { name: "Friend", card: (await friend.invoke("overview")).me.card });
  await friend.invoke("add_contact", { name: "Desk", card: (await desk.invoke("overview")).me.card });
  await friend.invoke("sync", {}); // publishes the friend's prekeys
  await desk.invoke("send", { name: "Friend", text: "history before the phone" });
  const deskFinger = (await desk.invoke("overview")).me.fingerprint;

  console.log("-- the phone: \"I already use Spike on another device\"");
  sh(`am start -n ${PKG}/.MainActivity`);
  await waitText("Your identity lives on this phone");
  await sleep(1500);
  await tapField(0); typeText(PHONE_PASS); hideKeyboard();
  await tapField(1); typeText(PHONE_PASS); hideKeyboard();
  // a bare button press with no passphrase says what to do
  sh("input swipe 540 1800 540 700 300");
  await sleep(500);
  await tapText("I already use Spike on another device");
  // with a fingerprint enrolled the phone offers fingerprint unlock here too
  if (await waitText("Turn on fingerprint unlock", 6000)) touch();
  check(await waitText("Waiting for the other device", 30000), "the phone shows a link code and waits");
  shot("l1-code");
  const code = nodes().map((n) => n.text).filter((x) => x.length > 40).sort((a, b) => b.length - a.length)[0];
  check(!!code, "the screen shows the link code as text");

  console.log("-- the desktop that has the identity enters the code");
  const phoneSafety = (async () => await waitText("Same code on both screens", 60000))();
  const deskSafety = await desk.invoke("link_begin", { code });
  check(await phoneSafety, "the phone shows the safety code");
  check(screenText().replace(/\s/g, "").includes(deskSafety.replace(/\s/g, "")), "the phone and the desktop show the same safety code");
  shot("l2-compare");
  await tapText("Same code", true);
  const finished = await desk.invoke("link_finish", { history: "all", days: 30 });
  check(finished.contacts >= 1, "the desktop sent its identity and contacts");
  check(await waitText("Chats", 60000), "the phone ends up in the chat list");
  await sleep(2000);
  shot("l3-chats");
  const o = debug("overview");
  check(o.me.fingerprint === deskFinger, "the phone now has the desktop's identity (same fingerprint)");
  check(o.me.devices === 2 || (o.me.device_list || []).length === 2, "the identity now has two devices");
  check(screenText().includes("Friend"), "the phone shows the desktop's contact");
  await tapText("Friend");
  check(await waitText("history before the phone", 20000), "the phone received the history");
  shot("l4-history");
  // the linked phone can talk as the same person
  await desk.invoke("send", { name: "Friend", text: "after linking" });
  check(await waitText("after linking", 25000), "a message sent by the desktop after linking also reaches the phone");
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
