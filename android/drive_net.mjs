// (setup shared with drive_call.mjs)
// Voice calls between the real Android app (in the emulator) and the real desktop app (ADR 0028):
// the desktop calls the phone and the phone answers, then the phone calls the desktop and the desktop
// answers. Each time it proves audio packets arrive on both sides (the desktop uses a fake microphone
// that beeps, so the phone must receive audio energy) and that both screens show the same call code.
//
// Topology: the emulator reaches the PC as 10.0.2.2 (relay through `adb reverse`); the media goes
// between the phone's emulated network and the PC's addresses through ICE.
// Needs Node 22+, Windows, a booted emulator, the debug APK, debug builds of the relay and desktop app:
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)   (or SPIKE_DESKTOP_EXE=<other build>)
//   node android/drive_call.mjs [screenshot-dir]
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

// --disable-gpu: without it this WebView2 ends the fake camera at once (a test-harness problem, not the app).
process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream --disable-gpu";
const PHONE_PASS = "phone test passphrase";
const RELAY_PORT = 8799;
const RELAY = `ws://127.0.0.1:${RELAY_PORT}`;

const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, screenText, waitText, tapText, shot, debug } = createPhone(D);

async function until(fn, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try { const v = await fn(); if (v) return v; } catch (_) {}
    await sleep(400);
  }
  return null;
}
const overlay = (p) => p.eval(`const o=document.getElementById('callov'); return o && !o.hidden ? o.innerText : ''`);
const press = (p, label) => p.eval(`const b=[...document.querySelectorAll('#callov button')].find(x=>x.getAttribute('aria-label')===${JSON.stringify(label)}); if(!b) return false; b.click(); return true;`);
const audioIn = (p) => p.eval(`const s=await cs.pc.getStats(); let r=null; s.forEach(x=>{ if(x.type==='inbound-rtp'&&x.kind==='audio') r={packets:x.packetsReceived,energy:x.totalAudioEnergy||0}; }); return r;`);
// The phone logs packet counts (never content) every two seconds while connected.
function phoneStats() {
  const log = adb("logcat", "-d", "-s", "SpikeCall:I");
  const all = [...log.matchAll(/stats rx=(\d+) energy=([\d.eE-]+) tx=(\d+) code=(\d*) vrx=(\d+)/g)];
  const m = all[all.length - 1];
  return m ? { rx: +m[1], energy: +m[2], tx: +m[3], code: m[4], vrx: +m[5] } : null;
}
const codeOf = (t) => (/Call code (\d{4})/.exec(t) || [])[1];

let relay = null;
try {
  console.log("-- setting up: a relay of our own on port 8798, clean install on the emulator");
  relay = D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${RELAY_PORT}`], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  // "Alerts while the app is closed" on, as a person would turn it on in Settings (the app starts its listener service).
  sh(`run-as ${PKG} sh -c 'mkdir -p shared_prefs && echo "<map><boolean name=\\"notify_background\\" value=\\"true\\" /></map>" > shared_prefs/spike_app.xml'`);
  sh(`pm grant ${PKG} android.permission.RECORD_AUDIO`);
  sh(`pm grant ${PKG} android.permission.POST_NOTIFICATIONS`);
  sh(`pm grant ${PKG} android.permission.CAMERA`);
  // What the person allows under "Allow calls on the lock screen" in the app's Settings (Android 14+ asks for it).
  sh(`appops set ${PKG} USE_FULL_SCREEN_INTENT allow`);
  adb("reverse", `tcp:${RELAY_PORT}`, `tcp:${RELAY_PORT}`);
  sh("svc power stayon true; settings put system screen_off_timeout 2147483647; settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);
  await sleep(3000);
  debug("create_identity", { passphrase: PHONE_PASS });
  sh(`am force-stop ${PKG}`);
  debug("unlock", { passphrase: PHONE_PASS });
  sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Chats", 30000), "the phone has an identity and shows Chats");
  debug("set_relay", { url: RELAY });

  console.log("-- the desktop app, and each as the other's contact");
  const desk = await D.launch("desktop", 9361);
  const phoneCard = debug("overview").me.card;
  const deskCard = (await desk.invoke("overview")).me.card;
  await desk.invoke("add_contact", { name: "Phone", card: phoneCard });
  debug("add_contact", { name: "Alice", card: deskCard });
  debug("sync");
  await desk.invoke("sync");
  await desk.refresh();

  // ---- a call survives the phone losing and regaining its network (ADR 0028)
  console.log("-- desktop calls the phone, then the phone's Wi-Fi is switched off and on");
  await desk.click("Phone");
  await desk.eval(`document.querySelector('[aria-label="Voice call Phone"]').click()`);
  check(await waitText("Alice is calling", 40000), "the phone rings");
  await tapText("Answer", true);
  check(await until(async () => /\d+:\d\d/.test(await overlay(desk)) && !(await overlay(desk)).includes("Connecting"), 30000), "the call connects");
  await sleep(4000);
  const before = { phone: phoneStats(), desk: await audioIn(desk) };
  console.log("   before:", JSON.stringify(before));
  check(before.phone && before.phone.rx > 20 && before.desk && before.desk.packets > 20, "audio flows before the change");
  sh("svc wifi disable");
  // The emulator may move the call to its mobile connection within seconds: catch the message while it lasts.
  check(await waitText("Reconnecting", 15000), "the call screen says it is reconnecting while the network is gone");
  await sleep(3000);
  sh("svc wifi enable");
  console.log("   Wi-Fi back on; waiting for the call to recover");
  const recovered = await until(async () => {
    const p = phoneStats(), d = await audioIn(desk);
    return p && d && p.rx > before.phone.rx + 100 && d.packets > before.desk.packets + 100;
  }, 60000);
  const after = { phone: phoneStats(), desk: await audioIn(desk) };
  console.log("   after:", JSON.stringify(after));
  check(!!recovered, "audio flows again after the network came back (same call)");
  check(screenText().includes("Hang up"), "the phone is still in the call screen");
  check(/\d+:\d\d/.test(await overlay(desk)), "the desktop is still in the call");
  const log = adb("logcat", "-d", "-s", "SpikeCall:I");
  console.log("   phone log:", log.split(String.fromCharCode(10)).filter((l) => /connection |trace: network/.test(l)).slice(-6).map((l) => l.replace(/^.*SpikeCall: /, "")).join(" | "));
  await tapText("Hang up", true);
  check(await until(async () => (await overlay(desk)) === "", 15000), "hanging up ends it");
} catch (e) {
  console.log("ERROR", e.message);
  try { shot("error"); } catch (_) {}
  D.fail();
} finally {
  try { relay && relay.kill(); } catch (_) {}
  await D.stop();
  const failures = D.failures();
  console.log(failures ? `${failures} FAILED` : "all passed", `· screenshots in ${D.shots}`);
  process.exit(failures ? 1 : 0);
}
