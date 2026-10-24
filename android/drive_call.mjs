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

process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream";
const PHONE_PASS = "phone test passphrase";
const RELAY_PORT = 8798;
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
  const all = [...log.matchAll(/stats rx=(\d+) energy=([\d.eE-]+) tx=(\d+) code=(\d*)/g)];
  const m = all[all.length - 1];
  return m ? { rx: +m[1], energy: +m[2], tx: +m[3], code: m[4] } : null;
}
const codeOf = (t) => (/Call code (\d{4})/.exec(t) || [])[1];

let relay = null;
try {
  console.log("-- setting up: a relay of our own on port 8798, clean install on the emulator");
  relay = D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${RELAY_PORT}`], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  sh(`pm grant ${PKG} android.permission.RECORD_AUDIO`);
  sh(`pm grant ${PKG} android.permission.POST_NOTIFICATIONS`);
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

  // ---- the desktop calls the phone
  console.log("-- desktop calls the phone");
  await desk.click("Phone");
  await desk.eval(`document.querySelector('[aria-label="Voice call Phone"]').click()`);
  check(await waitText("Alice is calling", 40000), "the phone rings with the caller's name");
  shot("1-phone-ringing");
  await tapText("Answer", true);
  check(await until(async () => /\d+:\d\d/.test(await overlay(desk)) && !(await overlay(desk)).includes("Connecting"), 30000), "the desktop's call connects");
  check(await waitText("Hang up", 5000), "the phone is in the call");
  await sleep(5000);
  let ps = phoneStats(), ds = await audioIn(desk);
  console.log("   phone:", JSON.stringify(ps), "desktop:", JSON.stringify(ds));
  check(ps && ps.rx > 20 && ps.energy > 0, "the phone receives the desktop's audio");
  check(ds && ds.packets > 20, "the desktop receives audio from the phone");
  const dt = await overlay(desk);
  check(codeOf(dt) && codeOf(dt) === codeOf(screenText()), `both screens show the same call code (${codeOf(dt)})`);
  shot("2-phone-in-call");
  await desk.shot("in-call");
  await press(desk, "Hang up");
  check(await waitText("Chats", 15000), "the phone's call screen closes when the desktop hangs up");

  // ---- the phone calls the desktop
  console.log("-- phone calls the desktop");
  adb("logcat", "-c");
  await tapText("Alice");
  await sleep(800);
  await tapText("Voice call", true);
  check(await until(async () => (await overlay(desk)).includes("is calling"), 40000), "the desktop rings");
  await desk.shot("ringing");
  check(await press(desk, "Answer"), "the desktop answers");
  check(await until(async () => /\d+:\d\d/.test(await overlay(desk)) && !(await overlay(desk)).includes("Connecting"), 30000), "the call connects");
  await sleep(5000);
  ps = phoneStats(); ds = await audioIn(desk);
  console.log("   phone:", JSON.stringify(ps), "desktop:", JSON.stringify(ds));
  check(ps && ps.rx > 20 && ps.energy > 0, "the phone receives the desktop's audio (phone as caller)");
  check(ds && ds.packets > 20, "the desktop receives audio from the phone (phone as caller)");
  shot("3-phone-calling");
  await tapText("Hang up", true);
  check(await until(async () => (await overlay(desk)) === "", 15000), "the desktop's call screen closes when the phone hangs up");

  // ---- declined
  console.log("-- decline");
  await tapText("Voice call", true);
  check(await until(async () => (await overlay(desk)).includes("is calling"), 40000), "the desktop rings again");
  await press(desk, "Decline");
  check(await waitText("declined the call", 15000), "the phone is told the call was declined");
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
