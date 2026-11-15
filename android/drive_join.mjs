// Joining an organisation's relay on the phone (ADR 0023), on the emulator against a real relay that requires
// enrollment: Settings, Home relay, Join; the device id is shown; a forged token is refused in words; the real
// token gets the design's confirmation; "Use this relay" makes it home and shows the organisation.
//   cargo build -p spike-relay -p spike-cli && bash android/build-native.sh --release && (cd android && ./gradlew assembleDebug)
//   node android/drive_join.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "../desktop/drive_lib.mjs";
import { createPhone, APK, PKG } from "./phone_lib.mjs";

const PHONE_PASS = "phone test passphrase";
const ORG = "ws://127.0.0.1:18812";
const ADMIN_ADDR = "127.0.0.1:18813";
const D = createDrive({ relayUrl: "ws://127.0.0.1:8787", shotsDir: process.argv[2] });
const { check, tmp } = D;
mkdirSync(D.shots, { recursive: true });
const { adb, sh, nodes, screenText, waitText, tapText, tapField, typeText, shot, hideKeyboard, touch, debug } = createPhone(D);
const bin = (n) => join(D.root, "target", "debug", `${n}.exe`);
const run = (n, args) => execFileSync(bin(n), args, { encoding: "utf8" }).trim();

/** A slow emulator now and then shows "<something> isn't responding": press Wait, as a person would. */
async function settle() {
  for (let i = 0; i < 6; i++) {
    const w = nodes().find((n) => n.text === "Wait");
    if (!w || !screenText().includes("isn't responding")) return;
    console.log("   (the emulator showed an \"isn't responding\" dialog; pressing Wait)");
    sh(`input tap ${Math.round(w.cx)} ${Math.round(w.cy)}`); await sleep(3000);
  }
}
const waitFor = async (sub, ms) => { await settle(); return (await waitText(sub, ms)) || (await settle(), waitText(sub, ms)); };

/** Scroll the page until `sub` shows (Settings pages are longer than the screen). */
async function scrollTo(sub) {
  for (let i = 0; i < 8; i++) {
    if (screenText().includes(sub)) return true;
    sh("input swipe 540 1700 540 900 300"); await sleep(500);
  }
  return screenText().includes(sub);
}
/** Clear a text field and type into it. */
async function fill(index, text) {
  await settle();
  await tapField(index);
  sh("input keyevent KEYCODE_MOVE_END " + "67 ".repeat(text.length + 300));
  sh("input keyevent KEYCODE_MOVE_HOME " + "112 ".repeat(text.length + 300));
  typeText(text);
  hideKeyboard();
}

try {
  console.log("-- setting up: an open relay and an organisation's relay on this PC, a clean install");
  try { execFileSync("taskkill", ["/F", "/IM", "spike-relay.exe"], { stdio: "ignore" }); } catch (_) {}
  const opKey = join(tmp, "operator.key"), adminKey = join(tmp, "admin.key"), forgedKey = join(tmp, "forged.key");
  const opPub = run("spike-relay-admin", ["keygen", "--out", opKey]);
  const adminPub = run("spike", ["tenant-keygen", "--out", adminKey]);
  run("spike", ["tenant-keygen", "--out", forgedKey]);
  D.start(bin("spike-relay"), ["--listen", "127.0.0.1:8787"], {});
  D.start(bin("spike-relay"), ["--listen", "127.0.0.1:18812", "--require-enrollment", "--enroll-no-visitors",
    "--metrics-listen", ADMIN_ADDR, "--operator-key", opPub], {});
  await sleep(1200);
  run("spike-relay-admin", ["--relay", ADMIN_ADDR, "--key", opKey, "create-tenant", "northwind", "--admin-pub", adminPub]);
  adb("install", "-r", APK);
  sh(`pm clear ${PKG}`);
  adb("reverse", "tcp:8787", "tcp:8787");
  adb("reverse", "tcp:18812", "tcp:18812");
  sh("settings put global window_animation_scale 0; settings put global transition_animation_scale 0; settings put global animator_duration_scale 0");
  await settle();
  sh(`am start -n ${PKG}/.MainActivity`);

  console.log("-- an identity on the phone");
  check(await waitFor("Your identity lives on this phone", 60000), "the phone shows the create-identity screen");
  await tapText("Create my identity"); await waitFor("Passphrase for this phone");
  for (let attempt = 0; attempt < 4; attempt++) {
    if (attempt > 0) {
      sh(`am start -n ${PKG}/.MainActivity`);
      await waitFor("Passphrase for this phone");
      for (const i of [0, 1]) { await tapField(i); sh("input keyevent KEYCODE_MOVE_END " + "67 ".repeat(40)); hideKeyboard(); }
    }
    await tapField(0); typeText(PHONE_PASS); hideKeyboard();
    await tapField(1); typeText(PHONE_PASS); hideKeyboard();
    const lens = nodes().filter((n) => n.cls.endsWith("EditText")).map((n) => (n.text || "").length);
    if (lens.length !== 2 || lens.some((l) => l !== PHONE_PASS.length)) continue;
    await tapText("Create my identity");
    if (!(await waitFor("The two passphrases differ", 3000))) break;
    await tapText("OK", true);
  }
  if (await waitFor("Turn on fingerprint unlock", 6000)) touch();
  check(await waitFor("Chats", 30000), "the phone has an identity");
  debug("set_relay", { url: "ws://127.0.0.1:8787" });

  console.log("-- Settings, Home relay, Join");
  await tapText("Settings", true);
  await tapText("Home relay", true);
  check(await scrollTo("Join an organisation's relay"), "Home relay offers to join an organisation's relay");
  await tapText("Join", true);
  const id = debug("overview").me.device_id;
  check(await scrollTo(id), "the phone shows its full device id");
  shot("j1-form");

  const forged = run("spike", ["tenant-token", "--key", forgedKey, "--tenant", "northwind", "--device", id]);
  await scrollTo("Organisation relay address");
  await fill(0, forged);
  await fill(1, ORG);
  await tapText("Continue", true);
  check(await waitFor("Use this relay", 10000), "a token for this phone gets the confirmation");
  await tapText("Use this relay", true);
  check(await waitFor("not valid here", 20000), "a forged token is refused in words");
  check(debug("overview").relay === "ws://127.0.0.1:8787", "the home relay did not change after a refusal");
  shot("j2-refused");

  const tok = run("spike", ["tenant-token", "--key", adminKey, "--tenant", "northwind", "--device", id, "--days", "1"]);
  await scrollTo("Organisation relay address");
  await fill(0, tok);
  await tapText("Continue", true);
  check(await waitFor("Join northwind's relay", 10000), "the confirmation names the organisation");
  const conf = screenText();
  check(conf.includes("This phone only") && conf.includes("Your name, contacts, messages") && conf.includes("127.0.0.1:18812"), "the confirmation says what joining means");
  shot("j3-confirm");
  await tapText("Use this relay", true);
  check(await waitFor("Organisation · northwind", 20000), "the Home relay page shows the organisation");
  const ov = debug("overview");
  check(ov.relay === ORG, "the organisation's relay is the home relay");
  check(ov.organisation && ov.organisation.tenant === "northwind", "the organisation is recorded");
  check(String(debug("test_relay")).startsWith("Connected"), "the phone signs in to the organisation's relay");
  shot("j4-joined");
  const usage = JSON.parse(run("spike-relay-admin", ["--relay", ADMIN_ADDR, "--key", opKey, "usage", "northwind"]));
  check(usage[0].devices === 1, "the relay counts one enrolled device");
} catch (e) {
  console.log("FAIL", e.message); D.fail();
  try { console.log("   the phone shows:", screenText().slice(0, 400)); shot("j-failed"); } catch (_) {}
} finally {
  await D.stop();
}
console.log(D.failures() ? `${D.failures()} failed` : "all passed");
process.exit(D.failures() ? 1 : 0);
