// Screenshots of the desktop screens the main screenshot driver does not reach, for comparing with the design:
// first run, each Settings section, the relay unreachable, and find in a conversation. Not a test.
//   cargo build -p spike-relay && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_audit.mjs [screenshot-dir]
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const PORT = 18796;
const D = createDrive({ relayUrl: `ws://127.0.0.1:${PORT}`, shotsDir: process.argv[2] });
const { root, tmp, start, launch, Page } = D;
const size = (p) => p.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });

try {
  const relay = start(join(root, "target", "debug", "spike-relay.exe"), ["--listen", `127.0.0.1:${PORT}`], {});
  await sleep(1000);

  // First run: a window with no identity yet, before anything is created.
  start(process.env.SPIKE_DESKTOP_EXE || join(root, "desktop", "src-tauri", "target", "debug", "spike-desktop.exe"), [], {
    SPIKE_DESKTOP_HOME: join(tmp, "fresh"), WEBVIEW2_USER_DATA_FOLDER: join(tmp, "fresh-wv"),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=9361",
  });
  const fresh = new Page("fresh", 9361);
  await fresh.connect(); await sleep(2000); await size(fresh); await sleep(300);
  await fresh.shot("first-run");

  const alice = await launch("alice", 9362);
  await size(alice); await alice.refresh(); await sleep(500);
  await alice.eval(`go('settings')`); await sleep(800);
  for (const [id, label] of [["set-identity", "identity"], ["set-relay", "relay"], ["backupCard", "backup"], ["calls-card", "calls"], ["apps-card", "apps"]]) {
    await alice.eval(`st.setSec=${JSON.stringify(id)}; render();`);
    await sleep(600);
    await alice.shot(`settings-${label}`);
  }
  await alice.eval(`go('chats')`); await sleep(500);

  // The relay goes away: the window says so once, and keeps working.
  relay.kill();
  for (let i = 0; i < 40 && !(await alice.text()).includes("Cannot reach the relay"); i++) await sleep(500);
  await alice.shot("relay-unreachable");
} catch (e) {
  console.error(e);
  D.fail();
} finally {
  await D.stop();
  console.log(`done · screenshots in ${D.shots}`);
  process.exit(0);
}
