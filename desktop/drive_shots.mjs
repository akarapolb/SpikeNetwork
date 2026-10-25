// Screenshots of the main desktop screens, light and dark, for comparing the look before and after a change.
// Not a test: it only fills two windows with a little content and captures them.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_shots.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

process.env.DRIVE_WV_ARGS = "--use-fake-device-for-media-stream --use-fake-ui-for-media-stream";
const RELAY = "ws://127.0.0.1:18795";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch } = D;

async function both(p, label) {
  for (const theme of ["light", "dark"]) {
    await p.eval(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
    await sleep(200);
    await p.shot(`${label}-${theme}`);
  }
}

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18795"], {});
  await sleep(1000);
  const alice = await launch("alice", 9351);
  await alice.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });
  const bob = await launch("bob", 9352);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob Tanaka", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  await alice.invoke("add_contact", { name: "Carol Diaz", card: await card(bob) }).catch(() => {});
  for (const t of ["Hi Bob! Are we still on for Thursday?", "The draft is in the shared folder."]) await alice.invoke("send", { name: "Bob Tanaka", text: t });
  await bob.invoke("sync");
  for (const t of ["Yes, Thursday works.", "I'll read the draft tonight and send notes. Longer messages wrap onto several lines so this one shows how a bubble grows."]) await bob.invoke("send", { name: "Alice", text: t });
  await alice.invoke("group_create", { name: "Design team", members: ["Bob Tanaka"], anyCanAdd: false, historyWeek: false }).catch((e) => console.log("group:", e.message));
  await alice.invoke("sync");
  await alice.refresh();
  await sleep(500);

  await both(alice, "1-chats-empty-main");
  await alice.click("Bob Tanaka", "#list");
  await sleep(800);
  await both(alice, "2-conversation");
  await alice.click("Design team", "#list");
  await sleep(800);
  await both(alice, "3-group");
  await alice.eval(`go('settings')`); await sleep(500);
  await both(alice, "4-settings");
  await alice.eval(`go('devices')`); await sleep(500);
  await both(alice, "5-devices");
  await alice.eval(`go('agents')`); await sleep(500);
  await both(alice, "6-agents");
  await alice.eval(`go('chats')`); await sleep(300);
  await alice.click("Add a contact"); await sleep(500);
  await both(alice, "7-add-contact");
  await alice.click("Bob Tanaka", "#list"); await sleep(500);
  await alice.click("Verify"); await sleep(500);
  await both(alice, "8-verify");
  await alice.click("Bob Tanaka", "#list"); await sleep(500);
  await alice.eval(`document.querySelector('[aria-label="Video call Bob Tanaka"]').click()`);
  await sleep(2500);
  await bob.shot("9-ringing");
  await bob.eval(`const b=[...document.querySelectorAll('#callov button')].find(x=>x.getAttribute('aria-label')==='Answer'); b && b.click();`);
  await sleep(4000);
  await alice.shot("10-in-call");
  await alice.eval(`const b=[...document.querySelectorAll('#callov button')].find(x=>x.getAttribute('aria-label')==='Hang up'); b && b.click();`);
  await sleep(800);
  await alice.invoke("lock");
  await alice.reload();
  await both(alice, "11-unlock");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(`${D.failures() ? "FAILED" : "done"} · screenshots in ${D.shots}`);
  process.exit(D.failures() ? 1 : 0);
}
