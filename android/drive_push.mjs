// A wake-up through UnifiedPush brings a stopped app back (ADR 0028). (earlier header kept below)
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
import http from "node:http";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

// --disable-gpu: without it this WebView2 ends the fake camera at once (a test-harness problem, not the app).
process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream --disable-gpu";
const PHONE_PASS = "phone test passphrase";
const RELAY_PORT = 8801;
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
  relay = D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${RELAY_PORT}`, "--push-allow-insecure", "--push-min-interval-secs", "1"], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
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

  // ---- UnifiedPush: the push app gives an address; the relay wakes the phone through it when the app is not running
  const seen = [];
  const endpoint = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => { seen.push({ path: req.url, body: Buffer.concat(chunks).toString() }); res.writeHead(201); res.end(); });
  }).listen(19001, "127.0.0.1");
  D.procs.push({ kill: () => endpoint.close() });

  debug("_bgunlock", PHONE_PASS);       // lets the app open its engine itself (needed to read after a wake-up)
  await sleep(1000);
  debug("_pushendpoint", "http://127.0.0.1:19001/upphone"); // as if the push app had just given us this address
  const trace = () => adb("logcat", "-d", "-s", "SpikeCall:I");
  check(!!(await until(() => /trace: push: the relay has the address/.test(trace()), 20000)), "the phone leaves its push address with the relay");

  console.log("-- the app is stopped by the system (process killed, no service running)");
  try { adb("root"); } catch (_) {}
  await sleep(2500);
  try { adb("reverse", `tcp:${RELAY_PORT}`, `tcp:${RELAY_PORT}`); } catch (_) {} // restarting adbd forgets the tunnel to the relay
  const pid = () => { try { return sh(`pidof ${PKG}`).trim().split(/\s+/)[0]; } catch (_) { return ""; } };
  const p0 = pid();
  sh(`kill -9 ${p0}`);
  await sleep(6000);
  check(!pid(), "the app process is gone and stays gone");

  console.log("-- the desktop calls the phone: the relay cannot deliver (the phone is offline) and sends a wake-up");
  await desk.click("Phone");
  adb("logcat", "-c");
  await desk.eval(`document.querySelector('[aria-label="Voice call Phone"]').click()`);
  check(!!(await until(() => seen.length >= 1, 20000)), "the relay sends a wake-up to the registered address");
  check(seen[0] && seen[0].path === "/upphone" && seen[0].body === "spike", `the wake-up is empty of content (path ${seen[0] && seen[0].path}, body "${seen[0] && seen[0].body}")`);
  check(!pid(), "the phone is still stopped: nothing but the wake-up reached it");

  console.log("-- the push app hands the wake-up to Spike (simulated) and the phone rings");
  const tag = "pw" + Math.random().toString(36).slice(2, 6);
  sh(`am broadcast -n ${PKG}/.DebugReceiver --es cmd _pushwake --es args '{}' --es tag ${tag}`);
  check(!!(await until(() => /trace: call event: ring /.test(trace()), 45000)), "the wake-up reopens the app, it reads the call, and the phone rings");
  check(!!(await until(() => /trace: incoming call alert posted/.test(trace()), 10000)), "the incoming-call alert is posted");
  check(/trace: push: wake-up received/.test(trace()), "the app logged the wake-up");
  await press(desk, "Hang up");
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
