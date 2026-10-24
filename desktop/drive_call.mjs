// Drives two real desktop app windows (alice, bob) through a voice call over the Chrome DevTools
// protocol, with fake microphones, and proves audio packets flow both ways (ADR 0028).
// Needs Node 22+, Windows (WebView2), and the debug builds of the app and relay:
//   cargo build -p spike-relay            (repository root)
//   cargo build                           (desktop/src-tauri)   [SPIKE_DESKTOP_EXE=... for another build]
//   node desktop/drive_call.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream";
const RELAY = "ws://127.0.0.1:18791";
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
// Audio packets received and the energy in them, from the browser's own statistics.
const audioIn = (p) => p.eval(`const s=await cs.pc.getStats(); let r=null; s.forEach(x=>{ if(x.type==='inbound-rtp'&&x.kind==='audio') r={packets:x.packetsReceived,energy:x.totalAudioEnergy||0}; }); return r;`);

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18791"], {});
  await sleep(1000);
  const alice = await launch("alice", 9341);
  const bob = await launch("bob", 9342);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  await alice.refresh(); await bob.refresh();

  // --- a voice call, answered
  await alice.click("Bob");
  await alice.eval(`document.querySelector('[aria-label="Voice call Bob"]').click()`);
  await until(async () => (await overlay(alice)).includes("Calling Bob"), "alice sees Calling Bob");
  check(await until(async () => (await overlay(bob)).includes("Alice is calling"), "bob rings", 30000), "bob's window rings with the caller's name");
  await bob.shot("1-ringing");
  check(await press(bob, "Answer"), "bob answers");
  const connected = (p) => async () => /\d+:\d\d/.test(await overlay(p)) && !(await overlay(p)).includes("Connecting");
  check(await until(connected(alice), "alice connected"), "alice's call connects");
  check(await until(connected(bob), "bob connected"), "bob's call connects");
  await sleep(3000);
  const [a, b] = [await audioIn(alice), await audioIn(bob)];
  console.log("audio in:", JSON.stringify({ alice: a, bob: b }));
  check(a && a.packets > 20 && a.energy > 0, "alice receives audio from bob");
  check(b && b.packets > 20 && b.energy > 0, "bob receives audio from alice");
  const [ta, tb] = [await overlay(alice), await overlay(bob)];
  const code = (t) => (/Call code (\d{4})/.exec(t) || [])[1];
  check(code(ta) && code(ta) === code(tb), `both show the same call code (${code(ta)})`);
  check(ta.includes("Direct, on this network"), "the screen says the call is direct");
  check(ta.includes("not verified"), "an unverified contact is flagged");
  await alice.shot("2-in-call");
  await bob.shot("2-in-call");

  await press(alice, "Hang up");
  check(await until(async () => (await overlay(bob)) === "", "bob's call screen closes"), "hanging up ends it for the other side");
  check((await overlay(alice)) === "", "and for the caller");

  // --- a declined call
  await alice.eval(`document.querySelector('[aria-label="Voice call Bob"]').click()`);
  await until(async () => (await overlay(bob)).includes("Alice is calling"), "bob rings again", 30000);
  check(await press(bob, "Decline"), "bob declines");
  check(await until(async () => (await overlay(alice)) === "", "alice's call screen closes"), "the caller's screen closes when declined");
  const toast = await alice.eval(`return document.getElementById('toast').textContent`);
  check(toast.includes("declined"), `the caller is told ("${toast}")`);

  // --- the caller gives up while it rings
  await alice.eval(`document.querySelector('[aria-label="Voice call Bob"]').click()`);
  await until(async () => (await overlay(bob)).includes("Alice is calling"), "bob rings a third time", 30000);
  await press(alice, "Hang up");
  check(await until(async () => (await overlay(bob)) === "", "bob stops ringing"), "ringing stops when the caller hangs up");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  const f = D.failures();
  console.log(f ? `${f} check(s) failed` : "all checks passed");
  process.exit(f ? 1 : 0);
}
