// The phone's screen and the debug door, shared by the Android drivers. Reads the screen through uiautomator,
// taps and types through `adb input`, runs bridge commands through the debug-only broadcast receiver.
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { sleep } from "../desktop/drive_lib.mjs";

export const SDK = process.env.ANDROID_HOME || "D:/Android/sdk";
export const ADB = `${SDK}/platform-tools/adb.exe`;
export const PKG = "network.spike.connect";
export const APK = "android/app/build/outputs/apk/debug/app-debug.apk";

export function createPhone(D) {
  const adb = (...a) => execFileSync(ADB, a, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  const sh = (cmd) => adb("shell", cmd);

  // ---- the phone's screen, read through uiautomator ---------------------------------------------------

  function nodes() {
    let xml = "";
    for (let i = 0; i < 5 && !xml.includes("<node "); i++) {
      // uiautomator now and then answers "null root node" while a screen is changing: ask again.
      try { sh("uiautomator dump /sdcard/ui.xml"); xml = adb("exec-out", "cat", "/sdcard/ui.xml"); } catch (_) {}
      if (!xml.includes("<node ")) execFileSync("node", ["-e", "setTimeout(()=>{},600)"]);
    }
    const out = [];
    for (const m of xml.matchAll(/<node [^>]*>/g)) {
      const a = Object.fromEntries([...m[0].matchAll(/([\w-]+)="([^"]*)"/g)].map((x) => [x[1], x[2]]));
      const b = (a.bounds || "").match(/\[(\d+),(\d+)\]\[(\d+),(\d+)\]/);
      if (!b) continue;
      const [x1, y1, x2, y2] = b.slice(1).map(Number);
      out.push({
        text: (a.text || "").replace(/&amp;/g, "&").replace(/&quot;/g, '"').replace(/&#10;/g, "\n"),
        desc: a["content-desc"] || "", cls: a.class || "", cx: (x1 + x2) / 2, cy: (y1 + y2) / 2, y1,
      });
    }
    return out;
  }
  const screenText = () => nodes().map((n) => n.text || n.desc).filter(Boolean).join(" | ");

  async function waitText(sub, ms = 20000) {
    const end = Date.now() + ms;
    let last = "";
    while (Date.now() < end) {
      last = screenText();
      if (last.includes(sub)) return true;
      await sleep(500);
    }
    console.log(`   (waited for "${sub}"; the phone shows: ${last.slice(0, 300)})`);
    return false;
  }

  function tap(x, y) { sh(`input tap ${Math.round(x)} ${Math.round(y)}`); }
  async function tapText(sub, exact = false) {
    const n = nodes().find((x) => (exact ? x.text === sub || x.desc === sub : (x.text || x.desc).includes(sub)));
    if (!n) throw new Error(`phone: nothing says "${sub}"`);
    tap(n.cx, n.cy);
    await sleep(500);
  }
  async function tapField(index) {
    // A slow emulator can still be bringing the screen back (after Back closed the keyboard): wait a little.
    let f;
    for (let i = 0; i < 20 && !f; i++) {
      f = nodes().filter((n) => n.cls.endsWith("EditText")).sort((a, b) => a.y1 - b.y1)[index];
      if (!f) await sleep(500);
    }
    if (!f) throw new Error(`phone: no text field #${index}`);
    tap(f.cx, f.cy);
    await sleep(400);
  }
  function typeText(text) {
    // `input text` takes %s for a space and trips over shell characters; ours are letters, digits and punctuation.
    for (let i = 0; i < text.length; i += 40) {
      sh(`input text '${text.slice(i, i + 40).replace(/ /g, "%s")}'`);
    }
  }
  function shot(label) {
    writeFileSync(join(D.shots, `phone-${label}.png`), execFileSync(ADB, ["exec-out", "screencap", "-p"], { maxBuffer: 64 * 1024 * 1024 }));
  }
  function back() { sh("input keyevent KEYCODE_BACK"); }
  /** Close the soft keyboard if it is showing (Back would otherwise leave the screen). The emulator must
   *  not be in hardware-keyboard mode (hw.keyboard=no in its config), or Gboard shows a toolbar over the app. */
  function hideKeyboard() {
    if (sh("dumpsys input_method").includes("mInputShown=true")) {
      sh("input keyevent KEYCODE_BACK");
      // Back can also leave the app when the keyboard was already going away: come back (state is kept).
      execFileSync("node", ["-e", "setTimeout(()=>{},500)"]);
      if (!sh("dumpsys activity activities").match(/topResumedActivity=.*network\.spike\.connect/)) sh(`am start -n ${PKG}/.MainActivity`);
    }
  }

  const touch = () => execFileSync(ADB, ["emu", "finger", "touch", "1"]);

  /** Run a bridge command inside the app (debug builds) and return its JSON answer. */
  function debug(cmd, args = {}) {
    const tag = `t${Math.random().toString(36).slice(2, 8)}`;
    adb("logcat", "-c");
    const raw = typeof args === "string" ? args : JSON.stringify(args);
    sh(`am broadcast -n ${PKG}/.DebugReceiver --es cmd '${cmd}' --es args '${raw.replace(/'/g, "")}' --es tag ${tag}`);
    for (let i = 0; i < 60; i++) {
      const log = adb("logcat", "-d", "-s", "SpikeDebug:I");
      if (log.includes(`${tag}#end`)) {
        const parts = [...log.matchAll(new RegExp(`${tag}#(\\d+) (.*)`, "g"))].sort((a, b) => a[1] - b[1]).map((m) => m[2].replace(/\r$/, ""));
        const answer = JSON.parse(parts.join(""));
        if (answer.err) throw new Error(`phone ${cmd}: ${answer.err}`);
        return answer.ok;
      }
      execFileSync("node", ["-e", "setTimeout(()=>{},250)"]);
    }
    throw new Error(`phone ${cmd}: no answer`);
  }


  return { adb, sh, nodes, screenText, waitText, tap, tapText, tapField, typeText, shot, back, hideKeyboard, touch, debug };
}
