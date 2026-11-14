// Pictures and your own devices on the real Android app (in the emulator), with the real desktop app as a contact and
// a second desktop linked as another device of the phone's:
//   - picture viewer: a picture from a contact shows in its bubble; tapping opens it full screen; double tap zooms;
//     Save puts it in the phone's Pictures; Share offers the system share sheet;
//   - send to my devices: a note and a file from the phone reach the linked desktop, a note and a picture from it reach
//     the phone, and the contact receives none of them.
// Needs Node 22+, Windows, the Android SDK with a booted emulator, the debug APK, debug builds of the relay and desktop:
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   ANDROID_SERIAL=emulator-5554 node android/drive_phone_extras.mjs [screenshot-dir]
import { mkdirSync } from "node:fs";
import { deflateSync, crc32 } from "node:zlib";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const PORT = 18813;
const RELAY = `ws://127.0.0.1:${PORT}`;

if (!process.env.ANDROID_SERIAL) {
  console.error("Set ANDROID_SERIAL to the emulator to drive (for example emulator-5554).");
  process.exit(2);
}
const D = createDrive({ relayUrl: RELAY, shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tap, tapText, tapField, typeText, shot, back, hideKeyboard, debug } = createPhone(D);

async function fill(index, text) { await tapField(index); typeText(text); hideKeyboard(); }
async function until(fn, ms = 25000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(500); }
  return null;
}
const byDesc = (sub) => nodes().find((n) => (n.desc || "").includes(sub));

/** A small PNG (a diagonal gradient), so the phone has a real picture to decode. */
function png(w, h, seed) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td) >>> 0);
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  const raw = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const o = y * (w * 3 + 1) + 1 + x * 3;
    raw[o] = (x * 255 / w + seed) & 255; raw[o + 1] = (y * 255 / h) & 255; raw[o + 2] = 160;
  }
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}
const sendBytes = (desk, cmd, bytes, headers) =>
  desk.eval(`const b=new Uint8Array(${JSON.stringify([...bytes])}); return await window.__TAURI__.core.invoke(${JSON.stringify(cmd)}, b, {headers:${JSON.stringify(headers)}});`);

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

  console.log("-- a contact on the desktop");
  const desk = await D.launch("desktop", 9351);
  await desk.invoke("add_contact", { name: "Phone", card: debug("overview").me.card });
  debug("add_contact", { name: "Alice", card: (await desk.invoke("overview")).me.card });
  await desk.invoke("send", { name: "Phone", text: "a picture is coming" });
  check(await waitText("a picture is coming", 25000), "the phone receives the contact's message");

  console.log("-- picture viewer");
  await sendBytes(desk, "send_file", png(320, 200, 0), { "x-contact": "Phone", "x-filename": "icons-v2.png", "x-caption": "" });
  await tapText("Alice");
  check(!!(await until(() => byDesc("Picture icons-v2.png"))), "the picture shows in its bubble");
  shot("extras-1-picture-in-chat");
  const pic = byDesc("Picture icons-v2.png");
  tap(pic.cx, pic.cy);
  check(await waitText("Pinch to zoom", 10000), "tapping the picture opens the viewer, which says how to zoom");
  check(screenText().includes("icons-v2.png") && screenText().includes("Alice"), "the viewer says who sent it and its name");
  shot("extras-2-viewer");
  const view = nodes().find((n) => n.desc === "Picture viewer") || { cx: 540, cy: 1100 };
  sh(`input tap ${Math.round(view.cx)} ${Math.round(view.cy)}`); await sleep(60); sh(`input tap ${Math.round(view.cx)} ${Math.round(view.cy)}`);
  check(await waitText("Double tap to fit", 5000), "a double tap zooms in");
  shot("extras-3-zoomed");
  await tapText("Save the picture", true);
  check(await waitText("Saved to Pictures", 5000), "Save says where the picture went");
  const media = sh("content query --uri content://media/external/images/media --projection _display_name:relative_path");
  check(media.includes("icons-v2") && media.includes("Spike Connect"), "the picture is in the phone's Pictures/Spike Connect");
  await tapText("Share the picture", true);
  check(await until(() => /chooser|resolver/i.test(sh("dumpsys window | grep mCurrentFocus")), 8000), "Share opens the system share sheet");
  back(); await sleep(800);
  if (!screenText().includes("Close the picture")) { back(); await sleep(500); }
  await tapText("Close the picture", true).catch(() => back());
  check(await waitText("Message Alice", 5000), "closing the viewer returns to the conversation");
  back(); await sleep(800);

  console.log("-- link a second desktop as another device of the phone's");
  const desk2 = await D.launch("desktop2", 9352);
  const code = await desk2.invoke("link_offer");
  await tapText("Devices", true); await sleep(600);
  check(!screenText().includes("Send to my devices"), "with one device there is nothing to send to");
  await tapText("Link a device");
  await waitText("Scan the code");
  await tapField(0); typeText(code); hideKeyboard();
  const waiting = desk2.invoke("link_wait");
  await sleep(300);
  await tapText("Continue", true);
  await waiting;
  await waitText("Compare this code");
  await tapText("The codes match", true);
  await waitText("What should it receive");
  const received = desk2.invoke("link_receive");
  await tapText("Link device", true);
  check((await received).contacts >= 1, "the second desktop is linked");
  await sleep(1500);
  await tapText("Devices", true); await sleep(800);

  console.log("-- send to my devices");
  check(await waitText("Send to my devices", 8000), "the Devices tab offers Send to my devices");
  await tapText("Send to my devices", true);
  check(await waitText("Only your devices", 8000), "the send screen opens");
  check(screenText().includes("Picture") && screenText().includes("Camera") && screenText().includes("File"), "it offers Picture, Camera and File");
  await tapText("A note for your devices"); typeText("note for the laptop"); hideKeyboard();
  await tapText("Send the note", true);
  const gotNote = await until(async () => (await desk2.invoke("own_notes")).find((n) => n.body === "note for the laptop" && !n.out));
  check(!!gotNote, "the linked desktop receives the phone's note");
  check(await waitText("Sent to Device", 8000), "the phone lists it as sent to the other device");
  sh(`run-as ${PKG} sh -c 'echo plan v4 > cache/pilot-plan-v4.txt'`);
  debug("send_to_devices", { file_name: "pilot-plan-v4.txt", path: `/data/data/${PKG}/cache/pilot-plan-v4.txt` });
  const gotFile = await until(async () => (await desk2.invoke("own_notes")).find((n) => n.file && n.file.name === "pilot-plan-v4.txt"));
  check(!!gotFile, "the linked desktop receives the phone's file");
  if (gotFile) {
    const saved = await desk2.invoke("save_attachment", { id: gotFile.id });
    check(!!saved, "the desktop saves the file");
  }
  await desk2.invoke("send_note_to_devices", { text: "from the laptop" });
  check(await waitText("from the laptop", 25000), "the phone shows the desktop's note without being asked");
  await sendBytes(desk2, "send_file_to_devices", png(200, 300, 90), { "x-filename": "IMG_2041.png" });
  check(await waitText("IMG_2041.png", 25000), "the phone lists the desktop's picture");
  check(screenText().includes("From Device"), "it says which device it came from");
  shot("extras-4-send-to-devices");
  await tapText("IMG_2041.png");
  check(await waitText("Pinch to zoom", 10000), "tapping a picture there opens the viewer");
  await tapText("Close the picture", true);
  await sleep(600);
  const contactSaw = await desk.invoke("conversation", { name: "Phone" });
  check(!contactSaw.some((m) => ["note for the laptop", "from the laptop"].includes(m.body) || (m.file && ["pilot-plan-v4.txt", "IMG_2041.png"].includes(m.file.name))), "the contact received none of it");
  const phoneChat = debug("conversation", { name: "Alice" });
  check(!phoneChat.some((m) => m.body === "from the laptop"), "notes to your devices stay out of conversations");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all checks passed");
  process.exit(D.failures() ? 1 : 0);
}
