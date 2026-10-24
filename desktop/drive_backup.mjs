// Drives real desktop app windows through losing the only device and restoring from a backup file
// (ADR 0029), over the Chrome DevTools protocol. Needs Node 22+, Windows (WebView2), and the debug
// builds of the app and relay:
//   cargo build -p spike-relay            (repository root)
//   cargo build                           (desktop/src-tauri)
//   node desktop/drive_backup.mjs [screenshot-dir]
import { join } from "node:path";
import { mkdirSync, readdirSync } from "node:fs";
import { createDrive, sleep } from "./drive_lib.mjs";

const RELAY = "ws://127.0.0.1:18791";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, check, tmp, Page } = D;
const KEY_RE = /^[0-9A-Z]{5}(-[0-9A-Z]{5}){4}$/;
const NEW_PASS = "a brand new device passphrase";

// Like launch() in drive_lib, but the window is left on its create screen when `create` is false.
async function open(name, port, { create, downloads }) {
  const exe = process.env.SPIKE_DESKTOP_EXE || join(root, "desktop", "src-tauri", "target", "debug", "spike-desktop.exe");
  const proc = start(exe, [], {
    SPIKE_DESKTOP_HOME: join(tmp, name),
    SPIKE_DESKTOP_DOWNLOADS: downloads,
    WEBVIEW2_USER_DATA_FOLDER: join(tmp, `${name}-wv`),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  });
  const p = new Page(name, port);
  p.proc = proc;
  await p.connect();
  await sleep(1500);
  if (create) {
    await p.invoke("create_identity", { passphrase: D.pass });
    await p.invoke("set_relay", { url: RELAY });
    await p.reload();
    await p.eval(`const i=document.querySelector('input'); i.value=${JSON.stringify(D.pass)}; i.dispatchEvent(new Event('input',{bubbles:true}));`);
    await p.click("Unlock");
    await sleep(2500);
  }
  return p;
}
const type = (p, sel, v) => p.eval(`const i=document.querySelector(${JSON.stringify(sel)}); i.value=${JSON.stringify(v)}; i.dispatchEvent(new Event('input',{bubbles:true})); i.dispatchEvent(new Event('change',{bubbles:true}));`);

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18791"], {});
  await sleep(1000);
  const downloads = join(tmp, "downloads");
  mkdirSync(downloads, { recursive: true });
  const alice = await open("alice", 9341, { create: true, downloads });
  const bob = await open("bob", 9342, { create: true, downloads });
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  await alice.invoke("send", { name: "Bob", text: "before the loss" });
  await bob.invoke("sync");
  const fingerprint = (await alice.invoke("overview")).me.fingerprint;

  // 1. Alice makes a backup from Settings, as a person would.
  await alice.eval("st.section='settings'; await refresh(); render();");
  await sleep(600);
  let t = await alice.text();
  check(t.toLowerCase().includes("backup and recovery") && t.includes("No backup yet") && t.includes("copy of your identity"), "settings shows the backup card, the warning and 'no backup yet'");
  await alice.click("Make a backup key");
  await sleep(300);
  const key = await alice.eval("return document.querySelector('#bkKey')?.innerText || ''");
  check(KEY_RE.test(key), `a backup key in the generated shape is shown (${key})`);
  await alice.click("Save backup file");
  await sleep(800);
  t = await alice.text();
  check(!readdirSync(downloads).length, "saving without ticking 'I wrote the key down' is refused");
  await alice.eval("document.querySelector('#bkAck').click(); document.querySelector('#bkHist').click();");
  await alice.click("Save backup file");
  await sleep(2500);
  const files = readdirSync(downloads);
  check(files.length === 1 && files[0].endsWith(".spikebackup"), `the backup file is in Downloads (${files})`);
  t = await alice.text();
  check(t.includes("Saved.") && t.includes("Last backup:") && !t.includes(key), "settings says saved, shows the date, and no longer shows the key");
  await alice.shot("1-backup-saved");
  const file = join(downloads, files[0]);

  // 2. The computer is lost: the window closes and its state is gone. A new install sees the create screen.
  alice.proc.kill();
  await sleep(1500);
  const alice2 = await open("alice-new", 9343, { create: false, downloads });
  await alice2.reload();
  t = await alice2.text();
  check(t.includes("Create my identity") && t.includes("Restore from a backup file"), "a fresh install offers 'Restore from a backup file'");
  await alice2.click("Restore from a backup file");
  await sleep(400);
  // Wrong passphrase first, then the right key typed in lower case without dashes.
  await alice2.send("DOM.enable");
  const doc = await alice2.send("DOM.getDocument");
  const node = await alice2.send("DOM.querySelector", { nodeId: doc.result.root.nodeId, selector: "#rsFile" });
  await alice2.send("DOM.setFileInputFiles", { files: [file], nodeId: node.result.nodeId });
  await type(alice2, "#rsPass", "not the right passphrase");
  await alice2.click("Open backup");
  await sleep(2500);
  t = await alice2.text();
  check(t.includes("wrong backup passphrase"), "a wrong backup passphrase is refused with a plain message");
  await type(alice2, "#rsPass", key.toLowerCase().replaceAll("-", ""));
  await alice2.click("Open backup");
  await sleep(2500);
  t = await alice2.text();
  check(t.includes("1 contact, 0 groups, 1 message") && t.includes("signs out every other device"), "the summary shows what the backup holds, with the sign-out warning");
  await alice2.shot("2-restore-confirm");
  await type(alice2, "#rsDev1", NEW_PASS);
  await type(alice2, "#rsDev2", NEW_PASS);
  await type(alice2, "#rsRelay", RELAY);
  await alice2.click("Restore my identity");
  await sleep(5000);
  t = await alice2.text();
  check(t.includes("Bob"), "the restored app opens with Bob in the chat list");
  await alice2.shot("3-restored");
  const me = (await alice2.invoke("overview")).me;
  check(me.fingerprint === fingerprint && me.devices === 1, "same identity (fingerprint), exactly one device");

  // 3. Bob and the restored Alice talk, with no key-change alarm.
  await bob.invoke("send", { name: "Alice", text: "are you back?" });
  await alice2.invoke("sync");
  await alice2.refresh();
  await alice2.click("Bob");
  await sleep(600);
  t = await alice2.text();
  check(t.includes("before the loss") && t.includes("are you back?"), "restored history and Bob's new message are in the thread");
  await alice2.invoke("send", { name: "Bob", text: "I am back" });
  await bob.invoke("sync");
  await bob.refresh();
  await bob.click("Alice");
  await sleep(600);
  t = await bob.text();
  // The listener may have fetched it before the explicit sync does, so the count is not checked.
  check(t.includes("I am back") && !/key.?change/i.test(t), "Bob receives from the restored Alice, no key-change warning");
  await bob.shot("4-bob-sees-alice");
  console.log(D.failures() ? `${D.failures()} FAILED` : "all passed");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  process.exit(D.failures() ? 1 : 0);
}
