// Direct device-to-device messages (ADR 0027), with the relay KILLED: the real Android app (in the emulator)
// and the real desktop app talk to each other over the network without any relay running.
//
// The relay is needed exactly once, as it must be (a first message starts a session with one of the other
// device's prekeys, and prekeys come from a relay). Then the script turns direct connections on in both apps,
// gives each the other's address, KILLS THE RELAY, and checks that messages arrive both ways. Finally it brings
// the relay back and switches the phone's direct listener off to show the relay is still the fallback.
//
// Topology: the emulator reaches the PC as 10.0.2.2; the PC reaches the emulator through `adb forward`.
// Uses its own relay port (8797) and only ever stops the relay process it started itself.
// Needs Node 22+, Windows, a booted emulator, the debug APK, debug builds of the relay and the desktop app:
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)   (or SPIKE_DESKTOP_EXE=<other build>)
//   node android/drive_direct.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, ADB, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const RELAY_PORT = 8797;
const RELAY = `ws://127.0.0.1:${RELAY_PORT}`;

const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tap, tapText, tapField, typeText, shot, hideKeyboard, touch, debug } = createPhone(D);

async function until(fn, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try { const v = await fn(); if (v) return v; } catch (_) {}
    await sleep(400);
  }
  return null;
}

let relay = null;
let fwd = 0;
const startRelay = () => { relay = D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${RELAY_PORT}`], {}); };
const killRelay = () => { try { relay.kill(); } catch (_) {} relay = null; };

try {
  console.log("-- setting up: a relay of our own on port 8797, clean install on the emulator");
  startRelay();
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", `tcp:${RELAY_PORT}`, `tcp:${RELAY_PORT}`);
  sh("svc power stayon true; settings put system screen_off_timeout 2147483647; settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);

  // The identity is made through the debug door (drive_mobile.mjs covers the create screen itself); the app is
  // then restarted with its engine unlocked, so its screens show Chats.
  await sleep(6000);
  debug("create_identity", { passphrase: PHONE_PASS });
  sh(`am force-stop ${PKG}`);
  debug("unlock", { passphrase: PHONE_PASS });
  sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Chats", 30000), "the phone has an identity and shows Chats");
  debug("set_relay", { url: RELAY });

  console.log("-- the desktop app, and each as the other's contact");
  const desk = await D.launch("desktop", 9351);
  const phoneCard = debug("overview").me.card;
  const deskCard = (await desk.invoke("overview")).me.card;
  await desk.invoke("add_contact", { name: "Phone", card: phoneCard });
  debug("add_contact", { name: "Alice", card: deskCard });

  console.log("-- a session needs a relay once: the first messages go the old way");
  // Each side connects once so its prekeys are on the relay.
  debug("sync");
  await desk.invoke("sync");
  await desk.invoke("send", { name: "Phone", text: "first, by relay" });
  check(!!(await until(() => debug("conversation", { name: "Alice" }).some((m) => m.body === "first, by relay"))), "the phone received the first message through the relay");
  debug("send", { name: "Alice", text: "hello, by relay" });
  check(!!(await until(async () => (await desk.invoke("conversation", { name: "Phone" })).some((m) => m.body === "hello, by relay"))), "the desktop received the reply through the relay");

  console.log("-- turn direct connections on in both apps and give each the other's address");
  const dd = await desk.invoke("direct_enable");
  const pd = debug("direct_enable");
  check(dd.listening && pd.listening, `both are listening (desktop :${dd.port}, phone :${pd.port})`);
  check(Array.isArray(dd.addresses), "the desktop shows its own addresses");
  adb("forward", `tcp:${pd.port}`, `tcp:${pd.port}`);
  fwd = pd.port;
  await desk.invoke("direct_set_address", { name: "Phone", address: `127.0.0.1:${pd.port}` });
  debug("direct_set_address", { name: "Alice", address: `10.0.2.2:${dd.port}` });

  // The screens: the desktop's Settings and the phone's Settings tab show the card.
  await desk.eval("st.section='settings'; st.screen=null; st.directLoaded=false; render();");
  await sleep(1500);
  const settingsText = (await desk.text()).toLowerCase();
  check(settingsText.includes("direct connections") && settingsText.includes("listening."), "the desktop's Settings shows Direct connections, listening");
  await desk.eval("document.getElementById('direct-card').scrollIntoView()");
  await sleep(500);
  await desk.shot("settings-direct");
  await tapText("Settings", true);
  await sleep(900);
  sh("input swipe 540 1800 540 700 300");
  await sleep(600);
  check(screenText().includes("Direct connections"), "the phone's Settings shows Direct connections");
  shot("1-phone-direct-settings");
  sh("input swipe 540 1800 540 700 300");
  await sleep(500);
  check(screenText().includes("Listening"), "the phone says it is listening");
  shot("2-phone-direct-listening");
  await tapText("Chats", true);

  console.log("-- THE RELAY IS KILLED");
  killRelay();
  await sleep(1500);
  let relayDown = false;
  try { await desk.invoke("test_relay"); } catch (_) { relayDown = true; }
  check(relayDown, "the desktop can no longer reach the relay");

  console.log("-- desktop to phone, directly");
  await desk.invoke("send", { name: "Phone", text: "no relay, desktop to phone" });
  check(await waitText("no relay, desktop to phone", 20000), "the phone shows the message in its list, with the relay dead");
  shot("3-phone-got-direct");
  await tapText("Alice");
  check(await waitText("no relay, desktop to phone"), "the phone's conversation has it");

  console.log("-- phone to desktop, directly (typed on the phone's own screen)");
  await tapField(0); typeText("no relay, phone to desktop"); hideKeyboard();
  await tapText("Send", true);
  const got = await until(async () => (await desk.invoke("conversation", { name: "Phone" })).some((m) => m.body === "no relay, phone to desktop"), 20000);
  check(!!got, "the desktop received the phone's message with the relay dead");
  await sleep(500);
  shot("4-phone-sent-direct");
  await desk.shot("desktop-got-direct");

  const ds = await desk.invoke("direct_status");
  const ps = debug("direct_status");
  check(ds.sent_direct >= 1 && ds.received >= 1, `the desktop counts direct sends and receives (${ds.sent_direct} out, ${ds.received} in)`);
  check(ps.sent_direct >= 1 && ps.received >= 1, `the phone counts direct sends and receives (${ps.sent_direct} out, ${ps.received} in)`);
  check(ds.contacts.find((c) => c.name === "Phone").address === `127.0.0.1:${pd.port}`, "the typed address is kept for the contact");

  console.log("-- the relay comes back and is still the fallback when direct is not available");
  startRelay();
  await sleep(1500);
  debug("direct_disable");
  const before = ds.sent_fallback;
  await desk.invoke("send", { name: "Phone", text: "relay again, after direct failed" });
  check(!!(await until(() => debug("conversation", { name: "Alice" }).some((m) => m.body === "relay again, after direct failed"), 30000)), "the phone received it through the relay when its direct listener was off");
  const ds2 = await desk.invoke("direct_status");
  check(ds2.sent_fallback > before, "the desktop counted a fallback to the relay");
  shot("5-phone-after-fallback");
} catch (e) {
  console.log("ERROR", e.message);
  try { shot("error"); } catch (_) {}
  D.fail();
} finally {
  try { if (fwd) adb("forward", "--remove", `tcp:${fwd}`); } catch (_) {}
  await D.stop();
  const failures = D.failures();
  console.log(failures ? `${failures} FAILED` : "all passed", `· screenshots in ${D.shots}`);
  process.exit(failures ? 1 : 0);
}
