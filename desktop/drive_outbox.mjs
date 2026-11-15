// ADR 0040: a message sent while the relay is down waits in the outbox, shows "Waiting to send" in amber, and goes
// by itself once the relay is back. Two real desktop windows over a real relay that is stopped and started again
// (with its database, so Bob's mailbox and prekeys survive).
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_outbox.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const PORT = 18850;
const RELAY = `ws://127.0.0.1:${PORT}`;
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

async function until(fn, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(400); }
  return null;
}

const db = join(D.tmp, "relay.db");
const relay = () => start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`, "--db", db], {});
const TEXT = "written while the relay was down";

try {
  let r = relay();
  await sleep(1000);
  const alice = await launch("alice", 9495);
  const bob = await launch("bob", 9496);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  const first = await alice.invoke("send", { name: "Bob", text: "hello first" });
  check(first.delivered === 1 && !first.waiting, `a send with the relay up is delivered (${JSON.stringify(first)})`);
  check(!!(await until(async () => { await bob.invoke("sync"); return (await bob.invoke("conversation", { name: "Alice" })).length === 1; })), "Bob has the first message");

  console.log("-- the relay stops");
  r.kill();
  await sleep(1500);
  await alice.refresh();
  await alice.click("Bob");
  await sleep(500);
  await alice.eval(`const t=document.querySelector('main textarea'); t.value=${JSON.stringify(TEXT)}; t.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));`);
  const said = await until(async () => { const t = await alice.eval(`const t=document.getElementById('toast'); return t && !t.hidden ? t.textContent : ''`); return t || null; }, 30000);
  check(!!said && said.includes("Waiting to send") && said.includes("relay can't be reached"), `the notice says the relay could not be reached (${JSON.stringify(said)})`);
  check(!(said || "").includes("when they connect") && !(said || "").includes("when their device connects"), "it does not promise delivery when they connect");
  const mark = await until(() => alice.eval(`const m=[...document.querySelectorAll('.msg.out')].find(x=>x.innerText.includes(${JSON.stringify(TEXT)})); const s=m&&m.querySelector('.sendstate'); return s ? {text:s.innerText, color:getComputedStyle(s).color, warn:getComputedStyle(document.documentElement).getPropertyValue('--warn-ink').trim()} : null`));
  check(!!mark && mark.text === "Waiting to send", `the message shows "Waiting to send" (${JSON.stringify(mark)})`);
  const rgb = (hex) => { const n = parseInt(hex.replace("#", ""), 16); return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`; };
  check(!!mark && mark.color === rgb(mark.warn), `in amber (--warn-ink ${mark && mark.warn}, got ${mark && mark.color})`);
  const conv = await alice.invoke("conversation", { name: "Bob" });
  const waitingMsg = conv.find((m) => m.body === TEXT);
  check(!!waitingMsg && waitingMsg.waiting === true && waitingMsg.not_sent === false, "the history DTO marks it waiting");
  check(conv.filter((m) => m.body !== TEXT).every((m) => !m.waiting), "the delivered message is not marked");
  check((await alice.invoke("overview")).contacts.find((c) => c.name === "Bob").unsent === 1, "the overview counts one unsent message for Bob");
  const row = await alice.eval(`return [...document.querySelectorAll('.row')].map(x=>x.innerText).find(t=>t.includes('Bob')) || ''`);
  check(row.includes("Waiting to send"), `the chat list says so too (${JSON.stringify(row)})`);
  await alice.shot("outbox-waiting");

  console.log("-- the relay comes back");
  r = relay();
  // Nobody touches anything: Alice's listener reconnects, syncs and sends what waits.
  const gone = await until(() => alice.eval(`const m=[...document.querySelectorAll('.msg.out')].find(x=>x.innerText.includes(${JSON.stringify(TEXT)})); return m && !m.querySelector('.sendstate')`), 60000);
  check(!!gone, "the mark goes by itself once the relay is back");
  check((await alice.invoke("overview")).contacts.find((c) => c.name === "Bob").unsent === 0, "nothing is unsent any more");
  const got = await until(async () => { await bob.invoke("sync"); const c = await bob.invoke("conversation", { name: "Alice" }); return c.some((m) => m.body === TEXT) ? c : null; }, 30000);
  check(!!got, "Bob receives it");
  await bob.invoke("sync");
  const again = await bob.invoke("conversation", { name: "Alice" });
  check(again.filter((m) => m.body === TEXT).length === 1 && again.length === 2, `exactly once (${again.length} messages)`);
  await alice.shot("outbox-sent");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(`${D.failures() ? `${D.failures()} FAILED` : "all passed"} · screenshots in ${D.shots}`);
  process.exit(D.failures() ? 1 : 0);
}
