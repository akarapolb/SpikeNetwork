// The system stops the app (or kills its process): with "Keep receiving after a restart" on, the listener service
// restarts by itself, reopens the engine and a call to the phone still rings (ADR 0028). Setup shared with drive_call.mjs.
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
const RELAY_PORT = 8800;
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
  await sleep(6000);
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

  // ---- the system stops the app: the service restarts and reopens the engine, so a call still rings
  debug("_bgunlock", PHONE_PASS);
  await sleep(1500);
  sh("input keyevent KEYCODE_HOME"); // leave the app, as a person would
  await sleep(2000);
  const rings = () => (adb("logcat", "-d", "-s", "SpikeCall:I").match(/trace: call event: ring /g) || []).length;
  const callFromDesktop = async () => {
    await desk.eval(`document.querySelector('[aria-label="Voice call Phone"]').click()`);
  };
  await desk.click("Phone");

  console.log("-- 1. a call while the app is closed but running");
  adb("logcat", "-c");
  await callFromDesktop();
  check(!!(await until(() => rings() >= 1, 40000)), "the phone rings (service running)");
  await press(desk, "Hang up");
  await sleep(2500);

  console.log("-- 2. the system kills the app process (like running out of memory)");
  try { adb("root"); } catch (_) {}
  await sleep(2500);
  const pid = () => { try { return sh(`pidof ${PKG}`).trim().split(/\s+/)[0]; } catch (_) { return ""; } };
  const p0 = pid();
  check(!!p0, `the app process is running (pid ${p0})`);
  sh(`kill -9 ${p0}`);
  await sleep(1500);
  check(!pid() || pid() !== p0, "the process is gone");
  const back = await until(() => { const p = pid(); return p && p !== p0 ? p : null; }, 90000);
  check(!!back, `the system restarts the listener service by itself (pid ${back})`);
  await sleep(8000);
  adb("logcat", "-c");
  await callFromDesktop();
  check(!!(await until(() => rings() >= 1, 40000)), "a call to the phone still rings, with nobody having opened the app");
  const log = adb("logcat", "-d", "-s", "SpikeCall:I");
  check(/trace: reopened after the system stopped the app/.test(log) || rings() >= 1, "the engine was reopened in the background");
  await press(desk, "Hang up");
  await sleep(2500);

  // A phone restart is not tested here: the system only lets a boot broadcast start the listener when it sends that
  // broadcast itself (one sent from `adb` is refused as a background start), so that path is only built, not driven.
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
