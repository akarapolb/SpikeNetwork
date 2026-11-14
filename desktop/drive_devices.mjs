// A contact's devices in the conversation's details panel, each with when we last heard from it:
// two real desktop windows over a real relay.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_devices.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const RELAY = "ws://127.0.0.1:18812";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

async function until(fn, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(300); }
  return null;
}
const rows = (p) => p.eval(`return [...document.querySelectorAll('.side .sdv[data-device]')].map(r => r.innerText.replace(/\\n/g, ' | '))`);

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18812"], {});
  await sleep(1000);
  const alice = await launch("alice", 9431);
  const bob = await launch("bob", 9432);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });

  console.log("-- before Bob has written");
  const bobId = (await bob.invoke("overview")).me.device_list[0].id;
  let c = (await alice.invoke("overview")).contacts.find((x) => x.name === "Bob");
  check(Array.isArray(c.device_list) && c.device_list.length === c.devices, `the contact carries a device list (${JSON.stringify(c.device_list)})`);
  check(c.device_list[0].id === bobId && c.device_list[0].last_heard === null, "Bob's device is listed by its id, not heard from yet");
  await alice.invoke("send", { name: "Bob", text: "hi Bob" });
  await alice.refresh();
  await alice.click("Bob");
  await sleep(500);
  await alice.eval(`try { localStorage.setItem('sidePanel', '1'); } catch (_) {} render();`);
  await sleep(300);
  let r = await rows(alice);
  check(r.length === 1 && r[0].includes(`Device ${bobId.slice(0, 6)}`) && r[0].includes("Not heard from yet"), `the panel lists Bob's device, not heard from yet (${JSON.stringify(r)})`);
  check((await alice.eval(`return document.querySelector('.side')?.innerText || ''`)).toUpperCase().includes("DEVICES · 1"), "the Devices caption gives the count");
  check(!(await alice.text()).includes("on their signed list"), "the old count-only line is gone");
  // Sending does not count: only a message *from* the device does.
  check((await alice.invoke("overview")).contacts.find((x) => x.name === "Bob").device_list[0].last_heard === null, "sending to Bob does not mark his device as heard from");

  console.log("-- after Bob writes");
  await bob.invoke("sync");
  const before = Date.now();
  await bob.invoke("send", { name: "Alice", text: "hi Alice" });
  await until(async () => { await alice.invoke("sync"); return (await alice.invoke("overview")).contacts.find((x) => x.name === "Bob").device_list[0].last_heard; });
  c = (await alice.invoke("overview")).contacts.find((x) => x.name === "Bob");
  const heard = c.device_list[0].last_heard;
  check(typeof heard === "number" && heard >= before - 5000 && heard <= Date.now() + 1000, `the arrival time is recorded by Alice's own clock (${heard})`);
  await alice.refresh();
  r = await until(async () => { await alice.refresh(); const x = await rows(alice); return x.length && x[0].includes("active now") ? x : null; });
  check(!!r, `the panel says Bob's device is active now (${JSON.stringify(r)})`);
  check(await alice.eval(`return heardLabel(Date.now() - 3 * 3600e3) === '3 h ago' && heardLabel(Date.now() - 10 * 60e3) === '10 min ago' && heardLabel(null) === 'Not heard from yet'`), "older arrivals read as minutes and hours ago");
  check(typeof (await bob.invoke("overview")).contacts.find((x) => x.name === "Alice").device_list[0].last_heard === "number", "Bob, who received Alice's message, has heard from her device too");
  await alice.shot("contact-devices");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(`${D.failures() ? `${D.failures()} FAILED` : "all passed"} · screenshots in ${D.shots}`);
  process.exit(D.failures() ? 1 : 0);
}
