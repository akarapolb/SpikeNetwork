// A call survives a network change (ADR 0028): the connection is restarted on the same call, through the same
// signalling, with fresh ICE details. Two real desktop windows. A network change is simulated by running the same
// function the "online" / "connection changed" events and a dropped connection call.
// Needs Node 22+, Windows (WebView2), debug builds of the relay and the app:
//   node desktop/drive_restart.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream --disable-gpu";
const RELAY = "ws://127.0.0.1:18794";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

async function until(fn, what, ms = 25000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { if (await fn()) return true; } catch (_) {} await sleep(300); }
  check(false, `timed out: ${what}`);
  return false;
}
const overlay = (p) => p.eval(`const o=document.getElementById('callov'); return o && !o.hidden ? o.innerText : ''`);
const press = (p, label) => p.eval(`const b=[...document.querySelectorAll('#callov button')].find(x=>x.getAttribute('aria-label')===${JSON.stringify(label)}); if(!b) return false; b.click(); return true;`);
const ufrag = (p) => p.eval(`return (/a=ice-ufrag:(\\S+)/.exec(cs.pc.localDescription.sdp)||[])[1]`);
const packets = (p) => p.eval(`const s=await cs.pc.getStats(); let n=0; s.forEach(x=>{ if(x.type==='inbound-rtp'&&x.kind==='audio') n=x.packetsReceived; }); return n;`);
const state = (p) => p.eval(`return cs ? cs.pc.connectionState : 'no call'`);

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18794"], {});
  await sleep(1200);
  const alice = await launch("alice", 9371);
  const bob = await launch("bob", 9372);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  await alice.refresh(); await bob.refresh();

  await alice.click("Bob");
  await alice.eval(`document.querySelector('[aria-label="Voice call Bob"]').click()`);
  await until(async () => (await overlay(bob)).includes("Alice is calling"), "bob rings", 30000);
  await press(bob, "Answer");
  const up = (p) => async () => /\d+:\d\d/.test(await overlay(p)) && !(await overlay(p)).includes("Connecting");
  check(await until(up(alice), "alice connected") && await until(up(bob), "bob connected"), "the call connects");
  await sleep(2500);
  let [u0a, u0b, p0a, p0b] = [await ufrag(alice), await ufrag(bob), await packets(alice), await packets(bob)];
  check(p0a > 20 && p0b > 20, `audio flows (${p0a}, ${p0b} packets)`);

  // 1. the caller's network changes: it offers a restart
  await alice.eval(`restartIce(); return 1;`);
  check(await until(async () => (await ufrag(alice)) !== u0a && (await ufrag(bob)) !== u0b, "new ICE details on both sides"), "the caller's restart gives both sides new connection details");
  check(await until(async () => (await state(alice)) === "connected" && (await state(bob)) === "connected", "connected again"), "both sides are connected again");
  await sleep(2500);
  let [p1a, p1b] = [await packets(alice), await packets(bob)];
  check(p1a > p0a + 20 && p1b > p0b + 20, `audio keeps flowing after the restart (${p1a}, ${p1b} packets)`);
  check(/\d+:\d\d/.test(await overlay(alice)) && /\d+:\d\d/.test(await overlay(bob)), "both call screens are still in the call");

  // 2. the callee's network changes: it asks the caller, who restarts
  const [u1a, u1b] = [await ufrag(alice), await ufrag(bob)];
  await bob.eval(`restartIce(); return 1;`);
  check(await until(async () => (await ufrag(alice)) !== u1a && (await ufrag(bob)) !== u1b, "new ICE details after the callee's change"), "the callee's change makes the caller restart");
  check(await until(async () => (await state(alice)) === "connected" && (await state(bob)) === "connected", "connected again 2"), "connected again");
  await sleep(2500);
  const [p2a, p2b] = [await packets(alice), await packets(bob)];
  check(p2a > p1a + 20 && p2b > p1b + 20, `audio keeps flowing (${p2a}, ${p2b} packets)`);

  // 3. hanging up still works after restarts
  await press(alice, "Hang up");
  check(await until(async () => (await overlay(bob)) === "", "bob's call ends"), "hanging up still ends the call for both");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  const f = D.failures();
  console.log(f ? `${f} check(s) failed` : "all checks passed");
  process.exit(f ? 1 : 0);
}
