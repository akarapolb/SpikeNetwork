// Settings > Identity: your name (offered to people you write to first, ADR 0038) and changing the
// passphrase (ADR 0003). Two real desktop windows over a real relay.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_identity.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const RELAY = "ws://127.0.0.1:18991";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check, pass } = D;
const NEW_PASS = "a different passphrase for drive";

async function until(fn, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(300); }
  return null;
}
const setVal = (p, sel, v) => p.eval(`const i=document.querySelector(${JSON.stringify(sel)}); i.value=${JSON.stringify(v)}; i.dispatchEvent(new Event('input',{bubbles:true}));`);
const toastText = (p) => p.eval(`return [...document.querySelectorAll('.toast')].map(t=>t.innerText).join(' | ')`);

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18991"], {});
  await sleep(1000);
  const alice = await launch("alice", 9691);
  const bob = await launch("bob", 9692);
  for (const p of [alice, bob]) await p.invoke("sync");

  console.log("-- Settings > Identity shows Name and Passphrase");
  await alice.eval("go('settings'); st.setSec='set-identity'; render();");
  await sleep(400);
  const rows = await alice.eval(`return [...document.querySelectorAll('#set-identity .srow .t b')].map(b=>b.innerText)`);
  check(rows[0] === "Name" && rows.includes("Passphrase"), `the rows are ${JSON.stringify(rows)}`);
  const sub = await alice.eval(`return document.querySelector('#set-identity .srow .t .note').innerText`);
  check(sub === "Shown to people you add. Each contact can rename you.", "the name row says who sees it");
  check(await alice.eval(`return document.querySelector('#profileName').value === ''`), "no name is set at first");

  console.log("-- set a name");
  await alice.eval(`const i=document.querySelector('#profileName'); i.value='  Alice Moreau '; i.dispatchEvent(new Event('change',{bubbles:true}));`);
  await sleep(800);
  check((await alice.invoke("overview")).me.name === "Alice Moreau", "the name is saved, trimmed");
  check(await alice.eval(`return document.querySelector('#profileName').value`) === "Alice Moreau", "the field shows the saved name");
  await alice.eval(`const i=document.querySelector('#profileName'); i.value='x'.repeat(70); i.dispatchEvent(new Event('change',{bubbles:true}));`);
  await sleep(600);
  check((await alice.invoke("overview")).me.name === "Alice Moreau", "a name past the limit is not saved (the field caps it at 64)");
  let bad = null;
  try { await alice.invoke("set_profile_name", { name: "Ali\nce" }); } catch (e) { bad = String(e); }
  check(!!bad, "a name with a line break is refused");
  await alice.eval("render();");
  await alice.shot("identity-name");

  console.log("-- the name is offered to someone Alice writes to first");
  await alice.invoke("add_contact", { name: "Bob", card: (await bob.invoke("overview")).me.card });
  await alice.invoke("send", { name: "Bob", text: "Hello from the site visit" });
  const reqs = await until(async () => { await bob.invoke("sync"); const r = await bob.invoke("message_requests"); return r.length ? r : null; });
  check(reqs && reqs[0].name === "Alice Moreau", `Bob's request carries the offered name (${reqs && reqs[0].name})`);
  await bob.refresh();
  await bob.eval("st.section='chats'; st.screen='requests'; render();");
  await sleep(400);
  check((await bob.text()).includes("They call themselves Alice Moreau"), "the request says what they call themselves");
  await bob.click("Accept", "main");
  await sleep(300);
  check(await bob.eval(`return document.querySelector('main .field input').value`) === "Alice Moreau", "Accept suggests the offered name");
  await setVal(bob, "main .field input", "Alice (site)");
  await bob.shot("request-suggested-name");
  await bob.click("Add contact", "main");
  await sleep(1200);
  const names = (await bob.invoke("overview")).contacts.map((c) => c.name);
  check(names.includes("Alice (site)") && !names.includes("Alice Moreau"), `Bob's own name for her wins (${JSON.stringify(names)})`);

  console.log("-- change the passphrase");
  await alice.eval("go('settings'); st.setSec='set-identity'; render();");
  await sleep(300);
  await alice.click("Change", "#set-identity");
  check(await alice.eval(`return !!document.querySelector('#pwForm') && document.activeElement?.id === 'pwOld'`), "Change opens the form with the current passphrase focused");
  await setVal(alice, "#pwOld", pass);
  await setVal(alice, "#pwNew", NEW_PASS);
  await setVal(alice, "#pwAgain", NEW_PASS + "x");
  await alice.click("Change passphrase");
  await sleep(400);
  check((await toastText(alice)).includes("do not match"), "mismatched new passphrases are caught");
  await setVal(alice, "#pwOld", "not the passphrase");
  await setVal(alice, "#pwAgain", NEW_PASS);
  await alice.shot("passphrase-form");
  await alice.click("Change passphrase");
  await until(async () => (await toastText(alice)).includes("wrong passphrase"), 8000);
  check((await toastText(alice)).includes("wrong passphrase"), "a wrong current passphrase is refused");
  check(await alice.eval(`return !!document.querySelector('#pwForm')`), "the form stays open after a refusal");
  await setVal(alice, "#pwOld", pass);
  await alice.click("Change passphrase");
  await until(async () => alice.eval(`return !document.querySelector('#pwForm')`), 10000);
  check((await toastText(alice)).includes("Passphrase changed"), "the change is confirmed");
  check((await alice.invoke("overview")).contacts.length === 1, "the session stays unlocked and working");

  console.log("-- lock, then only the new passphrase opens it");
  await alice.eval("await lockNow();");
  await sleep(800);
  await setVal(alice, "#pass", pass);
  await alice.click("Unlock");
  await sleep(2500);
  check(await alice.eval(`return !!document.querySelector('#pass')`), "the old passphrase no longer unlocks: still at the unlock screen");
  await setVal(alice, "#pass", NEW_PASS);
  await alice.click("Unlock");
  await until(async () => alice.eval(`return !document.querySelector('#pass')`), 10000);
  await sleep(1000);
  const ov = await alice.invoke("overview");
  check(ov.contacts.length === 1 && ov.contacts[0].name === "Bob" && ov.me.name === "Alice Moreau", "the new passphrase opens everything: contacts and name");
  const msgs = await alice.invoke("conversation", { name: "Bob" });
  check(msgs.some((m) => (m.body || m.text || "").includes("site visit")), "history is still there");
  await bob.invoke("send", { name: "Alice (site)", text: "Got it after your change" });
  const got = await until(async () => { await alice.invoke("sync"); const m = await alice.invoke("conversation", { name: "Bob" }); return m.some((x) => (x.body || x.text || "").includes("after your change")); });
  check(!!got, "messages still arrive after the change");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(`${D.failures() ? `${D.failures()} FAILED` : "all passed"} · screenshots in ${D.shots}`);
  process.exit(D.failures() ? 1 : 0);
}
