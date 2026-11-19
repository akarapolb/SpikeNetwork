// Scanning an enrollment token on the phone (ADR 0023): the admin's `spike tenant-token` draws the token as a
// QR code in the terminal; this reads that drawing back from the characters, puts it on the emulator's wall
// poster, and has the phone scan it with the virtual camera on Settings, Home relay, Join, then join.
//
// The virtual scene reads its poster when the emulator starts, so this runs in two steps around a restart:
//   cargo build -p spike-relay -p spike-cli && bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   node android/drive_join_scan.mjs prepare        (installs, makes an identity and a token, writes the poster)
//   restart the emulator with hw.camera.back = virtualscene in the AVD's config.ini
//   node android/drive_join_scan.mjs [screenshot-dir]   (unlocks, scans, joins; restores the poster)
import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, qrPng, SDK, APK, PKG } from "./phone_lib.mjs";

const PREPARE = process.argv[2] === "prepare";
const ORG = "ws://127.0.0.1:18822";
const ADMIN_ADDR = "127.0.0.1:18823";
const PASS = "scan test passphrase";
const POSTER = `${SDK}/emulator/resources/poster.png`;
const STATE = "android/camera-join.json"; // ignored by git: the admin key and token of this test only
const D = createDrive({ relayUrl: "ws://127.0.0.1:8787", shotsDir: PREPARE ? undefined : process.argv[2] });
const { check, tmp } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, screenText, waitText, tapText, tapField, typeText, shot, hideKeyboard, debug } = createPhone(D);
const bin = (n) => join(D.root, "target", "debug", `${n}.exe`);
const run = (n, args) => execFileSync(bin(n), args, { encoding: "utf8" }).trim();

/** The terminal drawing as rows of "0"/"1", quiet zone removed (the same reading as `spike_cli::qr::read_terminal`). */
function readDrawing(stderr) {
  const lines = stderr.split(/\r?\n/).filter((l) => l.includes("\x1b[30;107m")).map((l) => l.replace(/\x1b\[[0-9;]*m/g, ""));
  const rows = [];
  for (const l of lines) {
    rows.push([...l].map((c) => (c === "█" || c === "▀" ? "1" : "0")).join(""));
    rows.push([...l].map((c) => (c === "█" || c === "▄" ? "1" : "0")).join(""));
  }
  const q = 4, n = rows[0].length - 2 * q;
  return rows.slice(q, q + n).map((r) => r.slice(q, q + n));
}

async function prepare() {
  const adminKey = join(tmp, "admin.key");
  const adminPub = run("spike", ["tenant-keygen", "--out", adminKey]);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  sh(`pm grant ${PKG} android.permission.CAMERA`);
  debug("create_identity", { passphrase: PASS });
  const id = debug("overview").me.device_id;

  const r = spawnSync(bin("spike"), ["tenant-token", "--key", adminKey, "--tenant", "northwind", "--device", id, "--days", "1", "--qr", "always"], { encoding: "utf8" });
  const tok = r.stdout.trim();
  if (r.status !== 0) console.log("   tenant-token said:", r.stderr.trim());
  check(r.status === 0 && tok.startsWith("spike-enroll1:"), "tenant-token prints the token's text on standard output");
  check(!/[█▀▄]/.test(r.stdout), "the drawing stays off standard output, so scripts get the text alone");
  const rows = readDrawing(r.stderr);
  check(rows.length > 20 && rows.every((x) => x.length === rows.length), `the terminal shows a square QR code (${rows.length} modules)`);
  const quiet = spawnSync(bin("spike"), ["tenant-token", "--key", adminKey, "--tenant", "northwind", "--device", id], { encoding: "utf8" });
  check(!/[█▀▄]/.test(quiet.stderr), "without a terminal (and without --qr always) nothing is drawn");

  if (!existsSync(POSTER + ".orig")) copyFileSync(POSTER, POSTER + ".orig");
  writeFileSync(POSTER, qrPng(rows, Math.max(4, Math.floor(1000 / (rows.length + 8)))));
  writeFileSync(STATE, JSON.stringify({ adminPub, token: tok, deviceId: id }));
  console.log("poster written; restart the emulator (hw.camera.back = virtualscene), then run this without 'prepare'");
}

async function scan() {
  const st = JSON.parse(readFileSync(STATE, "utf8"));
  try { execFileSync("taskkill", ["/F", "/IM", "spike-relay.exe"], { stdio: "ignore" }); } catch (_) {}
  const opKey = join(tmp, "operator.key");
  const opPub = run("spike-relay-admin", ["keygen", "--out", opKey]);
  D.start(bin("spike-relay"), ["--listen", "127.0.0.1:8787"], {});
  D.start(bin("spike-relay"), ["--listen", "127.0.0.1:18822", "--require-enrollment", "--enroll-no-visitors",
    "--metrics-listen", ADMIN_ADDR, "--operator-key", opPub], {});
  await sleep(1200);
  run("spike-relay-admin", ["--relay", ADMIN_ADDR, "--key", opKey, "create-tenant", "northwind", "--admin-pub", st.adminPub]);
  adb("reverse", "tcp:8787", "tcp:8787");
  adb("reverse", "tcp:18822", "tcp:18822");
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  debug("unlock", { passphrase: PASS });
  debug("set_relay", { url: "ws://127.0.0.1:8787" });
  check(debug("overview").me.device_id === st.deviceId, "the phone is the one the token was made for");

  console.log("-- Settings, Home relay, Join, Scan the token's code");
  sh(`am start -n ${PKG}/.MainActivity`);
  check(await waitText("Chats", 30000), "the phone is ready");
  await tapText("Settings", true);
  await tapText("Home relay", true);
  for (let i = 0; i < 8 && !screenText().includes("Join an organisation's relay"); i++) { sh("input swipe 540 1700 540 900 300"); await sleep(500); }
  await tapText("Join", true);
  await tapText("Scan the token's code", true);
  await sleep(2500);
  shot("s1-camera");
  let scanned = false;
  for (let i = 0; i < 60 && !scanned; i++) {
    await sleep(1000);
    scanned = screenText().includes("Scan the token's code");
  }
  check(scanned, "the scanner read the code and closed");
  shot("s2-scanned");
  for (let i = 0; i < 8 && !screenText().includes("Organisation relay address"); i++) { sh("input swipe 540 1700 540 900 300"); await sleep(500); }
  await tapField(1);
  sh("input keyevent KEYCODE_MOVE_END " + "67 ".repeat(40));
  typeText(ORG); hideKeyboard();
  await tapText("Continue", true);
  check(await waitText("Join northwind's relay", 10000), "the scanned token gets the confirmation");
  shot("s3-confirm");
  await tapText("Use this relay", true);
  check(await waitText("Organisation · northwind", 20000), "the phone joined with the scanned token");
  const ov = debug("overview");
  check(ov.relay === ORG && ov.organisation && ov.organisation.tenant === "northwind", "the organisation's relay is home");
  shot("s4-joined");
}

try {
  if (PREPARE) await prepare(); else await scan();
} catch (e) {
  console.log("FAIL", e.message); D.fail();
  try { console.log("   the phone shows:", screenText().slice(0, 400)); shot("s-failed"); } catch (_) {}
} finally {
  if (!PREPARE && existsSync(POSTER + ".orig")) copyFileSync(POSTER + ".orig", POSTER);
  await D.stop();
}
console.log(D.failures() ? `${D.failures()} failed` : "all passed");
process.exit(D.failures() ? 1 : 0);
