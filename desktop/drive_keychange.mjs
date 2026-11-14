// A verified contact's safety number changes: Bob is re-created with a new identity under the same name.
// Alice's window must refuse the card as an update, take it only through "Replace their key", and then warn as
// in the design (the red banner, the list row, the details panel) until she compares the new number again.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_keychange.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const PORT = 18797;
const D = createDrive({ relayUrl: `ws://127.0.0.1:${PORT}`, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;
const size = (p) => p.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`], {});
  await sleep(1000);
  const alice = await launch("alice", 9371);
  const bob = await launch("bob", 9372);
  const bob2 = await launch("bob2", 9373); // Bob after reinstalling: a new identity
  await size(alice);
  const cardOf = async (p) => (await p.invoke("overview")).me.card;
  const bobCard = await cardOf(bob), bob2Card = await cardOf(bob2);

  await alice.invoke("add_contact", { name: "Bob", card: bobCard });
  const fp1 = (await alice.invoke("overview")).contacts.find((c) => c.name === "Bob").fingerprint;
  await alice.invoke("mark_verified", { name: "Bob", fingerprint: fp1 });
  await alice.refresh();
  await alice.eval(`go('chats'); await openChat('Bob');`); await sleep(500);
  let t = await alice.text();
  check(!t.includes("safety number changed") && !t.includes("Safety number changed"), "a verified contact shows no warning");

  // Paste the new card as an update: refused, with the choice to replace the key.
  await alice.eval(`st.contactFor='Bob'; st.contactForm=null; st.screen='contact'; render();`); await sleep(400);
  await alice.eval(`const a=document.querySelector('textarea[aria-label="New identity card"]'); a.value=${JSON.stringify(bob2Card)}; a.dispatchEvent(new Event('input',{bubbles:true}));`);
  await alice.click("Update card"); await sleep(600);
  t = await alice.text();
  check(t.includes("different safety number") && t.includes("Replace their key"), "a different identity is not taken as an update");
  check((await alice.invoke("overview")).contacts.find((c) => c.name === "Bob").verified, "still verified until replaced");
  await alice.shot("keychange-refused");
  await alice.click("Replace their key"); await sleep(900);

  const c = (await alice.invoke("overview")).contacts.find((x) => x.name === "Bob");
  check(!c.verified && !!c.key_changed, "the changed key drops Bob to not verified");
  check(c.fingerprint !== fp1, "the new safety number is stored");
  t = await alice.text();
  check(t.includes("Bob's safety number changed.") && t.includes("someone is in the middle"), "the conversation shows the red banner");
  check(await alice.eval(`return !!document.querySelector('.kc-banner[role=alert]')`), "the banner is an alert");
  check(await alice.eval(`return [...document.querySelectorAll('.list .row, .row')].some(r=>r.innerText.includes('Bob') && r.innerText.includes('Safety number changed'))`), "the chat list row says Safety number changed");
  const side = await alice.eval(`return document.querySelector('aside.side')?.innerText || ''`);
  check(side.includes("Was verified") && /new safety number/i.test(side) && /new identity key/i.test(side), "the details panel says it was verified and the key is new");
  await alice.shot("keychange-banner");

  // Verify again: the verify screen with the new number; matching clears the warning.
  await alice.click("Verify again", ".kc-banner"); await sleep(500);
  t = await alice.text();
  check(t.includes("Verify Bob") && t.replace(/\s/g, "").includes(c.fingerprint.replace(/[-\s]/g, "").slice(0, 5)), "Verify again opens the verify screen with the new number");
  await alice.shot("keychange-verify");
  await alice.click("They match"); await sleep(700);
  const after = (await alice.invoke("overview")).contacts.find((x) => x.name === "Bob");
  check(after.verified && !after.key_changed, "comparing again verifies and clears the warning");
  await alice.eval(`st.screen=null; render();`); await sleep(400);
  check(await alice.eval(`return !document.querySelector('.kc-banner') && ![...document.querySelectorAll('.row')].some(r=>r.innerText.includes('Safety number changed'))`), "the banner and the list warning are gone");
  await alice.shot("keychange-verified");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  const f = D.failures();
  console.log(f ? `${f} check(s) failed` : "all checks passed", `· screenshots in ${D.shots}`);
  process.exit(f ? 1 : 0);
}
