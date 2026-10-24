// Scans a contact's QR code with the emulator's virtual camera. Run android/make_camera_poster.mjs first and
// restart the emulator with hw.camera.back = virtualscene (see that file). No relay or desktop is needed.
//   node android/drive_camera.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { readFileSync, mkdirSync } from "node:fs";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const D = createDrive({ relayUrl: "ws://127.0.0.1:8787", shotsDir: process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, screenText, waitText, tapText, tapField, typeText, shot, hideKeyboard, debug } = createPhone(D);
const expect = JSON.parse(readFileSync("android/camera-card.json", "utf8"));

try {
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  // The engine lives in the app's process: make the identity first, then show the screens.
  debug("create_identity", { passphrase: "camera test passphrase" });
  sh(`am start -n ${PKG}/.MainActivity`);
  await sleep(2500);
  check(await waitText("Chats"), "the phone is ready");
  await tapText("Add a contact");
  await waitText("Add a contact");
  await tapText("Scan", true);
  if (await waitText("While using the app", 6000)) { shot("c0-permission"); await tapText("While using the app"); }
  await sleep(1500);
  shot("c1-camera");
  const seen = await waitText("Got a card", 60000);
  shot("c2-scanned");
  check(seen, "the scanner read the QR code on the poster");
  if (seen) {
    await tapField(0); typeText("Scanned"); hideKeyboard();
    await tapText("Add contact", true);
    check(await waitText("Scanned", 10000), "the scanned contact is in the chat list");
    const c = debug("overview").contacts.find((x) => x.name === "Scanned");
    check(!!c && c.fingerprint === expect.fingerprint, "its fingerprint is the one on the poster");
  }
} catch (e) {
  console.log("ERROR", e.message);
  try { shot("error"); } catch (_) {}
  D.fail();
} finally {
  await D.stop();
  const failures = D.failures();
  console.log(failures ? `${failures} FAILED` : "all passed", `· screenshots in ${D.shots}`);
  process.exit(failures ? 1 : 0);
}
