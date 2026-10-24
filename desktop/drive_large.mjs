// Drives three real desktop app windows (alice, bob, carol) through a large group, over the Chrome
// DevTools protocol. Needs Node 22+, Windows (WebView2), and the debug builds of the app and relay:
//   cargo build -p spike-relay            (repository root)
//   cargo build                           (desktop/src-tauri)
//   node desktop/drive_large.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const RELAY = "ws://127.0.0.1:18790";
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { root, start, launch, check } = D;

try {
  start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", "127.0.0.1:18790"], {});
  await sleep(1000);
  const alice = await launch("alice", 9331);
  const bob = await launch("bob", 9332);
  const carol = await launch("carol", 9333);
  const card = async (p) => (await p.invoke("overview")).me.card;
  for (const p of [alice, bob, carol]) await p.invoke("sync"); // the first sync publishes key packages
  for (const [n, p] of [["Bob", bob], ["Carol", carol]]) {
    await alice.invoke("add_contact", { name: n, card: await card(p) });
    await p.invoke("add_contact", { name: "Alice", card: await card(alice) });
  }
  await alice.refresh();

  if (process.env.DEBUG_DRIVE) { console.log(await alice.text()); await alice.shot("0-debug"); }
  // Alice makes a large group from the screen.
  await alice.click("New group");
  await alice.shot("1-new-group");
  await alice.click("Large");
  await alice.eval(`const i=document.querySelector('input[aria-label="Group name"]'); i.value="Org"; i.dispatchEvent(new Event('input',{bubbles:true}));`);
  const boxes = await alice.eval(`return [...document.querySelectorAll('label.pick input')].length`);
  check(boxes === 2, "alice sees her two contacts to pick");
  await alice.eval(`document.querySelectorAll('label.pick input').forEach(i=>{i.checked=true;i.dispatchEvent(new Event('change',{bubbles:true}));})`);
  await alice.shot("2-new-large-group");
  await alice.click("Create group");
  await sleep(2500);
  let t = await alice.text();
  check(t.includes("Org") && t.includes("Large"), "alice's list shows Org tagged Large");
  await alice.shot("3-created");

  // Bob and Carol see an invitation; nothing is joined until they accept.
  for (const p of [bob, carol]) {
    await p.invoke("sync");
    await p.refresh();
    t = await p.text();
    check(t.includes("Invite") && t.includes("Org"), `${p.name} sees an invitation`);
    await p.click("Org");
    await sleep(500);
    t = await p.text();
    check(t.includes("Nothing from the group is shown until you accept") && t.includes("large group"), `${p.name} gets the consent screen`);
    check(!t.includes("Block"), `${p.name}: no Block button for a large group`);
    if (p === bob) await p.shot("4-invite");
    await p.click("Join the group");
    await sleep(2000);
  }
  for (const p of [alice, bob, carol]) await p.invoke("sync");

  // A message from each way round.
  const say = async (p, text) => {
    await p.eval(`const ta=document.querySelector('textarea'); ta.value=${JSON.stringify(text)}; ta.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true,cancelable:true}));`);
    await sleep(1500);
  };
  await alice.refresh(); await alice.click("Org"); await sleep(500);
  await say(alice, "welcome to the org");
  await bob.refresh(); await carol.refresh();
  for (const p of [bob, carol]) { await p.invoke("sync"); await p.refresh(); }
  t = await bob.text();
  check(t.includes("Alice: welcome to the org") || t.includes("welcome to the org"), "bob received alice's message");
  await bob.click("Org"); await sleep(800);
  check((await bob.text()).includes("welcome to the org"), "bob reads it in the thread");
  await bob.shot("5-thread");
  await say(bob, "thanks alice");
  await carol.invoke("sync"); await carol.refresh(); await carol.click("Org"); await sleep(800);
  t = await carol.text();
  check(t.includes("thanks alice") && /~[0-9a-f]/.test(t), "carol reads bob's message under a name she can recognise, not as a contact");

  // Instant delivery: nobody calls sync here. The relay pushes, the listener wakes, the screen has it.
  await sleep(4000); // let the listeners reconnect and subscribe after joining (they back off after each displacement)
  await alice.refresh(); await alice.click("Org"); await sleep(500);
  await say(alice, "push test");
  let seen = false;
  for (let i = 0; i < 40 && !seen; i++) {
    await sleep(500);
    const g = (await carol.invoke("overview")).groups[0];
    seen = !!g && (g.last || "").includes("push test");
  }
  check(seen, "carol has alice's message within a few seconds without syncing (instant delivery)");

  // Members screen: alice is admin; the controls for large groups are there.
  await alice.invoke("sync"); await alice.refresh(); await alice.click("Org"); await sleep(500);
  await alice.click("Members"); await sleep(700);
  t = await alice.text();
  check(t.toLowerCase().includes("members (3)") && t.includes("up to 500"), "alice's members screen counts 3 of up to 500");
  const tl = t.toLowerCase();
  check(tl.includes("make admin") && tl.includes("leave group") && tl.includes("group settings"), "rename, admin, who-can-add and leave are offered");
  check(!t.includes("New members see"), "the history choice is not (not built for large groups)");
  await alice.shot("6-members");

  // Rename from the screen; the others see it after a sync.
  await alice.eval(`const i=document.querySelector('input[aria-label="Group name"]'); i.value="Board"; i.dispatchEvent(new Event('change',{bubbles:true}));`);
  await sleep(2500);
  for (const p of [bob, carol]) { await p.invoke("sync"); }
  check((await bob.invoke("overview")).groups[0].name === "Board", "bob sees the new name");
  await alice.refresh();

  // Alice removes Carol. (confirm() is auto-accepted for the run.)
  await alice.eval("window.confirm = () => true;");
  await alice.eval(`const rows=[...document.querySelectorAll('.member')]; const r=rows.find(x=>x.innerText.includes('Carol')); [...r.querySelectorAll('button')].find(b=>b.innerText==='Remove').click();`);
  await sleep(3000);
  t = await alice.text();
  check(t.toLowerCase().includes("members (2)"), "alice's member list shrinks to 2");
  // The listeners may well have synced already (instant delivery), so look at the result, not at one sync's notes.
  const until = async (fn, tries = 40) => { for (let i = 0; i < tries; i++) { if (await fn()) return true; await sleep(500); } return false; };
  await bob.invoke("sync");
  await carol.invoke("sync");
  check(await until(async () => (await carol.invoke("overview")).groups[0].status === "left"), "carol is told she was removed");
  await carol.refresh();
  t = await carol.text();
  check(t.includes("left"), "carol's list marks the group as left");
  await carol.click("Board"); await sleep(600);
  t = await carol.text();
  check(t.includes("You are not in this group"), "carol cannot write any more");
  await carol.shot("7-removed");
  // Bob and Alice carry on in the new mailbox.
  await bob.refresh(); await bob.click("Board"); await sleep(500);
  await say(bob, "just us now");
  await alice.invoke("sync");
  const aliceMsgs = await alice.invoke("group_conversation", { id: (await alice.invoke("overview")).groups[0].id });
  check(aliceMsgs.some((m) => m.body === "just us now"), "alice reads bob's message in the new mailbox");

  // Bob leaves from the screen; Alice, the admin, removes him on her next sync.
  await bob.eval("window.confirm = () => true;");
  await bob.refresh(); await bob.click("Board"); await sleep(500);
  await bob.click("Members"); await sleep(600);
  await bob.click("Leave group"); await sleep(2500);
  t = await bob.text();
  check((await bob.invoke("overview")).groups[0].status === "left", "bob's list marks the group as left");
  await alice.invoke("sync");
  check(await until(async () => (await alice.invoke("overview")).groups[0].members === 1), "alice sees bob leaving and removes him: one member left");
  await alice.refresh();
} catch (e) {
  console.log("ERROR", e.message);
  D.fail();
} finally {
  await D.stop();
  const failures = D.failures();
  console.log(failures ? `${failures} FAILED` : "all passed", `· screenshots in ${D.shots}`);
  process.exit(failures ? 1 : 0);
}
