// Makes the emulator's virtual camera look at a contact's QR code, so the app's scanner can be tested without
// a phone. It makes a contact card (from a desktop app), draws it as a QR code (using the phone app's own
// `qr_matrix` command), and writes it over the emulator's wall poster (the original is kept as poster.png.orig).
// Afterwards restart the emulator with its back camera set to virtualscene:
//   hw.camera.back = virtualscene   (in the AVD's config.ini)
//   node android/make_camera_poster.mjs     then     node android/drive_camera.mjs
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, writeFileSync } from "node:fs";
import { createDrive } from "../desktop/drive_lib.mjs";
import { createPhone, qrPng as png, SDK, APK, ADB, PKG } from "./phone_lib.mjs";

const D = createDrive({ relayUrl: "ws://127.0.0.1:8787" });
const { sh, debug } = createPhone(D);
const POSTER = `${SDK}/emulator/resources/poster.png`;

try {
  const desk = await D.launch("cardmaker", 9351);
  const me = (await desk.invoke("overview")).me;
  execFileSync(ADB, ["install", "-r", APK]);
  sh(`pm clear ${PKG}`);
  debug("create_identity", { passphrase: "poster maker passphrase" });
  const q = debug("qr_matrix", { text: me.card });
  if (!existsSync(POSTER + ".orig")) copyFileSync(POSTER, POSTER + ".orig");
  writeFileSync(POSTER, png(q.rows, Math.max(4, Math.floor(1000 / (q.size + 8)))));
  writeFileSync("android/camera-card.json", JSON.stringify({ fingerprint: me.fingerprint, card: me.card }));
  console.log(`poster written (${q.size} modules, fingerprint ${me.fingerprint}); restart the emulator with hw.camera.back = virtualscene`);
} finally {
  await D.stop();
}
