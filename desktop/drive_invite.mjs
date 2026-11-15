// One-time invite links (ADR 0039): three real desktop windows over a real relay. Alice shares a link
// from the empty chat list, Bob opens it and both become contacts, not verified; Carol opens the same
// link afterwards and is refused.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_invite.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const RELAY = "ws://127.0.0.1:18799";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

async function until(fn, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(300); }
  return null;
}
const typeInto = (p, sel, v) => p.eval(`const i=document.querySelector(${JSON.stringify(sel)}); i.value=${JSON.stringify(v)}; i.dispatchEvent(new Event('input',{bubbles:true}));`);
const toastText = (p) => p.eval(`const t=document.getElementById('toast'); return t && !t.hidden ? t.textContent : ''`);
const contact = async (p, name) => (await p.invoke("overview")).contacts.find((c) => c.name === name);
// Open Add a contact on the Invite link tab, paste `link` and open it.
async function openLink(p, link) {
  await p.eval(`st.screen = "add"; st.addTab = "invite"; render();`);
  await sleep(300);
  await typeInto(p, 'textarea[aria-label="Invite link"]', link);
  await p.click("Open invite", ".dlg");
}

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18799"], {});
  await sleep(1000);
  const alice = await launch("alice", 9411);
  const bob = await launch("bob", 9412);
  const carol = await launch("carol", 9413);
  await alice.invoke("set_profile_name", { name: "Alice Moreau" });
  await bob.invoke("set_profile_name", { name: "Bob Tanaka" });
  for (const p of [alice, bob, carol]) await p.invoke("sync");

  console.log("-- sharing a link from the empty chat list");
  await alice.refresh();
  const empty = await alice.text();
  check(empty.includes("Share an invite link"), "the empty chat list offers Share an invite link");
  check(empty.includes("One-time, valid 7 days. Arrives unverified."), "with the note: one-time, valid 7 days, arrives unverified");
  await alice.eval(`[...document.querySelectorAll('.empty a')].find(a=>a.innerText.includes('Share an invite link')).click()`);
  const link = await until(() => alice.eval(`return document.querySelector('[data-testid="invite-link"]')?.innerText || ''`));
  check(/^spikeinvite1:[A-Za-z0-9_-]+@ws:\/\//.test(link || ""), `the Invite link tab shows a one-time link (${(link || "").slice(0, 28)}…)`);
  check(!(link || "").includes((await alice.invoke("overview")).me.card), "the link does not carry Alice's card");
  await alice.shot("invite-link");
  await alice.click("My code", ".dlg");
  const code = await alice.text();
  check(code.includes("Share an invite link") && code.includes("One-time, valid 7 days. Arrives unverified."), "My code offers the invite link with its note too");
  await alice.shot("my-code");
  await alice.eval(`st.screen = null; render();`);

  console.log("-- Bob opens the link");
  await openLink(bob, link);
  await until(async () => (await contact(bob, "Alice Moreau")) !== undefined);
  const a = await contact(bob, "Alice Moreau");
  check(a && a.verified === false, "Bob has Alice as a contact, under the name she offers, not verified");
  check(await until(async () => (await bob.text()).includes("Alice Moreau")), "Bob's window opens the conversation with Alice");
  await bob.shot("opened");

  console.log("-- Alice is told");
  const b = await until(async () => { await alice.invoke("sync"); return contact(alice, "Bob Tanaka"); }, 20000);
  check(b && b.verified === false, "Alice has Bob as a contact, not verified, without pasting anything");
  check((await alice.invoke("message_requests")).length === 0, "Bob is not left as a message request");

  console.log("-- they can talk");
  await bob.invoke("send", { name: "Alice Moreau", text: "Got your link" });
  await alice.invoke("sync");
  const conv = await alice.invoke("conversation", { name: "Bob Tanaka" });
  check(JSON.stringify(conv).includes("Got your link"), "Alice reads Bob's first message");

  console.log("-- a second opening fails");
  await openLink(carol, link);
  const said = await until(() => toastText(carol), 15000);
  check((said || "").includes("already used or has expired"), `Carol is told the link was already used ("${said}")`);
  check((await carol.invoke("overview")).contacts.length === 0, "Carol gets no contact");
  await carol.shot("used");
  let refused = false;
  try { await bob.invoke("redeem_invite", { link, name: null }); } catch (e) { refused = String(e).includes("already used"); }
  check(refused, "Bob cannot open it a second time either");
} catch (e) {
  console.log("FAIL", e.message || e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
