// The title-bar command bar: people, chats, messages and actions from one field, run from the keyboard.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_command.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const RELAY = "ws://127.0.0.1:18799";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

const type = (p, v) => p.eval(`const i=document.querySelector('#tbq'); i.focus(); i.value=${JSON.stringify(v)}; i.dispatchEvent(new Event('input',{bubbles:true}));`).then(() => sleep(250));
const key = (p, k, mods = {}) => p.eval(`const i=document.querySelector('#tbq'); (document.activeElement===i?i:document).dispatchEvent(new KeyboardEvent('keydown',{key:${JSON.stringify(k)},bubbles:true,cancelable:true,ctrlKey:${!!mods.ctrl}}));`).then(() => sleep(300));
// What the list shows: each entry's section, text and whether it is the highlighted one.
const items = (p) => p.eval(`const pal=document.querySelector('#cmdpal'); if(!pal) return null; return [...pal.querySelectorAll('[role=group]')].flatMap(g=>[...g.querySelectorAll('[role=option]')].map(o=>({ sec: g.getAttribute('aria-label'), kind: o.dataset.kind, text: o.innerText.replace(/\\n/g,' '), on: o.getAttribute('aria-selected')==='true' })));`);

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18799"], {});
  await sleep(1000);
  const alice = await launch("alice", 9411);
  const bob = await launch("bob", 9412);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob Tanaka", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  for (const t of ["Hi Bob, are we still on?", "The rental car is booked for Thursday", "See you then"]) await alice.invoke("send", { name: "Bob Tanaka", text: t });
  await alice.invoke("group_create", { name: "Design team", members: ["Bob Tanaka"], anyCanAdd: false, historyWeek: false }).catch((e) => console.log("group:", e.message));
  await alice.refresh();

  console.log("-- opening");
  await key(alice, "k", { ctrl: true });
  check(await alice.eval(`return document.activeElement===document.querySelector('#tbq')`), `${"Ctrl K"} puts the cursor in the title-bar field`);
  let it = await items(alice);
  check(!!it && it.some((x) => x.sec === "Recent") && it.some((x) => x.text.includes("Lock now")), "with nothing typed it lists recent chats and actions");
  check(await alice.eval(`return document.querySelector('#tbq').getAttribute('aria-expanded')`) === "true", "the field says its list is open");
  await key(alice, "Escape");
  check(await alice.eval(`return !document.querySelector('#cmdpal') && document.activeElement!==document.querySelector('#tbq')`), "Esc closes the list and leaves the field");

  console.log("-- a person, and the chat filter");
  await type(alice, "bob");
  it = await items(alice);
  check(it[0] && it[0].sec === "People and chats" && it[0].kind === "person" && it[0].text.includes("Bob Tanaka") && it[0].on, `typing "bob" finds the person first, highlighted (${it[0] && it[0].text})`);
  check(it.some((x) => x.sec === "Actions" && x.text.startsWith("Call Bob Tanaka")), "and offers to call them");
  const rows = await alice.eval(`return [...document.querySelectorAll('#list .row')].map(r=>r.innerText)`);
  check(rows.length === 1 && rows[0].includes("Bob Tanaka"), `the chat list is still filtered as you type (${rows.length} row)`);
  await alice.shot("command-person");
  await key(alice, "Enter");
  check(await alice.eval(`return st.chat`) === "Bob Tanaka", "Enter opens the chat");
  check(await alice.eval(`return document.querySelector('#tbq').value==='' && document.querySelectorAll('#list .row').length===2`), "and clears the field and the filter");

  console.log("-- a message");
  await alice.eval("go('settings')"); await sleep(300);
  await type(alice, "rental car");
  it = await items(alice);
  const m = it.find((x) => x.sec === "Messages");
  check(!!m && m.text.includes("Bob Tanaka") && m.text.includes("rental car is booked"), `typing "rental car" finds the message (${m && m.text})`);
  await alice.shot("command-message");
  check(m && m.on, "the message is highlighted, as nothing else matches before it");
  await key(alice, "Enter");
  const f = await alice.eval(`return { chat: st.chat, q: document.querySelector('.findbar input')?.value, cur: document.querySelector('.msg.current')?.innerText || '' }`);
  check(f.chat === "Bob Tanaka" && f.q === "rental car" && f.cur.includes("rental car is booked"), `Enter opens the conversation at that message (${JSON.stringify(f)})`);

  console.log("-- actions, with the arrows");
  await type(alice, "settings");
  it = await items(alice);
  check(it.length > 3 && it.every((x) => x.sec === "Actions"), `"settings" lists the settings sections (${it.length})`);
  await key(alice, "ArrowDown");
  it = await items(alice);
  check(it[1].on && !it[0].on && it[1].text.includes("Home relay"), `ArrowDown moves to the next entry (${it.find((x) => x.on)?.text})`);
  await key(alice, "ArrowUp"); await key(alice, "ArrowUp");
  it = await items(alice);
  check(it[it.length - 1].on, "ArrowUp from the first entry wraps to the last");
  await key(alice, "ArrowDown"); await key(alice, "ArrowDown");
  await key(alice, "Enter");
  const s = await alice.eval(`return { sec: st.section, cur: document.querySelector('.setnav a[aria-current=true]')?.innerText || '' }`);
  check(s.sec === "settings" && s.cur.includes("Home relay"), `Enter runs it: Settings opens on Home relay (${JSON.stringify(s)})`);

  await type(alice, "new gr");
  it = await items(alice);
  check(it[0] && it[0].text.startsWith("New group"), `"new gr" finds New group (${it[0] && it[0].text})`);
  await key(alice, "Enter");
  check(await alice.eval(`return st.screen`) === "newgroup", "Enter starts a new group");

  await type(alice, "link");
  await key(alice, "Enter");
  check(await alice.eval(`return st.section`) === "devices", "Link a device opens Devices");

  await type(alice, "zzqx");
  check((await alice.eval(`return document.querySelector('#cmdpal .pempty')?.innerText || ''`)).includes("Nothing matches"), "no match says so");

  await type(alice, "lock");
  it = await items(alice);
  check(it[0] && it[0].text.includes("Lock now") && it[0].text.includes("Ctrl L"), `"lock" finds Lock now with its shortcut (${it[0] && it[0].text})`);
  await key(alice, "Enter"); await sleep(500);
  check(await alice.eval(`return document.querySelector('#app').hidden && !document.querySelector('#gate').hidden && !document.querySelector('#cmdpal')`), "Enter locks the app");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(`${D.failures() ? `${D.failures()} FAILED` : "all passed"} · screenshots in ${D.shots}`);
  process.exit(D.failures() ? 1 : 0);
}
