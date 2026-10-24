// A call that can only go through a TURN server (ADR 0028, phase 3): two real desktop windows, a relay that
// issues TURN logins, and a TURN server that accepts exactly those logins (relay/examples/turn_test_server.rs, a
// stand-in for coturn). Both windows have "Always use the relay for calls" on, so the browser is told to use
// relay candidates only: if the call connects and audio flows, the packets went through the TURN server.
// Needs Node 22+, Windows (WebView2), and debug builds of the relay, its TURN example and the app:
//   cargo build -p spike-relay --example turn_test_server && cargo build -p spike-relay
//   (cd desktop/src-tauri && cargo build)      [SPIKE_DESKTOP_EXE=... for another build]
//   node desktop/drive_turn.mjs [screenshot-dir]
import { join } from "node:path";
import { networkInterfaces } from "node:os";
import { createDrive, sleep } from "./drive_lib.mjs";

process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream --disable-gpu";
const RELAY = "ws://127.0.0.1:18793";
const TURN_PORT = 34780;
// Chromium's WebRTC does not use a TURN server on 127.0.0.1, so the test server listens on this machine's LAN address.
const LAN = Object.values(networkInterfaces()).flat().find((i) => i && i.family === "IPv4" && !i.internal)?.address || "127.0.0.1";
const SECRET = "turn drive secret";
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
const stats = (p) => p.eval(`const s=await cs.pc.getStats(); let a=null, pair=null, loc=null, rem=null; s.forEach(x=>{ if(x.type==='inbound-rtp'&&x.kind==='audio') a={packets:x.packetsReceived}; if(x.type==='candidate-pair'&&x.state==='succeeded'&&(x.nominated||x.selected)) pair=x; }); if(pair){ loc=s.get(pair.localCandidateId); rem=s.get(pair.remoteCandidateId); } return {audio:a, local:loc&&loc.candidateType, remote:rem&&rem.candidateType};`);

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18793", "--turn-url", `turn:${LAN}:${TURN_PORT}?transport=udp`, "--turn-secret", SECRET], {});
  start(join(root, "target", "debug", "examples", "turn_test_server.exe"), ["--public-ip", LAN, "--secret", SECRET, "--port", String(TURN_PORT)], {});
  await sleep(1500);
  const alice = await launch("alice", 9381);
  const bob = await launch("bob", 9382);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  await alice.refresh(); await bob.refresh();

  const servers = await alice.invoke("call_ice_servers");
  check(servers.length === 1 && servers[0].urls[0].startsWith("turn:" + LAN) && /^\d+:/.test(servers[0].username), "the relay hands the app a TURN login");
  check(!servers[0].username.includes((await alice.invoke("overview")).me.id || "~"), "the login does not carry the whole device id");

  // "Always use the relay for calls", as set on the Settings screen.
  for (const p of [alice, bob]) {
    await p.eval(`st.section='settings'; st.screen=null; render();`);
    await sleep(700);
    check(await until(() => p.eval(`return document.getElementById('calls-card') && document.getElementById('calls-card').innerText.includes('offers a TURN server')`), `${p.name}'s Settings says the relay has TURN`, 8000), `${p.name}'s Settings says the relay offers TURN`);
    await p.eval(`const b=document.getElementById('calls-relay-only'); b.checked=true; b.dispatchEvent(new Event('change'));`);
    check((await p.eval(`return localStorage.getItem('callsRelayOnly')`)) === "1", `${p.name} turned on "Always use the relay for calls"`);
    await p.eval(`st.section='chats'; render();`);
  }
  await alice.click("Bob");
  await alice.eval(`document.querySelector('[aria-label="Voice call Bob"]').click()`);
  check(await until(async () => (await overlay(bob)).includes("Alice is calling"), "bob rings", 30000), "bob rings");
  check(await press(bob, "Answer"), "bob answers");
  const up = (p) => async () => /\d+:\d\d/.test(await overlay(p)) && !(await overlay(p)).includes("Connecting");
  check(await until(up(alice), "alice connected"), "the call connects with relay candidates only");
  check(await until(up(bob), "bob connected"), "bob's side connects");
  await sleep(3500);
  const [sa, sb] = [await stats(alice), await stats(bob)];
  console.log("   alice:", JSON.stringify(sa), "bob:", JSON.stringify(sb));
  check(sa.local === "relay" && sb.local === "relay", "both ends use a relay candidate: the media goes through the TURN server");
  check(sa.audio && sa.audio.packets > 20 && sb.audio && sb.audio.packets > 20, "audio flows both ways through it");
  check((await overlay(alice)).includes("Through a relay") && (await overlay(bob)).includes("Through a relay"), "both call screens say 'Through a relay'");
  await alice.shot("turn-call");
  await press(alice, "Hang up");
  check(await until(async () => (await overlay(bob)) === "", "bob's call ends"), "hanging up still ends it");

  // Without "Always use the relay" the same call connects directly (the TURN server is only a fallback).
  for (const p of [alice, bob]) await p.eval(`localStorage.setItem('callsRelayOnly','0')`);
  await alice.eval(`document.querySelector('[aria-label="Voice call Bob"]').click()`);
  await until(async () => (await overlay(bob)).includes("Alice is calling"), "bob rings again", 30000);
  await press(bob, "Answer");
  await until(up(alice), "alice connected directly");
  await sleep(2500);
  const direct = await stats(alice);
  console.log("   direct:", JSON.stringify(direct));
  check(direct.local !== "relay", `with the setting off the call connects directly (${direct.local})`);
  await press(alice, "Hang up");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  const f = D.failures();
  console.log(f ? `${f} check(s) failed` : "all checks passed");
  process.exit(f ? 1 : 0);
}
