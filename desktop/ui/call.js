"use strict";
/* Calls (ADR 0028). The backend carries the signalling and decides who rings and who wins; this file
   owns the media: it makes the WebRTC offer and answer, hands the text to the backend, and does what
   the backend says (ring, connect, add the other side's candidates, end).
   Phase 1 is the same network only: no STUN or TURN server is configured, so only local addresses are
   used and nothing leaves the network. */

PATHS.phone = '<path d="M22 16.9v3a2 2 0 0 1-2.2 2 19.8 19.8 0 0 1-8.6-3.1 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.1 4.2 2 2 0 0 1 4.1 2h3a2 2 0 0 1 2 1.7c.1 1 .4 1.9.7 2.8a2 2 0 0 1-.5 2.1L8.1 9.9a16 16 0 0 0 6 6l1.3-1.3a2 2 0 0 1 2.1-.4c.9.3 1.8.6 2.8.7a2 2 0 0 1 1.7 2z"/>';
PATHS.video = '<rect x="2" y="6" width="14" height="12" rx="2"/><path d="M16 10l6-3v10l-6-3z"/>';
PATHS.mic = '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>';
PATHS.screen = '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>';
PATHS.micoff = '<path d="M4 4l16 16"/><path d="M9 9v2a3 3 0 0 0 5 2M15 10V6a3 3 0 0 0-5.5-1.7M5 11a7 7 0 0 0 11 5.7M19 11a7 7 0 0 1-.6 2.8M12 18v3"/>';

let cs = null; // the call on this screen, if any

const callEls = {};
function callRoot() {
  let r = document.getElementById("callov");
  if (!r) { r = document.createElement("div"); r.id = "callov"; r.className = "callov"; r.hidden = true; document.body.appendChild(r); }
  return r;
}

function callButtons(c) {
  if (c.agent) return null;
  const busy = () => { if (cs) { toast("You are already in a call."); return true; } return false; };
  return [
    h("button", { class: "iconbtn", type: "button", "aria-label": "Voice call " + c.name, title: "Voice call", onclick: () => { if (!busy()) startCall(c.name, false); } }, icon("phone")),
    h("button", { class: "iconbtn", type: "button", "aria-label": "Video call " + c.name, title: "Video call", onclick: () => { if (!busy()) startCall(c.name, true); } }, icon("video")),
  ];
}

/* ---------- media ---------- */
/* The relay operator's TURN server, when there is one (ADR 0028): a call can then connect between networks. It only
   forwards encrypted packets. With "Always use the relay" on, no direct address is shared with the other person. */
function relayOnly() { try { return localStorage.getItem("callsRelayOnly") === "1"; } catch (_) { return false; } }
async function iceServers() { try { return await invoke("call_ice_servers"); } catch (_) { return []; } }
function newPeer(servers) {
  const pc = new RTCPeerConnection({ iceServers: servers || [], bundlePolicy: "max-bundle", iceTransportPolicy: relayOnly() && servers && servers.length ? "relay" : "all" });
  cs.pc = pc; cs.pendingIce = []; cs.localIce = []; cs.remoteSet = false;
  // Candidates go to the relay in a few large batches, not one connection per candidate: every relay
  // connection displaces the listener's, so a trickle of small sends made the relay link flap during a call.
  pc.onicecandidate = (e) => {
    if (!cs) return;
    if (!e.candidate || !e.candidate.candidate) { clearTimeout(cs.iceTimer); flushIce(); return; } // gathering done
    cs.localIce.push(e.candidate.candidate);
    clearTimeout(cs.iceTimer);
    cs.iceTimer = setTimeout(flushIce, 400);
  };
  pc.ontrack = (e) => {
    if (!cs.remote) cs.remote = new MediaStream();
    cs.remote.addTrack(e.track);
    const v = callEls.remote; if (v) { toSpeaker(v); v.srcObject = cs.remote; v.play().catch(() => {}); }
    drawCall();
  };
  pc.onconnectionstatechange = () => {
    const s = pc.connectionState;
    if (!cs) return;
    if (s === "connected") {
      clearTimeout(cs.discTimer); clearTimeout(cs.giveUp); cs.restartedAt = 0;
      if (!cs.connectedAt) { cs.connectedAt = Date.now(); cs.status = "connected"; cs.qTimer = setInterval(measure, 2000); }
      cs.status = "connected"; cs.reconnecting = false; describePath(); drawCall();
    }
    // A dropped connection (a network change, a Wi-Fi hiccup) is restarted before the call is given up on.
    if (s === "disconnected" && cs.connectedAt) {
      cs.reconnecting = true; cs.reconnectAt = Date.now(); drawCall();
      clearTimeout(cs.discTimer);
      const me = cs;
      cs.discTimer = setTimeout(() => { if (cs === me && me.pc.connectionState === "disconnected") restartIce(); }, 3000);
    }
    if (s === "failed") {
      if (cs.connectedAt && !cs.restartedAt) restartIce();
      else giveUpCall("Call failed: the devices could not connect.");
    }
  };
  return pc;
}
async function flushIce() {
  if (!cs || !cs.localIce.length) return;
  const batch = cs.localIce.splice(0, 16);
  try { await cs.ready; await invoke("call_ice", { candidates: batch }); } catch (_) {}
  if (cs && cs.localIce.length) cs.iceTimer = setTimeout(flushIce, 50);
}
/* Why the microphone (or camera) was not given to the call, in words the person can act on (defect: calls failed on some Windows machines). */
function mediaProblem(e, video) {
  const n = (e && e.name) || "";
  const what = video ? "The camera or microphone" : "The microphone";
  if (n === "NotFoundError" || n === "OverconstrainedError") return `${what} was not found on this computer. Plug one in, or pick another device in Windows Sound settings.`;
  if (n === "NotReadableError") return `${what} is in use by another program. Close it and try again.`;
  if (n === "NotAllowedError" || n === "SecurityError") return `${what} is blocked. In Windows Settings, go to Privacy & security, then Microphone (and Camera for video), turn on access for desktop apps, then try again.`;
  return `${what} is not available (${n || e}).`;
}
async function getMedia(video) {
  stopMeter(); // the Settings level meter gives the microphone back before a call takes it
  const vid = (cam) => (video ? Object.assign({ width: 1280, height: 720 }, cam ? { deviceId: { exact: cam } } : {}) : false);
  try { return await withChosen((mic, cam) => navigator.mediaDevices.getUserMedia({ audio: mic ? { deviceId: { exact: mic } } : true, video: vid(cam) })); }
  catch (e) { throw new Error(mediaProblem(e, video)); }
}
function fingerprintOf(sdp) { const m = /a=fingerprint:sha-256 ([0-9A-Fa-f:]+)/.exec(sdp || ""); return m ? m[1].toUpperCase() : ""; }
/* Four digits both people can read to each other, from the two DTLS certificate fingerprints. The media
   is already bound to the identities by the signed offer and answer; this catches a problem there. */
async function callCode() {
  const a = fingerprintOf(cs.pc.localDescription && cs.pc.localDescription.sdp), b = fingerprintOf(cs.pc.remoteDescription && cs.pc.remoteDescription.sdp);
  if (!a || !b) return "";
  const buf = await crypto.subtle.digest("SHA-256", new TextEncoder().encode([a, b].sort().join("|")));
  const v = new DataView(buf).getUint32(0);
  return String(v % 10000).padStart(4, "0");
}
/* How the connection is doing, as a word: packet loss over the last two seconds and the round trip. */
async function measure() {
  const c = cs;
  if (!c || !c.pc) return;
  try {
    const stats = await c.pc.getStats();
    let rx = 0, lost = 0, rtt = null;
    stats.forEach((r) => {
      if (r.type === "inbound-rtp" && r.kind === "audio") { rx = r.packetsReceived || 0; lost = r.packetsLost || 0; }
      if (r.type === "candidate-pair" && (r.selected || r.nominated) && r.state === "succeeded" && r.currentRoundTripTime != null) rtt = r.currentRoundTripTime;
    });
    const dRx = Math.max(0, rx - (c.prevRx || 0)), dLost = Math.max(0, lost - (c.prevLost || 0));
    c.prevRx = rx; c.prevLost = lost;
    // Sound arriving again is the end of "Reconnecting", whether or not the connection ever reported a drop.
    // And no sound for four seconds is the start of it, even before the connection reports a drop.
    c.stall = dRx > 0 ? 0 : (c.stall || 0) + 1;
    if (c.stall >= 2 && !c.reconnecting) { c.reconnecting = true; c.reconnectAt = Date.now(); }
    else if (c.reconnecting && dRx > 0 && c.pc.connectionState === "connected" && Date.now() - (c.reconnectAt || 0) > 1500) c.reconnecting = false;
    if (c.reconnecting) { showSub(); return; }
    const loss = dRx + dLost > 0 ? dLost / (dRx + dLost) : 0;
    c.quality = loss > 0.10 || (rtt != null && rtt > 0.6) ? "poor" : loss > 0.03 || (rtt != null && rtt > 0.3) ? "fair" : "good";
    c.qualityNote = `${rtt != null ? Math.round(rtt * 1000) + " ms round trip, " : ""}${Math.round(loss * 100)}% of packets lost`;
    if (cs === c) showSub();
  } catch (_) {}
}
const QUALITY = { good: "Good connection", fair: "Fair connection", poor: "Poor connection" };
/* The line under the name once connected: the time, then how the connection is doing. */
function connLine(c) {
  if (c.reconnecting) return "Reconnecting…";
  return clockOf(Date.now() - c.connectedAt) + (c.quality ? "  ·  " + QUALITY[c.quality] : "");
}
function showSub() {
  const s = callRoot().querySelector(".sub");
  if (!s || !cs || !cs.connectedAt) return;
  s.textContent = connLine(cs);
  s.title = cs.qualityNote || "";
  s.classList.toggle("warn", !!cs.reconnecting || cs.quality === "poor");
}
async function describePath() {
  try {
    const stats = await cs.pc.getStats();
    let pair = null; stats.forEach((r) => { if (r.type === "candidate-pair" && (r.selected || r.nominated) && r.state === "succeeded") pair = r; });
    if (!pair) return;
    const loc = stats.get(pair.localCandidateId);
    cs.path = loc && loc.candidateType === "relay" ? "Through a relay" : "Direct, on this network";
    drawCall();
  } catch (_) {}
}
function giveUpCall(message) { invoke("call_fail").catch(() => {}); endLocal(message); }

/* After a network change the connection is restarted on the same call (ADR 0028). Only the person who placed the call
   makes the new offer; the other side asks for one. If it does not come back within 20 seconds the call ends. */
async function restartIce() {
  if (!cs || !cs.pc || !cs.connectedAt) return;
  const me = cs;
  clearTimeout(cs.giveUp);
  cs.restartedAt = Date.now();
  cs.reconnecting = true; cs.reconnectAt = Date.now(); drawCall();
  cs.giveUp = setTimeout(() => { if (cs === me && me.pc.connectionState !== "connected") giveUpCall("Call lost: the connection could not be restored."); }, 20000);
  try {
    // fresh TURN login, in case the call has outlived the old one
    const servers = await iceServers();
    if (servers.length) { const c = cs.pc.getConfiguration(); c.iceServers = servers; cs.pc.setConfiguration(c); }
    if (cs.incoming) { await invoke("call_restart_ask"); return; }
    cs.pc.restartIce();
    const offer = await cs.pc.createOffer({ iceRestart: true });
    await cs.pc.setLocalDescription(offer);
    await invoke("call_restart", { sdp: offer.sdp });
  } catch (e) { if (cs === me) giveUpCall("Call lost: " + (e.message || e)); }
}
window.addEventListener("online", () => { if (cs && cs.connectedAt) restartIce(); });
if (navigator.connection && navigator.connection.addEventListener) navigator.connection.addEventListener("change", () => { if (cs && cs.connectedAt) restartIce(); });

/* Sharing the screen in a video call (ADR 0028): the picture of the screen takes the place of the camera's on the same
   connection, so nothing has to be negotiated again. Stopping (here, or with the browser's own "stop sharing" bar)
   puts the camera back. Voice calls have no picture to swap: start a video call to share. */
const videoSender = () => cs && cs.pc && cs.pc.getSenders().find((s) => s.track && s.track.kind === "video");
async function toggleShare() {
  if (!cs || !cs.pc || !cs.video || !cs.connectedAt) return;
  if (cs.sharing) { await stopShare(); return; }
  const me = cs, sender = videoSender();
  if (!sender) return;
  let display;
  try { display = await navigator.mediaDevices.getDisplayMedia({ video: { frameRate: 15 }, audio: false }); }
  catch (_) { toast("Screen sharing was not allowed."); return; }
  if (cs !== me) { display.getTracks().forEach((t) => t.stop()); return; }
  const track = display.getVideoTracks()[0];
  try { track.contentHint = "detail"; } catch (_) {}
  me.camTrack = sender.track;
  try { await sender.replaceTrack(track); } catch (e) { track.stop(); toast("Could not share the screen."); return; }
  me.shareStream = display; me.sharing = true;
  track.addEventListener("ended", () => { if (cs === me) stopShare(); });
  drawCall();
}
async function stopShare() {
  const me = cs;
  if (!me || !me.sharing) return;
  me.sharing = false;
  const sender = videoSender();
  if (sender && me.camTrack && me.camTrack.readyState === "live") { try { await sender.replaceTrack(me.camTrack); } catch (_) {} }
  if (me.shareStream) me.shareStream.getTracks().forEach((t) => t.stop());
  me.shareStream = null;
  if (cs === me) drawCall();
}

function stopMedia() {
  if (!cs) return;
  clearTimeout(cs.iceTimer); clearTimeout(cs.discTimer); clearTimeout(cs.giveUp); clearInterval(cs.qTimer);
  if (cs.local) cs.local.getTracks().forEach((t) => t.stop());
  if (cs.shareStream) cs.shareStream.getTracks().forEach((t) => t.stop());
  if (cs.pc) { cs.pc.onconnectionstatechange = null; cs.pc.close(); }
  stopRing();
}

/* ---------- starting and answering ---------- */
async function startCall(name, video) {
  const c = st.ov.contacts.find((x) => x.name === name);
  cs = { name, video, incoming: false, status: "calling", verified: !!(c && c.verified) };
  drawCall();
  try {
    await invoke("set_media_access", { on: true }).catch(() => {});
    cs.local = await getMedia(video);
    const pc = newPeer(await iceServers());
    cs.local.getTracks().forEach((t) => pc.addTrack(t, cs.local));
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    const me = cs;
    cs.ready = invoke("call_start", { name, video, sdp: offer.sdp }).then((id) => { if (cs === me) cs.id = id; return id; });
    await cs.ready;
  } catch (e) { endLocal(String(e.message || e)); return; }
  drawCall();
}
async function answerCall() {
  if (!cs || !cs.incoming || cs.status !== "ringing") return;
  stopRing();
  cs.status = "connecting"; drawCall();
  try {
    await invoke("set_media_access", { on: true }).catch(() => {});
    cs.local = await getMedia(cs.video);
    const pc = newPeer(await iceServers());
    cs.local.getTracks().forEach((t) => pc.addTrack(t, cs.local));
    await pc.setRemoteDescription({ type: "offer", sdp: cs.offer });
    cs.remoteSet = true;
    const answer = await pc.createAnswer();
    await pc.setLocalDescription(answer);
    const me = cs;
    cs.ready = invoke("call_accept", { sdp: answer.sdp }).then(() => { if (cs === me) cs.ready = Promise.resolve(); });
    await cs.ready;
    cs.code = await callCode(); drawCall();
  } catch (e) {
    const msg = String(e.message || e);
    invoke("call_decline", { all: false }).catch(() => {});
    endLocal(msg);
  }
}
function declineCall() { if (!cs) return; invoke("call_decline", { all: false }).catch(() => {}); endLocal(null); }
function hangUp() { if (!cs) return; invoke("call_hangup").catch(() => {}); endLocal(cs.connectedAt ? "Call ended" : null); }
function endLocal(message) {
  if (!cs) return;
  stopMedia(); cs = null; drawCall();
  invoke("set_media_access", { on: false }).catch(() => {});
  if (message) toast(message);
}

/* ---------- what the backend asks for ---------- */
async function onCallEvent(ev) {
  if (ev.type === "ring") {
    if (cs) return; // the backend only rings when idle; a stale event
    // "Ring on this computer" off: the call rings only on the person's other devices; a missed call is still told when
    // it ends. Two people calling each other at once (glare) is answered as usual.
    if (!ringHere() && !ev.glare) return;
    const c = st.ov && st.ov.contacts.find((x) => x.name === ev.name);
    cs = { id: ev.call, name: ev.name, video: ev.video, incoming: true, status: "ringing", offer: ev.sdp, verified: !!(c && c.verified) };
    startRing(); drawCall();
    if (!document.hasFocus() && bannerMode() !== "off")
      showBanner(bannerMode() === "who" ? `${ev.name} is calling.` : `Incoming ${ev.video ? "video" : "voice"} call.`);
    if (ev.glare) answerCall();
    return;
  }
  if (ev.type === "ended") {
    if (!cs || (cs.id && cs.id !== ev.call)) { if (!cs && ev.incoming && ev.result === "missed") toast(`Missed call from ${ev.name || "someone"}.`); return; }
    const who = ev.name || cs.name, secs = ev.seconds;
    const m = { completed: `Call ended (${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}).`, missed: `Missed call from ${who}.`, declined: `${who} declined the call.`, no_answer: `${who} did not answer.`, busy: `${who} is busy.`, answered_elsewhere: "Answered on another of your devices.", failed: "The call failed." }[ev.result] || "Call ended.";
    endLocal(m); return;
  }
  if (!cs || (cs.id && cs.id !== ev.call)) return;
  if (ev.type === "stop_ringing") { stopRing(); return; }
  if (ev.type === "connect") {
    try { cs.id = ev.call; await cs.pc.setRemoteDescription({ type: "answer", sdp: ev.sdp }); cs.remoteSet = true; cs.status = "connecting"; cs.code = await callCode(); for (const c of cs.pendingIce.splice(0)) await addRemote(c); drawCall(); }
    catch (e) { invoke("call_fail").catch(() => {}); endLocal("The other device's answer could not be used."); }
    return;
  }
  if (ev.type === "restart_ask") { if (!cs.incoming) restartIce(); return; }
  if (ev.type === "restart") {
    try {
      await cs.pc.setRemoteDescription({ type: "offer", sdp: ev.sdp });
      const answer = await cs.pc.createAnswer();
      await cs.pc.setLocalDescription(answer);
      await invoke("call_restart_answer", { sdp: answer.sdp });
    } catch (e) { giveUpCall("Call lost: " + (e.message || e)); }
    return;
  }
  if (ev.type === "restart_answer") {
    try { await cs.pc.setRemoteDescription({ type: "answer", sdp: ev.sdp }); } catch (e) { giveUpCall("Call lost: " + (e.message || e)); }
    return;
  }
  if (ev.type === "remote_ice") {
    for (const c of ev.candidates) { if (cs.remoteSet) await addRemote(c); else cs.pendingIce.push(c); }
  }
}
async function addRemote(c) { try { await cs.pc.addIceCandidate({ candidate: c, sdpMLineIndex: 0 }); } catch (_) {} }

/* The backend holds the call rules; this loop just waits for the next thing to do. */
async function callLoop() {
  for (;;) {
    if (!st.ov) { await new Promise((r) => setTimeout(r, 700)); continue; }
    try { for (const ev of await invoke("call_wait", { ms: 15000 })) await onCallEvent(ev); }
    catch (_) { await new Promise((r) => setTimeout(r, 1500)); }
  }
}

/* ---------- ringing ---------- */
let ringTimer = null, ringCtx = null;
function startRing() {
  stopRing();
  try {
    ringCtx = new (window.AudioContext || window.webkitAudioContext)();
    toSpeaker(ringCtx);
    const beep = () => { const o = ringCtx.createOscillator(), g = ringCtx.createGain(); o.frequency.value = 440; g.gain.value = 0.05; o.connect(g); g.connect(ringCtx.destination); o.start(); o.stop(ringCtx.currentTime + 0.4); };
    beep(); ringTimer = setInterval(beep, 2000);
  } catch (_) {}
}
function stopRing() { clearInterval(ringTimer); ringTimer = null; if (ringCtx) { try { ringCtx.close(); } catch (_) {} ringCtx = null; } }

/* ---------- the call screen ---------- */
function clockOf(ms) { const s = Math.floor(ms / 1000); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`; }
function drawCall() {
  const r = callRoot();
  clearInterval(drawCall.t);
  if (!cs) { r.hidden = true; r.replaceChildren(); callEls.remote = callEls.self = null; return; }
  r.hidden = false;
  const c = cs;
  // Ringing, as in the design: a small card over the chat (who, how they are calling, Decline and Answer), not the
  // whole window. The sentence for screen readers stays "<name> is calling".
  r.classList.toggle("ringing", c.status === "ringing");
  if (c.status === "ringing") {
    callEls.remote = callEls.self = null;
    r.replaceChildren(h("div", { class: "ringcard", role: "alertdialog", "aria-label": `${c.name} is calling` },
      h("span", { class: "sr" }, `${c.name} is calling. `),
      h("div", { class: "who" },
        h("span", { class: "tile", "aria-hidden": "true" }, ini(c.name)),
        h("div", {},
          h("b", {}, c.name, c.verified ? h("span", { class: "vt", title: "Verified" }, icon("shield")) : null),
          h("span", { class: "sub" }, c.video ? "Incoming video call" : "Incoming voice call"),
          c.verified ? null : h("span", { class: "nv" }, "Not verified yet"))),
      h("div", { class: "do" },
        h("button", { class: "btn decline", type: "button", "aria-label": "Decline", onclick: declineCall }, "Decline"),
        h("button", { class: "btn primary", type: "button", "aria-label": "Answer", onclick: answerCall }, "Answer"))));
    return;
  }
  const remote = h("video", { class: "remote", autoplay: "", playsinline: "" });
  const self = h("video", { class: "self", autoplay: "", playsinline: "", muted: "" });
  self.muted = true;
  callEls.remote = remote; callEls.self = self;
  toSpeaker(remote);
  if (c.remote) remote.srcObject = c.remote;
  if (c.local && c.video) self.srcObject = c.sharing && c.shareStream ? c.shareStream : c.local;
  if (c.sharing) self.classList.add("sharing");
  const hasVideo = c.video && c.remote && c.remote.getVideoTracks().length;
  const title = c.status === "ringing" ? `${c.name} is calling` : c.status === "calling" ? `Calling ${c.name}` : c.name;
  const sub = c.status === "ringing" ? (c.video ? "Incoming video call" : "Incoming voice call")
    : c.status === "calling" ? "Ringing on all of their devices"
    : c.connectedAt ? connLine(c) : "Connecting";
  const facts = [
    c.verified ? h("span", { class: "ok" }, icon("shield"), `${c.name} is a verified contact`) : h("span", { class: "warn" }, `${c.name} is not verified yet`),
    c.code ? h("span", { class: c.verified ? "" : "warn" }, `Call code ${c.code}${c.verified ? "" : ": read it to each other; it must match"}`) : null,
    c.path ? h("span", {}, c.path) : null,
    c.sharing ? h("span", { class: "warn" }, "You are sharing your screen") : null,
    h("span", {}, "Encrypted between your devices; the keys are checked against the identity you verified"),
  ];
  const ctl = c.status === "ringing"
    ? [h("button", { class: "end", type: "button", "aria-label": "Decline", onclick: declineCall }, icon("phone")), h("button", { class: "accept", type: "button", "aria-label": "Answer", onclick: answerCall }, icon("phone"))]
    : [
      h("button", { type: "button", "aria-label": "Mute microphone", "aria-pressed": String(!!c.muted), onclick: () => { c.muted = !c.muted; if (c.local) c.local.getAudioTracks().forEach((t) => (t.enabled = !c.muted)); drawCall(); } }, icon(c.muted ? "micoff" : "mic")),
      c.video ? h("button", { type: "button", "aria-label": "Turn camera off", "aria-pressed": String(!!c.camOff), onclick: () => { c.camOff = !c.camOff; if (c.local) c.local.getVideoTracks().forEach((t) => (t.enabled = !c.camOff)); drawCall(); } }, icon("video")) : null,
      c.video && c.connectedAt ? h("button", { type: "button", "aria-label": c.sharing ? "Stop sharing screen" : "Share screen", "aria-pressed": String(!!c.sharing), onclick: toggleShare }, icon("screen")) : null,
      h("button", { class: "end", type: "button", "aria-label": "Hang up", onclick: hangUp }, icon("phone")),
    ];
  r.replaceChildren(h("div", { class: "callbox", role: "dialog", "aria-label": "Call with " + c.name },
    h("div", { class: "stage" },
      hasVideo ? remote : [h("span", { class: `big ${tint(c.name)}`, "aria-hidden": "true" }, ini(c.name)), remote],
      h("h2", {}, title), h("p", { class: "sub", role: "status" }, sub),
      c.video && c.local ? self : null,
      h("div", { class: "facts" }, ...facts)),
    h("div", { class: "controls" }, ...ctl)));
  if (c.connectedAt) { drawCall.t = setInterval(showSub, 1000); showSub(); }
}

/* ---------- devices: microphone, speaker and camera ---------- */
/* The choice is kept on this computer only (localStorage), by the browser's device id. An empty choice is the system
   default. A chosen device that is unplugged later is asked for as "ideal", so the call falls back to the default. */
const DEV_KEY = { mic: "callMic", spk: "callSpeaker", cam: "callCam" };
function chosen(kind) { try { return localStorage.getItem(DEV_KEY[kind]) || ""; } catch (_) { return ""; } }
function choose(kind, id) { try { localStorage.setItem(DEV_KEY[kind], id || ""); } catch (_) {} }
/* Ask for the chosen microphone and camera exactly; if one of them is gone (unplugged), ask again for the default. */
async function withChosen(get) {
  const mic = chosen("mic"), cam = chosen("cam");
  try { return await get(mic, cam); }
  catch (e) {
    if (!(mic || cam) || (e && e.name !== "OverconstrainedError" && e.name !== "NotFoundError")) throw e;
    return await get("", "");
  }
}
function ringHere() { try { return localStorage.getItem("callsRing") !== "0"; } catch (_) { return true; } }
/* Send an element's (or an AudioContext's) sound to the chosen speaker. Not every webview has setSinkId; then the
   system default plays it. */
function toSpeaker(el) {
  const id = chosen("spk");
  if (!el || !id || typeof el.setSinkId !== "function") return Promise.resolve();
  return el.setSinkId(id).catch(() => {});
}

// What the lists show. Labels are only given to a page that may use the microphone, so the lists are read after the
// level meter has opened it.
const devs = { list: null, problem: null, loading: false };
async function loadDevices() {
  if (devs.loading) return;
  devs.loading = true;
  try {
    if (!navigator.mediaDevices || !navigator.mediaDevices.enumerateDevices) { devs.list = []; devs.problem = "This computer offers no microphone, speaker or camera to the app."; return; }
    await startMeter();
    devs.list = await navigator.mediaDevices.enumerateDevices();
  } catch (_) { devs.list = devs.list || []; }
  finally { devs.loading = false; if (document.getElementById("calls-card")) render(); }
}
if (navigator.mediaDevices && navigator.mediaDevices.addEventListener)
  navigator.mediaDevices.addEventListener("devicechange", () => { if (document.getElementById("calls-card")) { devs.list = null; render(); } });

/* The live microphone level while Settings > Calls is on screen. The microphone is let go as soon as the section is
   left (checked every frame) or a call takes it. */
const meter = { stream: null, ctx: null, raf: 0, el: null, gone: 0, mic: null };
async function startMeter() {
  if (cs) return; // a call has the microphone
  if (meter.stream && meter.mic === chosen("mic")) return;
  stopMeter();
  const mic = chosen("mic");
  meter.mic = mic;
  await invoke("set_media_access", { on: true }).catch(() => {});
  let stream;
  try { stream = await withChosen((m) => navigator.mediaDevices.getUserMedia({ audio: m ? { deviceId: { exact: m } } : true, video: false })); }
  catch (e) {
    if (!cs) invoke("set_media_access", { on: false }).catch(() => {});
    devs.problem = mediaProblem(e, false);
    return;
  }
  if (cs || !document.getElementById("calls-card")) { stream.getTracks().forEach((t) => t.stop()); if (!cs) invoke("set_media_access", { on: false }).catch(() => {}); return; }
  devs.problem = null;
  meter.stream = stream;
  try {
    meter.ctx = new (window.AudioContext || window.webkitAudioContext)();
    const an = meter.ctx.createAnalyser(); an.fftSize = 512;
    meter.ctx.createMediaStreamSource(stream).connect(an);
    const buf = new Uint8Array(an.fftSize);
    const tick = () => {
      if (!meter.stream) return;
      if (!meter.el || !meter.el.isConnected) { if (++meter.gone > 20) { stopMeter(); return; } }
      else {
        meter.gone = 0;
        an.getByteTimeDomainData(buf);
        let sum = 0; for (const v of buf) { const x = (v - 128) / 128; sum += x * x; }
        const level = Math.min(1, Math.sqrt(sum / buf.length) * 4);
        meter.el.firstChild.style.width = Math.round(level * 100) + "%";
        meter.el.setAttribute("aria-valuenow", String(Math.round(level * 100)));
      }
      meter.raf = requestAnimationFrame(tick);
    };
    meter.raf = requestAnimationFrame(tick);
  } catch (_) {}
}
function stopMeter() {
  cancelAnimationFrame(meter.raf);
  const had = !!meter.stream;
  if (meter.stream) meter.stream.getTracks().forEach((t) => t.stop());
  if (meter.ctx) { try { meter.ctx.close(); } catch (_) {} }
  meter.stream = meter.ctx = null; meter.gone = 0; meter.mic = null;
  if (had && !cs) invoke("set_media_access", { on: false }).catch(() => {});
}

/* A short two-note chime on the chosen speaker. */
async function testSpeaker() {
  try {
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    await toSpeaker(ctx);
    [660, 880].forEach((f, i) => {
      const o = ctx.createOscillator(), g = ctx.createGain(), t = ctx.currentTime + i * 0.35;
      o.frequency.value = f; g.gain.setValueAtTime(0.0001, t); g.gain.exponentialRampToValueAtTime(0.15, t + 0.03); g.gain.exponentialRampToValueAtTime(0.0001, t + 0.3);
      o.connect(g); g.connect(ctx.destination); o.start(t); o.stop(t + 0.32);
    });
    setTimeout(() => { try { ctx.close(); } catch (_) {} }, 1200);
  } catch (_) { toast("The test sound could not be played."); }
}

/** Settings card, as in the design: microphone (with its level), speaker (with a test sound), camera, whether calls go
    through the relay, and whether this computer rings. */
function callsCard() {
  const note = h("span", { class: "note", style: "display:block" }, "Checking whether your relay offers a TURN server…");
  invoke("call_ice_servers").then((s) => {
    note.textContent = s.length
      ? "Your relay offers a TURN server, so calls can connect between different networks. It only forwards encrypted packets."
      : "Your relay has no TURN server: calls work only between devices that can reach each other directly, such as on the same Wi-Fi.";
    if (s.length) note.dataset.turn = "1";
  }).catch(() => { note.textContent = "Could not ask the relay about TURN."; });

  if (!devs.list && !devs.loading) loadDevices();
  else if (devs.list && !meter.stream && !cs) startMeter();
  const of = (kind) => (devs.list || []).filter((d) => d.kind === kind && d.deviceId !== "default" && d.deviceId !== "communications");
  const NAMES = { audioinput: "Microphone", audiooutput: "Speaker", videoinput: "Camera" };
  const pickFor = (kind, key, label) => {
    const list = of(kind);
    if (devs.list && !list.length) return h("span", { class: "note", id: `calls-no-${key}` }, kind === "audiooutput" ? "The system's default speaker" : `No ${NAMES[kind].toLowerCase()} found`);
    const cur = chosen(key);
    const s = h("select", { class: "ssel", id: `calls-${key}`, "aria-label": label, disabled: devs.list ? null : "",
      onchange: (e) => { choose(key, e.target.value); if (key === "mic") { stopMeter(); startMeter(); } if (key === "spk" && cs && callEls.remote) toSpeaker(callEls.remote); toast(`${NAMES[kind]}: ${e.target.selectedOptions[0].text}.`); } },
      h("option", { value: "" }, devs.list ? "System default" : "Loading…"),
      list.map((d, i) => h("option", { value: d.deviceId, selected: d.deviceId === cur || null }, d.label || `${NAMES[kind]} ${i + 1}`)));
    return s;
  };
  const bar = h("span", { class: "meter", id: "calls-meter", role: "meter", "aria-label": "Microphone level", "aria-valuemin": "0", "aria-valuemax": "100", "aria-valuenow": "0" }, h("i", { style: "width:0%" }));
  meter.el = bar;
  const canSink = typeof HTMLMediaElement !== "undefined" && "setSinkId" in HTMLMediaElement.prototype;

  const box = h("input", { type: "checkbox", id: "calls-relay-only" });
  box.checked = relayOnly();
  box.addEventListener("change", () => { try { localStorage.setItem("callsRelayOnly", box.checked ? "1" : "0"); } catch (_) {} toast(box.checked ? "Calls will go through the relay's TURN server when it has one." : "Calls may connect directly."); });
  const ring = h("input", { type: "checkbox", id: "calls-ring" });
  ring.checked = ringHere();
  ring.addEventListener("change", () => { try { localStorage.setItem("callsRing", ring.checked ? "1" : "0"); } catch (_) {} toast(ring.checked ? "Calls ring on this computer." : "Calls ring only on your other devices. A missed call is still shown here."); });

  return h("section", { class: "card", id: "calls-card" },
    ...secHead("Calls", "The camera and microphone are used only while a call is on screen, and the microphone while this section is open, to show its level."),
    devs.problem ? h("p", { class: "status soft", id: "calls-problem", role: "status" }, devs.problem) : null,
    srow("Microphone", null, devs.problem ? null : bar, pickFor("audioinput", "mic", "Microphone")),
    srow("Speaker", canSink ? null : "This computer plays calls on its default speaker.",
      h("button", { class: "btn", type: "button", id: "calls-test", onclick: testSpeaker }, "Test"),
      canSink ? pickFor("audiooutput", "spk", "Speaker") : null),
    srow("Camera", null, pickFor("videoinput", "cam", "Camera")),
    h("label", { class: "toggle" }, box, h("span", {}, h("b", {}, "Always call through the relay"), h("span", { class: "note", style: "display:block" }, "Hides your address from the other person. The relay then sees both addresses. Needs a TURN server on the relay."), note)),
    h("label", { class: "toggle" }, ring, h("span", {}, h("b", {}, "Ring on this computer"), h("span", { class: "note", style: "display:block" }, "Your other devices ring too; the first to answer silences the rest. Off: calls ring only on your other devices."))));
}

window.addEventListener("DOMContentLoaded", () => { callLoop(); });
