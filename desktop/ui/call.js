"use strict";
/* Calls (ADR 0028). The backend carries the signalling and decides who rings and who wins; this file
   owns the media: it makes the WebRTC offer and answer, hands the text to the backend, and does what
   the backend says (ring, connect, add the other side's candidates, end).
   Phase 1 is the same network only: no STUN or TURN server is configured, so only local addresses are
   used and nothing leaves the network. */

PATHS.phone = '<path d="M22 16.9v3a2 2 0 0 1-2.2 2 19.8 19.8 0 0 1-8.6-3.1 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.1 4.2 2 2 0 0 1 4.1 2h3a2 2 0 0 1 2 1.7c.1 1 .4 1.9.7 2.8a2 2 0 0 1-.5 2.1L8.1 9.9a16 16 0 0 0 6 6l1.3-1.3a2 2 0 0 1 2.1-.4c.9.3 1.8.6 2.8.7a2 2 0 0 1 1.7 2z"/>';
PATHS.video = '<rect x="2" y="6" width="14" height="12" rx="2"/><path d="M16 10l6-3v10l-6-3z"/>';
PATHS.mic = '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>';
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
  pc.onicecandidate = (e) => {
    if (!e.candidate || !e.candidate.candidate) return;
    cs.localIce.push(e.candidate.candidate);
    clearTimeout(cs.iceTimer);
    cs.iceTimer = setTimeout(flushIce, 50);
  };
  pc.ontrack = (e) => {
    if (!cs.remote) cs.remote = new MediaStream();
    cs.remote.addTrack(e.track);
    const v = callEls.remote; if (v) { v.srcObject = cs.remote; v.play().catch(() => {}); }
    drawCall();
  };
  pc.onconnectionstatechange = () => {
    const s = pc.connectionState;
    if (!cs) return;
    if (s === "connected" && !cs.connectedAt) { cs.connectedAt = Date.now(); cs.status = "connected"; describePath(); drawCall(); }
    if (s === "failed") { const id = cs.id; endLocal("Call failed: the devices could not connect."); invoke("call_fail").catch(() => {}); }
  };
  return pc;
}
async function flushIce() {
  if (!cs || !cs.localIce.length) return;
  const batch = cs.localIce.splice(0, 16);
  try { await cs.ready; await invoke("call_ice", { candidates: batch }); } catch (_) {}
  if (cs && cs.localIce.length) cs.iceTimer = setTimeout(flushIce, 50);
}
async function getMedia(video) {
  try { return await navigator.mediaDevices.getUserMedia({ audio: true, video: video ? { width: 1280, height: 720 } : false }); }
  catch (e) { throw new Error(video ? "Camera or microphone not available: " + (e.name || e) : "Microphone not available: " + (e.name || e) + ". Allow it for Spike Connect and try again."); }
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
function stopMedia() {
  if (!cs) return;
  clearTimeout(cs.iceTimer);
  if (cs.local) cs.local.getTracks().forEach((t) => t.stop());
  if (cs.pc) { cs.pc.onconnectionstatechange = null; cs.pc.close(); }
  stopRing();
}

/* ---------- starting and answering ---------- */
async function startCall(name, video) {
  const c = st.ov.contacts.find((x) => x.name === name);
  cs = { name, video, incoming: false, status: "calling", verified: !!(c && c.verified) };
  drawCall();
  try {
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
  if (message) toast(message);
}

/* ---------- what the backend asks for ---------- */
async function onCallEvent(ev) {
  if (ev.type === "ring") {
    if (cs) return; // the backend only rings when idle; a stale event
    const c = st.ov && st.ov.contacts.find((x) => x.name === ev.name);
    cs = { id: ev.call, name: ev.name, video: ev.video, incoming: true, status: "ringing", offer: ev.sdp, verified: !!(c && c.verified) };
    startRing(); drawCall();
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
  const remote = h("video", { class: "remote", autoplay: "", playsinline: "" });
  const self = h("video", { class: "self", autoplay: "", playsinline: "", muted: "" });
  self.muted = true;
  callEls.remote = remote; callEls.self = self;
  if (c.remote) remote.srcObject = c.remote;
  if (c.local && c.video) self.srcObject = c.local;
  const hasVideo = c.video && c.remote && c.remote.getVideoTracks().length;
  const title = c.status === "ringing" ? `${c.name} is calling` : c.status === "calling" ? `Calling ${c.name}` : c.name;
  const sub = c.status === "ringing" ? (c.video ? "Incoming video call" : "Incoming voice call")
    : c.status === "calling" ? "Ringing on all of their devices"
    : c.connectedAt ? clockOf(Date.now() - c.connectedAt) : "Connecting";
  const facts = [
    h("span", { class: "ok" }, icon("shield"), c.verified ? `${c.name} is a verified contact` : `${c.name} is not verified yet`),
    c.code ? h("span", { class: c.verified ? "" : "warn" }, `Call code ${c.code}${c.verified ? "" : ": read it to each other; it must match"}`) : null,
    c.path ? h("span", {}, c.path) : null,
    h("span", {}, "Encrypted between your devices; the keys are checked against the identity you verified"),
  ];
  const ctl = c.status === "ringing"
    ? [h("button", { class: "end", type: "button", "aria-label": "Decline", onclick: declineCall }, icon("phone")), h("button", { class: "accept", type: "button", "aria-label": "Answer", onclick: answerCall }, icon("phone"))]
    : [
      h("button", { type: "button", "aria-label": "Mute microphone", "aria-pressed": String(!!c.muted), onclick: () => { c.muted = !c.muted; if (c.local) c.local.getAudioTracks().forEach((t) => (t.enabled = !c.muted)); drawCall(); } }, icon(c.muted ? "micoff" : "mic")),
      c.video ? h("button", { type: "button", "aria-label": "Turn camera off", "aria-pressed": String(!!c.camOff), onclick: () => { c.camOff = !c.camOff; if (c.local) c.local.getVideoTracks().forEach((t) => (t.enabled = !c.camOff)); drawCall(); } }, icon("video")) : null,
      h("button", { class: "end", type: "button", "aria-label": "Hang up", onclick: hangUp }, icon("phone")),
    ];
  r.replaceChildren(h("div", { class: "callbox", role: "dialog", "aria-label": "Call with " + c.name },
    h("div", { class: "stage" },
      hasVideo ? remote : [h("span", { class: "big", "aria-hidden": "true" }, ini(c.name)), remote],
      h("h2", {}, title), h("p", { class: "sub", role: "status" }, sub),
      c.video && c.local ? self : null,
      h("div", { class: "facts" }, ...facts)),
    h("div", { class: "controls" }, ...ctl)));
  if (c.connectedAt) drawCall.t = setInterval(() => { const s = r.querySelector(".sub"); if (s && cs) s.textContent = clockOf(Date.now() - cs.connectedAt); }, 1000);
}

/** Settings card: where calls may connect, and whether to hide this computer's address from the other person. */
function callsCard() {
  const note = h("span", { class: "note" }, "Checking whether your relay offers a TURN server…");
  invoke("call_ice_servers").then((s) => {
    note.textContent = s.length
      ? "Your relay offers a TURN server, so calls can connect between different networks. It only forwards encrypted packets."
      : "Your relay has no TURN server: calls work only between devices that can reach each other directly, such as on the same Wi-Fi.";
    if (s.length) note.dataset.turn = "1";
  }).catch(() => { note.textContent = "Could not ask the relay about TURN."; });
  const box = h("input", { type: "checkbox", id: "calls-relay-only" });
  box.checked = relayOnly();
  box.addEventListener("change", () => { try { localStorage.setItem("callsRelayOnly", box.checked ? "1" : "0"); } catch (_) {} toast(box.checked ? "Calls will go through the relay's TURN server when it has one." : "Calls may connect directly."); });
  return h("section", { class: "card", id: "calls-card" }, h("h2", { class: "eyebrow" }, "Calls"), note,
    h("label", { style: "display:flex;gap:10px;align-items:flex-start" }, box, h("span", {}, h("b", { style: "font-weight:600" }, "Always use the relay for calls"), h("span", { class: "note", style: "display:block" }, "The other person then never sees this computer's own address, and your relay's operator sees that a call took place and how much data it carried. Needs a TURN server on the relay."))));
}

window.addEventListener("DOMContentLoaded", () => { callLoop(); });
