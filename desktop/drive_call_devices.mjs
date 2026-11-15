// Drives two real desktop app windows with fake microphones and cameras through Settings > Calls: the device lists
// fill, the microphone level moves, a choice is kept, and a call then uses the chosen microphone. Also checks that
// "Ring on this computer" off leaves this computer silent while the call rings elsewhere.
// Needs Node 22+, Windows (WebView2), and the debug builds of the app and relay:
//   cargo build -p spike-relay            (repository root)
//   cargo build                           (desktop/src-tauri)   [SPIKE_DESKTOP_EXE=... for another build]
//   node desktop/drive_call_devices.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream";
const RELAY = "ws://127.0.0.1:18793";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

async function until(fn, what, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { if (await fn()) return true; } catch (_) {} await sleep(300); }
  check(false, `timed out: ${what}`);
  return false;
}
const overlay = (p) => p.eval(`const o=document.getElementById('callov'); return o && !o.hidden ? o.innerText : ''`);
const press = (p, label) => p.eval(`const b=[...document.querySelectorAll('#callov button')].find(x=>x.getAttribute('aria-label')===${JSON.stringify(label)}); if(!b) return false; b.click(); return true;`);
const options = (p, id) => p.eval(`const s=document.getElementById(${JSON.stringify(id)}); return s ? [...s.options].map(o=>({v:o.value,t:o.text})) : null;`);

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18793"], {});
  await sleep(1000);
  const alice = await launch("alice", 9351);
  const bob = await launch("bob", 9352);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  await alice.refresh(); await bob.refresh();

  // --- Settings > Calls lists the devices
  await alice.eval(`st.section='settings'; st.setSec='calls-card'; st.screen=null; render();`);
  check(await until(async () => { const o = await options(alice, "calls-mic"); return o && o.length > 1 && o[0].t === "System default"; }, "microphone list fills"), "the microphone list fills");
  const mics = await options(alice, "calls-mic");
  console.log("   microphones:", JSON.stringify(mics.map((o) => o.t)));
  const cams = await options(alice, "calls-cam");
  console.log("   cameras:", JSON.stringify(cams && cams.map((o) => o.t)));
  check(cams && cams.length > 1, "the camera list fills");
  const spk = await alice.eval(`const s=document.getElementById('calls-spk'); if (s) return 'list:' + s.options.length; const n=document.getElementById('calls-no-spk'); return n ? 'note:' + n.textContent : (document.getElementById('calls-card').innerText.includes('default speaker') ? 'no setSinkId' : '');`);
  console.log("   speaker:", spk);
  check(!!spk, "the speaker row shows a list or says the default speaker plays");
  check(!(await alice.eval(`return !!document.getElementById('calls-problem')`)), "no device problem is shown");
  check(mics.slice(1).every((o) => o.t && !/^Microphone \d+$/.test(o.t)), "devices are listed by name");

  // the level meter moves (the fake microphone plays a beep)
  let peak = 0;
  for (let i = 0; i < 25 && peak === 0; i++) { peak = Math.max(peak, Number(await alice.eval(`const m=document.getElementById('calls-meter'); return m ? m.getAttribute('aria-valuenow') : 0`))); await sleep(120); }
  check(peak > 0, `the microphone level moves while the section is open (${peak})`);
  check(await alice.eval(`return !!(micLevel.stream && micLevel.stream.getAudioTracks()[0].readyState==='live')`), "the meter holds the microphone while the section is open");

  // the Test button plays without an error
  await alice.eval(`document.getElementById('calls-test').click()`);
  await sleep(800);
  check(!(await alice.eval(`return document.getElementById('toast').textContent.includes('could not be played')`)), "the speaker Test plays");

  // pick the last microphone and camera; the choice is kept
  const pickMic = mics[mics.length - 1], pickCam = cams[cams.length - 1];
  await alice.eval(`const s=document.getElementById('calls-mic'); s.value=${JSON.stringify(pickMic.v)}; s.dispatchEvent(new Event('change'));`);
  await alice.eval(`const s=document.getElementById('calls-cam'); s.value=${JSON.stringify(pickCam.v)}; s.dispatchEvent(new Event('change'));`);
  check((await alice.eval(`return localStorage.getItem('callMic')`)) === pickMic.v, `the microphone choice is kept (${pickMic.t})`);
  check((await alice.eval(`return localStorage.getItem('callCam')`)) === pickCam.v, `the camera choice is kept (${pickCam.t})`);
  check(await until(() => alice.eval(`return !!(micLevel.stream && micLevel.stream.getAudioTracks()[0].getSettings().deviceId===${JSON.stringify(pickMic.v)})`), "meter on chosen mic", 5000), "the meter switches to the chosen microphone");
  await alice.eval(`render();`); await sleep(300);
  check((await alice.eval(`return document.getElementById('calls-mic').value`)) === pickMic.v, "the list shows the chosen microphone after a redraw");
  await alice.shot("settings-calls");

  // leaving the section lets the microphone go
  await alice.eval(`st.section='chats'; render();`);
  check(await until(() => alice.eval(`return !micLevel.stream`), "meter stops", 5000), "leaving the section lets the microphone go");

  // --- a video call uses the chosen microphone and camera
  await alice.click("Bob");
  await alice.eval(`document.querySelector('[aria-label="Video call Bob"]').click()`);
  check(await until(async () => (await overlay(bob)).includes("Alice is calling"), "bob rings", 30000), "bob rings");
  check(await press(bob, "Answer"), "bob answers");
  const connected = (p) => async () => /\d+:\d\d/.test(await overlay(p)) && !(await overlay(p)).includes("Connecting");
  check(await until(connected(alice), "alice connected"), "the call connects");
  const used = await alice.eval(`return { mic: cs.local.getAudioTracks()[0].getSettings().deviceId, cam: (cs.local.getVideoTracks()[0]||{getSettings:()=>({})}).getSettings().deviceId }`);
  console.log("   in call:", JSON.stringify(used));
  check(used.mic === pickMic.v, "the call uses the chosen microphone");
  check(used.cam === pickCam.v, "the call uses the chosen camera");
  await sleep(2500);
  const a = await bob.eval(`const s=await cs.pc.getStats(); let r=null; s.forEach(x=>{ if(x.type==='inbound-rtp'&&x.kind==='audio') r={packets:x.packetsReceived}; }); return r;`);
  check(a && a.packets > 20, "bob hears alice through it");
  await press(alice, "Hang up");
  check(await until(async () => (await overlay(bob)) === "", "call ends"), "the call ends");

  // --- "Ring on this computer" off: bob's window stays quiet; the call is missed there when alice gives up
  await bob.eval(`st.section='settings'; st.setSec='calls-card'; st.screen=null; render();`);
  await until(() => bob.eval(`return !!document.getElementById('calls-ring')`), "bob's ring toggle", 5000);
  check(await bob.eval(`return document.getElementById('calls-ring').checked`), "ringing is on by default");
  await bob.eval(`const b=document.getElementById('calls-ring'); b.checked=false; b.dispatchEvent(new Event('change'));`);
  check((await bob.eval(`return localStorage.getItem('callsRing')`)) === "0", "bob turns ringing off on this computer");
  await bob.eval(`st.section='chats'; render();`);
  await alice.eval(`document.querySelector('[aria-label="Voice call Bob"]').click()`);
  await until(async () => (await overlay(alice)).includes("Calling Bob"), "alice calling");
  await sleep(5000);
  check((await overlay(bob)) === "", "bob's window does not ring");
  await press(alice, "Hang up");
  await bob.eval(`localStorage.setItem('callsRing','1')`);
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  const f = D.failures();
  console.log(f ? `${f} check(s) failed` : "all checks passed");
  process.exit(f ? 1 : 0);
}
