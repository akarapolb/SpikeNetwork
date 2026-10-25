// Screen sharing in a video call between two real desktop app windows (ADR 0028): alice shares her screen, bob
// receives a changing picture of it, alice stops and her camera comes back; sharing is not offered in a voice call.
// Same setup as drive_call.mjs:
//   node desktop/drive_share.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

// --auto-select-desktop-capture-source picks the first screen without the picker, a test-harness stand-in for the person's choice.
process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream --disable-gpu --auto-select-desktop-capture-source=Entire";
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
const has = (p, label) => p.eval(`return !![...document.querySelectorAll('#callov button')].find(x=>x.getAttribute('aria-label')===${JSON.stringify(label)})`);
const videoIn = (p) => p.eval(`const s=await cs.pc.getStats(); let r=null; s.forEach(x=>{ if(x.type==='inbound-rtp'&&x.kind==='video') r={frames:x.framesDecoded||0,w:x.frameWidth||0,h:x.frameHeight||0}; }); return r;`);
const sending = (p) => p.eval(`const t=cs.pc.getSenders().find(s=>s.track&&s.track.kind==='video').track; return {label:t.label, surface:(t.getSettings().displaySurface||''), state:t.readyState};`);
const connected = (p) => async () => /\d+:\d\d/.test(await overlay(p)) && !(await overlay(p)).includes("Connecting");

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

  // --- a voice call has no share button
  await alice.click("Bob");
  await alice.eval(`document.querySelector('[aria-label="Voice call Bob"]').click()`);
  check(await until(async () => (await overlay(bob)).includes("Alice is calling"), "bob rings", 30000), "bob rings for a voice call");
  await press(bob, "Answer");
  await until(connected(alice), "voice call connected");
  check(!(await has(alice, "Share screen")), "a voice call offers no screen sharing");
  await press(alice, "Hang up");
  await until(async () => (await overlay(bob)) === "", "closed");
  await sleep(1500);

  // --- a video call, then share
  await alice.eval(`document.querySelector('[aria-label="Video call Bob"]').click()`);
  await until(async () => (await overlay(bob)).includes("Alice is calling"), "bob rings for video", 30000);
  await press(bob, "Answer");
  await until(connected(alice), "alice connected");
  await until(connected(bob), "bob connected");
  await sleep(3000);
  check(await has(alice, "Share screen"), "a video call offers screen sharing");
  const before = await sending(alice);
  console.log("   alice sends:", JSON.stringify(before));
  check(await press(alice, "Share screen"), "alice presses Share screen");
  const shared = await until(async () => (await sending(alice)).surface !== "" || /screen|window|monitor/i.test((await sending(alice)).label), "alice sends her screen", 15000);
  const after = await sending(alice);
  console.log("   alice sends:", JSON.stringify(after));
  check(shared, "the picture alice sends is now her screen");
  check((await overlay(alice)).includes("You are sharing your screen"), "alice is told she is sharing");
  check(await has(alice, "Stop sharing screen"), "the button becomes Stop sharing");
  await sleep(1500);
  const f1 = await videoIn(bob);
  await sleep(3000);
  const f2 = await videoIn(bob);
  console.log("   bob receives:", JSON.stringify(f1), JSON.stringify(f2));
  check(f2 && f2.frames > (f1 ? f1.frames : 0), "bob keeps receiving frames while alice shares");
  await bob.shot("share-bob");
  await alice.shot("share-alice");
  check(await press(alice, "Stop sharing screen"), "alice stops sharing");
  await until(async () => (await sending(alice)).surface === "" && !/screen/i.test((await sending(alice)).label), "camera back", 10000);
  const back = await sending(alice);
  console.log("   alice sends:", JSON.stringify(back));
  check(back.state === "live" && back.surface === "", "her camera is back");
  const g1 = await videoIn(bob);
  await sleep(2500);
  const g2 = await videoIn(bob);
  check(g2 && g1 && g2.frames > g1.frames, "bob receives the camera again");
  check(!(await overlay(alice)).includes("You are sharing your screen"), "the sharing notice is gone");
  await press(alice, "Hang up");
  check(await until(async () => (await overlay(bob)) === "", "bob's call screen closes"), "the call ends for both");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  const f = D.failures();
  console.log(f ? `${f} check(s) failed` : "all checks passed");
  process.exit(f ? 1 : 0);
}
