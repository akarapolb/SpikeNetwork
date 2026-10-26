// Notification banners: two real desktop windows. Alice's window is treated as not focused; Bob writes and calls.
// Checks what each banner says for each setting: who it is from (the name), or only that something arrived,
// never the message text, and nothing when banners are off or the window is in use.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_banner.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream";
const RELAY = "ws://127.0.0.1:18796";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

async function until(fn, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(300); }
  return null;
}

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18796"], {});
  await sleep(1000);
  const alice = await launch("alice", 9381);
  const bob = await launch("bob", 9382);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  await alice.refresh(); await bob.refresh();

  // Record every banner the window asks for (and still show it); "focused" is ours to set.
  await alice.eval(`window.__banners = []; window.__focused = false; document.hasFocus = () => window.__focused;
    const real = window.showBanner; window.showBanner = (body) => { window.__banners.push(body); real(body); };`);
  const banners = () => alice.eval("return window.__banners.slice()");
  const mode = (m) => alice.eval(`localStorage.setItem("banners", ${JSON.stringify(m)}); window.__banners = [];`);

  let shown = "ok";
  try { await alice.invoke("notify_banner", { title: "Spike Connect", body: "Banner test." }); } catch (e) { shown = String(e.message || e); }
  check(shown === "ok", `the system shows a banner when asked (${shown})`);

  await mode("who");
  await bob.invoke("send", { name: "Alice", text: "the secret plan" });
  const b1 = await until(async () => (await banners())[0]);
  check(b1 === "New message from Bob.", `"who it is from": the banner names the sender ("${b1}")`);
  check(!(b1 || "").includes("secret"), "the banner never carries the message text");

  await mode("some");
  await bob.invoke("send", { name: "Alice", text: "and another" });
  const b2 = await until(async () => (await banners())[0]);
  check(b2 === "New message.", `"something arrived": no name ("${b2}")`);

  await mode("off");
  await bob.invoke("send", { name: "Alice", text: "third" });
  await until(async () => (await alice.invoke("overview")).contacts.find((c) => c.name === "Bob").unread >= 3);
  await sleep(1500);
  check((await banners()).length === 0, "banners off: none");

  await mode("who");
  await alice.eval("window.__focused = true;");
  await bob.invoke("send", { name: "Alice", text: "fourth" });
  await until(async () => (await alice.invoke("overview")).contacts.find((c) => c.name === "Bob").unread >= 4);
  await sleep(1500);
  check((await banners()).length === 0, "no banner while the window is in use");

  await alice.eval("window.__focused = false;");
  await bob.click("Alice");
  await bob.eval(`document.querySelector('[aria-label="Voice call Alice"]').click()`);
  const b3 = await until(async () => (await banners()).find((b) => b.includes("calling")));
  check(b3 === "Bob is calling.", `an incoming call shows a banner ("${b3}")`);
  await alice.shot("banner-ringing");
  await bob.eval(`const b=[...document.querySelectorAll('#callov button')].find(x=>x.getAttribute('aria-label')==='Hang up'); b && b.click();`);
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(`${D.failures() ? `${D.failures()} FAILED` : "all passed"} · screenshots in ${D.shots}`);
  process.exit(D.failures() ? 1 : 0);
}
