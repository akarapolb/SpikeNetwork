// Joining an organisation by scanning its token (ADR 0035), with the emulator's virtual camera: Northwind's admin
// console (a real desktop window) makes a token on its Enrolment page; the QR code the console shows is captured
// from the window and put on the emulator's wall poster; the phone scans it on Settings, Home relay, Join…, gets
// the join sheet, and joins Northwind's relay.
//
// The virtual scene reads its poster when the emulator starts, so this runs in two steps around a restart:
//   bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   ANDROID_SERIAL=emulator-5554 node android/drive_org_scan.mjs prepare      (the console makes the token; poster)
//   restart the emulator with hw.camera.back = virtualscene in the AVD's config.ini
//   ANDROID_SERIAL=emulator-5554 node android/drive_org_scan.mjs [screenshot-dir]   (scan, join; restores the poster)
// The token is single use and valid 24 hours: run `prepare` again for each scan.
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, SDK, APK, PKG } from "./phone_lib.mjs";

const PREPARE = process.argv[2] === "prepare";
const PORT = 18831, ORG_PORT = 18832;
const RELAY = `ws://127.0.0.1:${PORT}`, ORG_RELAY = `ws://127.0.0.1:${ORG_PORT}`;
const POSTER = `${SDK}/emulator/resources/poster.png`;
const STATE = "android/camera-org.json"; // ignored by git: the admin public key and token of this test only

if (!process.env.ANDROID_SERIAL) {
  // This driver wipes the app's data: never let it pick a real phone by accident.
  console.error("Set ANDROID_SERIAL to the emulator to drive (for example emulator-5554).");
  process.exit(2);
}
const D = createDrive({ relayUrl: RELAY, shotsDir: PREPARE ? undefined : process.argv[2] });
const { check } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, waitText, tapText, shot, debug, screenText } = createPhone(D);
const relayExe = join(D.root, "target", "debug", "spike-relay.exe");

async function until(fn, ms = 25000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { try { const v = await fn(); if (v) return v; } catch (_) {} await sleep(500); }
  return null;
}

/** Northwind's relay, enrolled mode, with the tenant registered under the console's admin key. */
function startOrgRelay(adminPub, dbName) {
  const db = join(D.tmp, dbName);
  execFileSync(relayExe, ["--db", db, "tenant", "create", "northwind", "--name", "Northwind", "--admin-pub", adminPub]);
  D.start(relayExe, ["--listen", `127.0.0.1:${ORG_PORT}`, "--db", db, "--enrolled"], {});
}

/** The console makes a token on its Enrolment page; the QR image it shows becomes the camera's poster. */
async function prepare() {
  const ann = await D.launch("ann", 9451);
  await ann.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  const admin = await ann.invoke("org_admin_create", { tenant: "northwind", name: "Northwind", relay: ORG_RELAY });
  // The console reads its pages from the organisation's relay: a throwaway one for this step.
  startOrgRelay(admin.admin_pub, "prepare.db");
  await sleep(1000);
  await ann.refresh();
  await ann.eval(`st.oc=null; go('org');`);
  check(await until(async () => (await ann.text()).includes("Healthy")), "the console reaches Northwind's relay");
  await ann.eval(`st.orgSec='devices'; render();`); await sleep(500);
  await ann.click("Make token");
  const shown = await until(() => ann.eval(`return !!document.querySelector('.orgwiz img.qrimg[src^="data:"]')`));
  check(shown, "the console shows the token as a QR code");
  await sleep(500);
  const token = await ann.eval(`return st.orgTok.made.token`);
  check(String(token).startsWith("spikeorg1:"), "the console made a spikeorg1 token");
  const r = await ann.eval(`const b = document.querySelector('.orgwiz img.qrimg').getBoundingClientRect(); return {x:b.x, y:b.y, w:b.width, h:b.height}`);
  // The image as the window draws it, four times over, on a white square with room around it.
  const pad = 24;
  const cap = await ann.send("Page.captureScreenshot", { format: "png", clip: { x: r.x - pad, y: r.y - pad, width: r.w + 2 * pad, height: r.h + 2 * pad, scale: 4 } });
  if (!existsSync(POSTER + ".orig")) copyFileSync(POSTER, POSTER + ".orig");
  writeFileSync(POSTER, Buffer.from(cap.result.data, "base64"));
  writeFileSync(join(D.shots, "console-qr.png"), Buffer.from(cap.result.data, "base64"));
  writeFileSync(STATE, JSON.stringify({ adminPub: admin.admin_pub, token }));
  console.log("poster written; restart the emulator (hw.camera.back = virtualscene), then run this without 'prepare'");
}

/** A clean install scans the poster and joins. */
async function scan() {
  const st = JSON.parse(readFileSync(STATE, "utf8"));
  D.start(relayExe, ["--listen", `127.0.0.1:${PORT}`], {});
  startOrgRelay(st.adminPub, "org-relay.db");
  await sleep(1000);

  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  sh(`pm grant ${PKG} android.permission.CAMERA`);
  for (const p of [PORT, ORG_PORT]) adb("reverse", `tcp:${p}`, `tcp:${p}`);
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  // The engine lives in the app's process: make the identity first, then show the screens.
  debug("create_identity", { passphrase: "org scan test passphrase" });
  debug("set_relay", { url: RELAY });
  sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Chats", 30000), "the phone is ready");

  console.log("-- Settings, Home relay, Join…, Scan");
  await tapText("Settings", true); await sleep(600);
  await tapText("Home relay", true); await sleep(800);
  check(await waitText("Join an organisation"), "Home relay offers to join an organisation");
  await tapText("Join…", true);
  await tapText("Scan", true);
  await sleep(2500);
  shot("org-scan-1-camera");
  check(await waitText("Join Northwind's relay", 60000), "the scanner read the console's code and shows the join sheet");
  const sheet = screenText();
  check(sheet.includes("This phone only") && sheet.includes("127.0.0.1:" + ORG_PORT), "the sheet names what it enrols and the relay the token carries");
  shot("org-scan-2-sheet");
  await tapText("Use this relay", true);
  check(await until(() => debug("overview").relay === ORG_RELAY), "the phone's home relay is now Northwind's");
  check(await waitText("Managed by Northwind"), "Home relay says who manages the phone");
  shot("org-scan-3-joined");
}

try {
  if (PREPARE) await prepare(); else await scan();
} catch (e) {
  console.log("FAIL", e.message || e);
  D.fail();
  try { console.log("   the phone shows:", screenText().slice(0, 300)); shot("org-scan-failed"); } catch (_) {}
} finally {
  if (!PREPARE && existsSync(POSTER + ".orig")) copyFileSync(POSTER + ".orig", POSTER);
  await D.stop();
  console.log(D.failures() ? `${D.failures()} check(s) failed` : "all passed");
  process.exit(D.failures() ? 1 : 0);
}
