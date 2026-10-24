// Makes the emulator's virtual camera look at a contact's QR code, so the app's scanner can be tested without
// a phone. It makes a contact card (from a desktop app), draws it as a QR code (using the phone app's own
// `qr_matrix` command), and writes it over the emulator's wall poster (the original is kept as poster.png.orig).
// Afterwards restart the emulator with its back camera set to virtualscene:
//   hw.camera.back = virtualscene   (in the AVD's config.ini)
//   node android/make_camera_poster.mjs     then     node android/drive_camera.mjs
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";
import { createDrive } from "../desktop/drive_lib.mjs";
import { createPhone, SDK, APK, ADB, PKG } from "./phone_lib.mjs";

const D = createDrive({ relayUrl: "ws://127.0.0.1:8787" });
const { sh, debug } = createPhone(D);
const POSTER = `${SDK}/emulator/resources/poster.png`;

function crc32(buf) {
  let c, crc = 0xffffffff;
  for (const b of buf) { c = (crc ^ b) & 0xff; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; crc = (crc >>> 8) ^ c; }
  return (crc ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}
/** A grayscale PNG of the QR rows: `scale` pixels per module, a 4-module quiet zone, on a 1024 square canvas. */
function png(rows, scale) {
  const n = rows.length, quiet = 4, side = (n + 2 * quiet) * scale;
  const raw = Buffer.alloc((side + 1) * side, 255);
  for (let y = 0; y < side; y++) {
    raw[y * (side + 1)] = 0;
    for (let x = 0; x < side; x++) {
      const my = Math.floor(y / scale) - quiet, mx = Math.floor(x / scale) - quiet;
      if (my >= 0 && my < n && mx >= 0 && mx < n && rows[my][mx] === "1") raw[y * (side + 1) + 1 + x] = 0;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(side, 0); ihdr.writeUInt32BE(side, 4); ihdr[8] = 8; ihdr[9] = 0;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

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
