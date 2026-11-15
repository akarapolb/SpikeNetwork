// Joining an organisation's relay from the desktop app (ADR 0023), in a real window against a real relay that
// requires enrollment. Settings, Home relay: Join, copy the device id, paste the token and address, check the
// confirmation, "Use this relay"; then the relay is home and the organisation is shown. Also a refused token.
//   cargo build -p spike-relay -p spike-cli && (cd desktop/src-tauri && cargo build)
//   node desktop/drive_join.mjs [screenshot-dir]
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { createDrive, sleep } from "./drive_lib.mjs";

const OPEN = "ws://127.0.0.1:18801";
const ORG = "ws://127.0.0.1:18802";
const ADMIN_ADDR = "127.0.0.1:18803";
const D = createDrive({ relayUrl: OPEN, shotsDir: process.argv[2] });
const { root, tmp, start, launch, check } = D;
const bin = (n) => join(root, "target", "debug", `${n}.exe`);
const run = (n, args) => execFileSync(bin(n), args, { encoding: "utf8" }).trim();

try {
  const opKey = join(tmp, "operator.key"), adminKey = join(tmp, "northwind-admin.key");
  const opPub = run("spike-relay-admin", ["keygen", "--out", opKey]);
  const adminPub = run("spike", ["tenant-keygen", "--out", adminKey]);
  start(bin("spike-relay"), ["--listen", "127.0.0.1:18801"], {});
  start(bin("spike-relay"), ["--listen", "127.0.0.1:18802", "--require-enrollment", "--enroll-no-visitors",
    "--metrics-listen", ADMIN_ADDR, "--operator-key", opPub], {});
  await sleep(1200);
  run("spike-relay-admin", ["--relay", ADMIN_ADDR, "--key", opKey, "create-tenant", "northwind", "--admin-pub", adminPub, "--devices", "50"]);

  const a = await launch("alice", 9361);
  await a.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });
  await a.eval(`go('settings'); st.setSec='set-relay'; render();`); await sleep(400);
  check((await a.text()).includes("Join an organisation's relay"), "Home relay offers to join an organisation's relay");
  await a.click("Join", "#set-relay");
  const id = await a.eval(`return st.ov.me.device_id`);
  check((await a.text()).includes(id), "the form shows this computer's full device id");
  await a.shot("join-1-form");

  // A token signed by someone who is not the organisation's admin: refused, nothing changes.
  const forgedKey = join(tmp, "forged.key");
  run("spike", ["tenant-keygen", "--out", forgedKey]);
  const forged = run("spike", ["tenant-token", "--key", forgedKey, "--tenant", "northwind", "--device", id]);
  const fill = async (tok, url) => a.eval(`const t=document.querySelector('[data-keep=jointok]'); t.value=${JSON.stringify(tok)}; t.dispatchEvent(new Event('input',{bubbles:true}));
    const u=document.querySelector('[data-keep=joinurl]'); u.value=${JSON.stringify(url)}; u.dispatchEvent(new Event('input',{bubbles:true}));`);
  await fill(forged, ORG);
  await a.click("Continue", "#set-relay");
  await a.click("Use this relay", "#set-relay"); await sleep(1500);
  const refusedText = await a.text();
  check(refusedText.includes("not valid here"), "a forged token is refused in words");
  check((await a.invoke("overview")).relay === OPEN, "the home relay did not change after a refusal");
  await a.shot("join-2-refused");

  // The real token.
  const tok = run("spike", ["tenant-token", "--key", adminKey, "--tenant", "northwind", "--device", id, "--days", "1"]);
  await fill(tok, ORG);
  await a.click("Continue", "#set-relay");
  const conf = await a.text();
  check(conf.includes("Join northwind's relay") && conf.includes("This computer only") && conf.includes("Your name, contacts, messages"), "the confirmation says what joining means");
  await a.shot("join-3-confirm");
  await a.click("Use this relay", "#set-relay"); await sleep(2000);
  const ov = await a.invoke("overview");
  check(ov.relay === ORG, "the organisation's relay is the home relay");
  check(ov.organisation && ov.organisation.tenant === "northwind", "the organisation is shown");
  check((await a.text()).includes("Organisation · northwind"), "Settings shows the organisation");
  check((await a.invoke("test_relay")).startsWith("Connected"), "the app signs in to the organisation's relay");
  for (const theme of ["light", "dark"]) {
    await a.eval(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`); await sleep(200);
    await a.shot(`join-4-joined-${theme}`);
  }
  const usage = JSON.parse(run("spike-relay-admin", ["--relay", ADMIN_ADDR, "--key", opKey, "usage", "northwind"]));
  check(usage[0].devices === 1, "the relay counts one enrolled device");
  const files = readdirSync(join(tmp, "alice"), { recursive: true }).map((f) => join(tmp, "alice", f)).filter((f) => statSync(f).isFile());
  check(files.some((f) => f.endsWith("enrollments.enc")), "the token is kept with the other settings");
  check(!files.some((f) => readFileSync(f).toString("latin1").includes("spike-enroll1:")), "the token is stored sealed");
} catch (e) {
  console.log("FAIL", e.message); D.fail();
} finally {
  await D.stop();
}
console.log(D.failures() ? `${D.failures()} failed` : "all passed");
process.exit(D.failures() ? 1 : 0);
