// Search in a conversation, and replies and reactions: two real desktop windows over a real relay.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_extras.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const RELAY = "ws://127.0.0.1:18798";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

async function until(fn, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(300); }
  return null;
}
const typeInto = (p, sel, v) => p.eval(`const i=document.querySelector(${JSON.stringify(sel)}); i.value=${JSON.stringify(v)}; i.dispatchEvent(new Event('input',{bubbles:true}));`);

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18798"], {});
  await sleep(1000);
  const alice = await launch("alice", 9401);
  const bob = await launch("bob", 9402);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob]) await p.invoke("sync");
  await alice.invoke("add_contact", { name: "Bob", card: await card(bob) });
  await bob.invoke("add_contact", { name: "Alice", card: await card(alice) });
  for (const t of ["Lunch on Friday?", "The report is due Monday", "Pizza or sushi for lunch", "ok"]) await alice.invoke("send", { name: "Bob", text: t });
  await bob.invoke("sync");
  await alice.refresh();
  await alice.click("Bob");
  await sleep(500);

  const noNull = async (p, where) => check(!/(^|\n)\s*null\s*(\n|$)/.test(await p.text()), `no stray "null" on the screen (${where})`);
  await noNull(alice, "an open chat");
  console.log("-- search in a conversation");
  await alice.click("Search in this conversation");
  check(await alice.eval(`return !!document.querySelector('.findbar input')`), "the search bar opens");
  await typeInto(alice, ".findbar input", "lunch");
  await sleep(300);
  const st1 = await alice.eval(`return { n: document.querySelectorAll('.msg.hit').length, count: document.querySelector('.findcount').innerText, cur: document.querySelector('.msg.current')?.innerText || '', marks: document.querySelectorAll('.msg mark').length }`);
  check(st1.n === 2 && st1.count === "2 of 2", `two messages match "lunch" (${st1.n}, "${st1.count}")`);
  check(st1.cur.includes("Pizza"), "the newest match is the current one");
  check(st1.marks === 2, "the matched words are highlighted");
  await alice.shot("search");
  await alice.click("Older match");
  check((await alice.eval(`return document.querySelector('.msg.current')?.innerText || ''`)).includes("Lunch on Friday"), "Older match moves to the earlier message");
  await typeInto(alice, ".findbar input", "nothing like this");
  check((await alice.eval(`return document.querySelector('.findcount').innerText`)) === "No matches", "a search with no matches says so");
  await alice.click("Close search");
  check(await alice.eval(`return !document.querySelector('.findbar') && !document.querySelector('.msg mark')`), "closing the search removes the bar and the highlights");

  console.log("-- replies");
  await bob.invoke("send", { name: "Alice", text: "Sushi, definitely" });
  await until(async () => (await alice.text()).includes("Sushi, definitely"));
  const inMsg = `[...document.querySelectorAll('.msg')].find(x=>x.innerText.includes('Sushi, definitely'))`;
  await alice.eval(`[...${inMsg}.querySelectorAll('button')].find(b=>b.innerText==='Reply').click()`);
  await sleep(300);
  check((await alice.eval(`return document.querySelector('.replystrip')?.innerText || ''`)).includes("Replying to Bob"), "Reply shows who you are replying to above the message box");
  await alice.eval(`const ta=document.querySelector('.composer textarea'); ta.value='Sushi it is'; ta.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true,cancelable:true}));`);
  await sleep(1500);
  check(await alice.eval(`return !document.querySelector('.replystrip')`), "sending clears the reply strip");
  const myQuote = await alice.eval(`return [...document.querySelectorAll('.msg')].find(x=>x.innerText.includes('Sushi it is'))?.querySelector('.quote')?.innerText || ''`);
  check(myQuote.includes("Bob") && myQuote.includes("Sushi, definitely"), `the reply quotes Bob's message ("${myQuote.replace(/\n/g, " / ")}")`);
  await bob.refresh(); await bob.click("Alice"); await sleep(500);
  const theirQuote = await until(async () => { await bob.refresh(); return bob.eval(`return [...document.querySelectorAll('.msg')].find(x=>x.innerText.includes('Sushi it is'))?.querySelector('.quote')?.innerText || ''`); });
  check((theirQuote || "").includes("You"), `Bob sees the quote of his own message ("${(theirQuote || "").replace(/\n/g, " / ")}")`);

  console.log("-- reactions");
  const bobMsg = `[...document.querySelectorAll('.msg')].find(x=>x.innerText.includes('Sushi it is'))`;
  await bob.eval(`[...${bobMsg}.querySelectorAll('button')].find(b=>b.innerText==='React').click()`);
  await sleep(300);
  check(await bob.eval(`return document.querySelectorAll('.picker button').length`) === 6, "React offers six quick reactions");
  await bob.eval(`document.querySelector('.picker button[aria-label="React 👍"]').click()`);
  await sleep(1500);
  check((await bob.eval(`return ${bobMsg}.querySelector('.react.mine')?.innerText || ''`)).includes("👍"), "Bob's own reaction shows as his");
  const aliceChip = await until(async () => { await alice.refresh(); return alice.eval(`return [...document.querySelectorAll('.msg')].find(x=>x.innerText.includes('Sushi it is'))?.querySelector('.react')?.innerText || ''`); });
  check((aliceChip || "").includes("👍"), "Alice sees Bob's reaction on her message");
  await alice.shot("reply-and-reaction");
  // Alice adds the same one: the count goes to 2. Bob takes his back: 1 left, Alice's.
  await alice.eval(`[...document.querySelectorAll('.msg')].find(x=>x.innerText.includes('Sushi it is')).querySelector('.react').click()`);
  await sleep(1500);
  check((await alice.eval(`return [...document.querySelectorAll('.msg')].find(x=>x.innerText.includes('Sushi it is')).querySelector('.react').innerText`)).includes("2"), "the same reaction from two people counts 2");
  await bob.refresh();
  await bob.eval(`${bobMsg}.querySelector('.react.mine').click()`);
  await sleep(1500);
  const after = await until(async () => { await alice.refresh(); const t = await alice.eval(`const c=[...document.querySelectorAll('.msg')].find(x=>x.innerText.includes('Sushi it is')).querySelector('.react'); return c ? c.className + '|' + c.innerText : ''`); return t.includes("mine") && !t.includes("2") ? t : null; });
  check(!!after, `taking a reaction back leaves only Alice's (${after})`);
  await noNull(alice, "after replies and reactions");
  await alice.eval("go('chats'); st.chat = null; render();");
  await noNull(alice, "no chat open");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(`${D.failures() ? `${D.failures()} FAILED` : "all passed"} · screenshots in ${D.shots}`);
  process.exit(D.failures() ? 1 : 0);
}
