// One-time invite links (ADR 0039) on the phone, with desktop windows on the other side:
//  1. the phone shares a link from its empty chat list; Bob (desktop) opens it and both become contacts, not
//     verified; Carol (desktop) opens the same link afterwards and is refused;
//  2. Dana (desktop) shares a link; the phone opens it from a tap in another app (an ACTION_VIEW of the
//     spikeinvite1: link), and a second opening fails.
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   ANDROID_SERIAL=emulator-5554 node android/drive_invite.mjs [screenshot-dir]
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const RELAY_PORT = 8799;
const RELAY = `ws://127.0.0.1:${RELAY_PORT}`;
const NOTE = "One-time, valid 7 days. Arrives unverified.";

const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tapText, shot, back, debug } = createPhone(D);
const phoneContact = (name) => debug("overview").contacts.find((c) => c.name === name);

async function until(fn, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(500); }
  return null;
}
const typeInto = (p, sel, v) => p.eval(`const i=document.querySelector(${JSON.stringify(sel)}); i.value=${JSON.stringify(v)}; i.dispatchEvent(new Event('input',{bubbles:true}));`);
const toastText = (p) => p.eval(`const t=document.getElementById('toast'); return t && !t.hidden ? t.textContent : ''`);
async function openOnDesktop(p, link) {
  await p.eval(`st.screen = "add"; st.addTab = "invite"; render();`);
  await sleep(300);
  await typeInto(p, 'textarea[aria-label="Invite link"]', link);
  await p.click("Open invite", ".dlg");
}

try {
  console.log("-- setting up: a relay of our own, three desktop windows, clean install on the emulator");
  D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${RELAY_PORT}`], {});
  await sleep(1000);
  const bob = await D.launch("bob", 9471);
  const carol = await D.launch("carol", 9472);
  const dana = await D.launch("dana", 9473);
  await bob.invoke("set_profile_name", { name: "Bob Tanaka" });
  await dana.invoke("set_profile_name", { name: "Dana Kim" });
  for (const p of [bob, carol, dana]) await p.invoke("sync");
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", `tcp:${RELAY_PORT}`, `tcp:${RELAY_PORT}`);
  sh("svc power stayon true; settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);
  await sleep(6000);
  // As in drive_keychange: create and unlock through the debug door, keeping the app in front while it works.
  const front = () => { if (!sh("dumpsys activity activities").match(/topResumedActivity=.*network\.spike\.connect/)) sh(`am start -n ${PKG}/.MainActivity`); };
  const settle = async (cmd, done, keepFront) => {
    if (keepFront) { front(); await sleep(2000); }
    try { debug(cmd, { passphrase: PHONE_PASS }); } catch (e) { console.log(`   (${e.message}; waiting)`); }
    for (let i = 0; i < 90; i++) { if (keepFront) front(); try { if (done(debug("app_status"))) return; } catch (_) {} await sleep(1000); }
    throw new Error(`phone ${cmd} never finished`);
  };
  await settle("create_identity", (s) => s.exists, true);
  sh(`am force-stop ${PKG}`);
  await settle("unlock", (s) => s.unlocked, false);
  debug("set_relay", { url: RELAY });
  debug("set_profile_name", { name: "Pat Phone" });
  sh(`am start -n ${PKG}/.MainActivity`);
  await sleep(1500);
  for (let i = 0; i < 3 && screenText().includes("isn't responding"); i++) { try { await tapText("Wait", true); } catch (_) {} await sleep(800); }
  check(await waitText("Nobody here yet", 60000), "the phone shows its empty chat list");

  console.log("-- 1 · the phone shares a link from the empty chat list");
  check(await waitText("Share an invite link", 30000), "the empty chat list offers Share an invite link");
  check(await waitText(NOTE, 10000), "with the note: one-time, valid 7 days, arrives unverified");
  shot("invite-first-day");
  await tapText("Share an invite link", true);
  // The share sheet opens over Add a contact: close it and read the link the screen keeps.
  await until(() => !screenText().includes("Nobody here yet") && !screenText().includes("Making"), 20000);
  await sleep(2500);
  shot("invite-share-sheet");
  if (!screenText().includes("Copy link")) back();
  const link = await until(() => nodes().map((n) => n.text).find((t) => t && t.startsWith("spikeinvite1:")), 20000);
  check(/^spikeinvite1:[A-Za-z0-9_-]+@ws:\/\//.test(link || ""), `Add a contact, My code shows the link (${(link || "").slice(0, 28)}…)`);
  check(screenText().includes(NOTE), "My code shows the note under the link");
  shot("invite-my-code");

  await openOnDesktop(bob, link);
  const pat = await until(async () => (await bob.invoke("overview")).contacts.find((c) => c.name === "Pat Phone"));
  check(pat && pat.verified === false, "Bob has the phone as a contact, under the name it offers, not verified");
  const b = await until(() => phoneContact("Bob Tanaka"), 30000);
  check(b && b.verified === false, "the phone has Bob as a contact, not verified, without pasting anything");
  back();
  check(await waitText("Bob Tanaka", 20000), "Bob is in the phone's chat list");
  shot("invite-joined");
  await bob.invoke("send", { name: "Pat Phone", text: "Hello from the link" });
  check(await until(() => JSON.stringify(debug("conversation", { name: "Bob Tanaka" })).includes("Hello from the link"), 30000), "the phone gets Bob's message");

  await openOnDesktop(carol, link);
  const said = await until(() => toastText(carol), 15000);
  check((said || "").includes("already used or has expired"), `Carol is refused: the link was already used ("${said}")`);
  check((await carol.invoke("overview")).contacts.length === 0, "Carol gets no contact");

  console.log("-- 2 · the phone opens Dana's link from another app");
  const danaLink = await dana.invoke("create_invite");
  sh(`am start -a android.intent.action.VIEW -d '${danaLink}' ${PKG}`);
  check(await waitText("Add a contact", 20000), "the link opens Add a contact");
  check(await until(() => screenText().includes("spikeinvite1:"), 10000), "with the link filled in on the Paste tab");
  shot("invite-opened");
  await tapText("Add contact", true);
  const d = await until(() => phoneContact("Dana Kim"), 30000);
  check(d && d.verified === false, "the phone has Dana as a contact, under the name she offers, not verified");
  check(await waitText("Dana Kim", 15000), "the phone opens the conversation with Dana");
  const p = await until(async () => { await dana.invoke("sync"); return (await dana.invoke("overview")).contacts.find((c) => c.name === "Pat Phone"); }, 30000);
  check(p && p.verified === false, "Dana has the phone as a contact, not verified");
  let refused = false;
  try { debug("redeem_invite", { link: danaLink }); } catch (e) { refused = String(e.message).includes("already used"); }
  check(refused, "opening Dana's link a second time fails");
} catch (e) {
  console.log("FAIL", e.message || e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
