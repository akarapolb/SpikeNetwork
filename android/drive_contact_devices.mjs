// Contact details on the real Android app (in the emulator): each of a contact's devices, with when the phone last
// heard from it. The contact is the command-line client. Checks the screen opens from the conversation's Details
// button, lists the device by its short id, says "Not heard from yet" before a message from it arrives and
// "active now" after, and that Safety number leads to the verify screen.
// Needs Node 22+, Windows, the Android SDK with a booted emulator, the debug APK, debug builds of the relay and CLI:
//   bash android/build-native.sh && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay -p spike-cli
//   ANDROID_SERIAL=emulator-5554 node android/drive_contact_devices.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const PORT = 18813;
const RELAY = `ws://127.0.0.1:${PORT}`;

if (!process.env.ANDROID_SERIAL) {
  // This driver wipes the app's data: never let it pick a real phone by accident.
  console.error("Set ANDROID_SERIAL to the emulator to drive (for example emulator-5554).");
  process.exit(2);
}
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tapText, tapField, typeText, shot, back, hideKeyboard, debug } = createPhone(D);

const bobHome = join(mkdtempSync(join(tmpdir(), "spike-bob-")), "home");
const cli = (...a) =>
  execFileSync(join(D.root, "target", "debug", "spike.exe"), ["--home", bobHome, "--allow-insecure-transport", ...a], {
    encoding: "utf8", env: { ...process.env, SPIKE_PASSPHRASE: "bob test passphrase", SPIKE_RELAY: RELAY },
  });
async function fill(index, text) { await tapField(index); typeText(text); hideKeyboard(); }
async function until(fn, ms = 25000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(500); }
  return null;
}
const bobOnPhone = () => debug("overview").contacts.find((c) => c.name === "Bob");

try {
  console.log("-- setting up: relay on this PC, clean install on the emulator");
  D.start(join(D.root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`], {});
  await sleep(1000);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", `tcp:${PORT}`, `tcp:${PORT}`);
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Your identity lives on this phone", 60000), "the phone shows the create screen");
  await tapText("Create my identity"); await waitText("Passphrase for this phone");
  for (let attempt = 0; attempt < 4; attempt++) {
    await fill(0, PHONE_PASS);
    await fill(1, PHONE_PASS);
    if (nodes().filter((n) => n.cls.endsWith("EditText") && (n.text || "").length > 0).length === 2) break;
    sh(`am start -n ${PKG}/.MainActivity`);
    await sleep(1500);
  }
  await tapText("Create my identity");
  if (await waitText("Turn on fingerprint unlock", 6000)) await tapText("Not now").catch(() => {});
  check(await waitText("Chats"), "the identity is created");
  debug("set_relay", { url: RELAY });
  cli("init");
  cli("add", "Phone", debug("overview").me.card);
  cli("keys");
  const bobCard = cli("card").match(/spike2:[0-9a-f]+/)[0];
  debug("add_contact", { name: "Bob", card: bobCard });
  debug("sync");
  debug("send", { name: "Bob", text: "hi Bob" });

  console.log("-- before Bob has written");
  let bob = bobOnPhone();
  const dev = bob.device_list?.[0];
  check(bob.device_list?.length === bob.devices && !!dev, `the contact carries its signed device list (${JSON.stringify(bob.device_list)})`);
  check(dev?.last_heard === null, "sending to Bob does not mark his device as heard from");
  sh(`am start -n ${PKG}/.MainActivity`);
  await sleep(800);
  await tapText("Bob");
  check(await waitText("hi Bob"), "the conversation opens");
  await tapText("Details", true);
  check(await waitText("Safety number"), "Details opens the contact details");
  let t = screenText();
  check(t.includes(`DEVICES · ${bob.devices}`), "the Devices caption gives the count");
  check(t.includes(`Device ${dev.id.slice(0, 6)}`), "the device is labelled by its short id, not an invented name");
  check(t.includes("Not heard from yet"), "a device nothing has arrived from says so");
  check(t.includes("Message") && t.includes("Call") && t.includes("Video"), "the quick actions are there, as in the design");
  check(t.includes("The relay is never asked"), "the screen says where last-heard comes from");
  shot("contact-devices-1-before");

  console.log("-- after Bob writes");
  cli("recv");
  const before = Date.now();
  cli("send", "Phone", "hi from Bob");
  const heard = await until(async () => { debug("sync"); return bobOnPhone().device_list?.[0]?.last_heard; });
  check(typeof heard === "number" && heard >= before - 120000, `the phone records when Bob's message arrived (${heard})`);
  back(); await sleep(600);
  await tapText("Details", true);
  check(await waitText("active now", 10000), "the device now reads active now");
  shot("contact-devices-2-after");

  console.log("-- safety number");
  await tapText("Safety number");
  check(await waitText("Read the codes aloud"), "Safety number opens the verify screen");
  back(); await sleep(500);
  check(await waitText("hi from Bob"), "back from verify returns to the conversation");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
