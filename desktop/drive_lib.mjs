// Shared by the scripts that drive real app windows over the Chrome DevTools protocol (Windows, WebView2).
import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export function createDrive({ relayUrl, shotsDir, pass = "drive test passphrase" }) {
  const root = process.cwd();
  const shots = shotsDir || join(tmpdir(), "spike-shots");
  mkdirSync(shots, { recursive: true });
  const tmp = mkdtempSync(join(tmpdir(), "spike-drive-"));
  const procs = [];
  let failures = 0;
  const check = (ok, what) => { console.log(`${ok ? "ok  " : "FAIL"} ${what}`); if (!ok) failures++; return ok; };

  function start(cmd, args, env) {
    // DRIVE_STDERR=1 shows the apps' own debug output (the listener logs what it does, never content).
    const p = spawn(cmd, args, { env: { ...process.env, ...env }, stdio: process.env.DRIVE_STDERR ? ["ignore", "ignore", "inherit"] : "ignore" });
    procs.push(p);
    return p;
  }

  class Page {
    constructor(name, port) { this.name = name; this.port = port; this.id = 0; this.pending = new Map(); }
    async connect() {
      for (let i = 0; i < 80; i++) {
        try {
          const list = await (await fetch(`http://127.0.0.1:${this.port}/json`)).json();
          const t = list.find((x) => x.type === "page");
          if (t) { this.ws = new WebSocket(t.webSocketDebuggerUrl); break; }
        } catch (_) {}
        await sleep(250);
      }
      if (!this.ws) throw new Error(`${this.name}: no page`);
      await new Promise((res, rej) => { this.ws.onopen = res; this.ws.onerror = rej; });
      this.ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && this.pending.has(d.id)) { this.pending.get(d.id)(d); this.pending.delete(d.id); } };
    }
    send(method, params = {}) { return new Promise((res) => { const id = ++this.id; this.pending.set(id, res); this.ws.send(JSON.stringify({ id, method, params })); }); }
    async eval(expr) {
      const r = await this.send("Runtime.evaluate", { expression: `(async()=>{${expr}})()`, awaitPromise: true, returnByValue: true });
      if (r.result.exceptionDetails) throw new Error(`${this.name}: ${JSON.stringify(r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text)}`);
      return r.result.result.value;
    }
    invoke(cmd, args = {}) { return this.eval(`return await window.__TAURI__.core.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)});`); }
    // Click the first button/element whose text includes `text`, within an optional css scope.
    async click(text, scope = "body") {
      const ok = await this.eval(`const els=[...document.querySelectorAll(${JSON.stringify(scope)} + ' button, ' + ${JSON.stringify(scope)} + ' [role=radio]')].filter(e=>!e.hidden&&e.offsetParent!==null); const e=els.find(x=>(x.innerText||x.getAttribute('aria-label')||'').includes(${JSON.stringify(text)})); if(!e) return false; e.click(); return true;`);
      if (!ok) throw new Error(`${this.name}: no button "${text}"`);
      await sleep(250);
    }
    async text() { return this.eval("return document.body.innerText"); }
    async shot(label) {
      const r = await this.send("Page.captureScreenshot", { format: "png" });
      writeFileSync(join(shots, `${this.name}-${label}.png`), Buffer.from(r.result.data, "base64"));
    }
    async reload() { await this.send("Page.reload"); await sleep(1500); }
    // Redraw from the backend without leaving the screen (a reload would show the unlock screen).
    async refresh() { await this.eval("await refresh(); render();"); await sleep(300); }
  }

  async function launch(name, port) {
    // SPIKE_DESKTOP_EXE: a build made elsewhere (the usual one is locked while the app is running).
    const exe = process.env.SPIKE_DESKTOP_EXE || join(root, "desktop", "src-tauri", "target", "debug", "spike-desktop.exe");
    const home = join(tmp, name);
    start(exe, [], {
      SPIKE_DESKTOP_HOME: home,
      WEBVIEW2_USER_DATA_FOLDER: join(tmp, `${name}-wv`),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port} ${process.env.DRIVE_WV_ARGS || ""}`,
    });
    const p = new Page(name, port);
    await p.connect();
    await sleep(1500);
    await p.invoke("create_identity", { passphrase: pass });
    await p.invoke("set_relay", { url: relayUrl });
    await p.reload();
    // The window always starts at the unlock screen: sign in through it, as a person would.
    await p.eval(`const i=document.querySelector('input'); i.value=${JSON.stringify(pass)}; i.dispatchEvent(new Event('input',{bubbles:true}));`);
    await p.click("Unlock");
    await sleep(2500);
    return p;
  }


  async function stop() {
    for (const p of procs) { try { p.kill(); } catch (_) {} }
    await sleep(500);
    try { rmSync(tmp, { recursive: true, force: true }); } catch (_) {}
  }

  return { root, shots, tmp, procs, start, Page, launch, check, failures: () => failures, fail: () => { failures++; }, stop, pass };
}
