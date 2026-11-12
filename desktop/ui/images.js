/* Pictures in chat: inline previews, a viewer you can zoom and pan, and pasting a picture from the clipboard.
   Image bytes stay in the window's memory only (blob: URLs) and are dropped when the app locks. Nothing is written
   to disk unless you press Save. SVG is not previewed on purpose: it can carry script. */
const IMG_TYPES = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp", bmp: "image/bmp" };
const IMG_AUTO_MAX = 15 * 1024 * 1024; // larger pictures wait for a click before they are fetched
const imgExt = (n) => (String(n).split(".").pop() || "").toLowerCase();
const isImageName = (n) => Object.prototype.hasOwnProperty.call(IMG_TYPES, imgExt(n));
const imgCache = new Map(); // message id -> Promise of a blob: URL

function imageUrl(m) {
  if (!imgCache.has(m.id)) {
    const p = invoke("attachment_bytes", { id: m.id }).then((buf) => {
      const bytes = buf instanceof ArrayBuffer ? buf : new Uint8Array(buf);
      return URL.createObjectURL(new Blob([bytes], { type: IMG_TYPES[imgExt(m.file.name)] }));
    });
    p.catch(() => imgCache.delete(m.id));
    imgCache.set(m.id, p);
  }
  return imgCache.get(m.id);
}
function clearImages() {
  closeViewer(); closePaste();
  for (const p of imgCache.values()) p.then((u) => URL.revokeObjectURL(u), () => {});
  imgCache.clear();
}

/* The preview shown in a message that carries a picture. */
function imageThumb(m) {
  if (m.deleted || !m.file || !isImageName(m.file.name)) return null;
  const box = h("button", { class: "imgthumb", type: "button", "aria-label": `View ${m.file.name}`, title: "View image" });
  let url = null, started = false;
  const load = () => {
    if (started) return; started = true;
    box.textContent = "Loading…";
    const s = $("main").querySelector(".scroll");
    const atEnd = !!s && s.scrollHeight - s.scrollTop - s.clientHeight < 80;
    imageUrl(m).then((u) => {
      url = u;
      box.replaceChildren(h("img", { src: u, alt: m.file.name, draggable: "false" }));
      if (atEnd) scrollEnd();
    }, (e) => { started = false; box.textContent = "Could not load the image. Click to try again."; box.title = String(e); });
  };
  box.addEventListener("click", () => { if (url) openViewer(m, url); else load(); });
  if (m.file.size <= IMG_AUTO_MAX || imgCache.has(m.id)) load();
  else box.textContent = `Show image (${fmtSize(m.file.size)})`;
  return box;
}

/* ---- Viewer: fit, 100%, zoom with the wheel or + and -, drag to move, Esc to close. */
let viewer = null;
function closeViewer() { if (viewer) { viewer.close(); viewer = null; } }
function openViewer(m, url) {
  closeViewer();
  const stage = h("div", { class: "vstage" });
  const img = h("img", { alt: m.file.name, draggable: "false" });
  stage.append(img);
  const label = h("span", { class: "vzoom" }, "");
  const btn = (text, title, fn) => h("button", { class: "btn", type: "button", title, "aria-label": title, onclick: fn }, text);
  let s = 1, x = 0, y = 0, fit = 1, nw = 0, nh = 0;
  const apply = () => { img.style.transform = `translate(${x}px, ${y}px) scale(${s})`; label.textContent = `${Math.round(s * 100)}%`; };
  const size = () => ({ w: stage.clientWidth, h: stage.clientHeight });
  const doFit = () => { const v = size(); fit = Math.min(v.w / nw, v.h / nh, 1); s = fit; x = (v.w - nw * s) / 2; y = (v.h - nh * s) / 2; apply(); };
  const zoomAt = (f, px, py) => {
    const ns = Math.min(8, Math.max(Math.min(fit, 1) * 0.5, s * f));
    x = px - (px - x) * ns / s; y = py - (py - y) * ns / s; s = ns; apply();
  };
  const zoomMid = (f) => { const v = size(); zoomAt(f, v.w / 2, v.h / 2); };
  const actual = () => { const v = size(); zoomAt(1 / s, v.w / 2, v.h / 2); };
  img.addEventListener("load", () => { nw = img.naturalWidth; nh = img.naturalHeight; doFit(); });
  img.src = url;
  stage.addEventListener("wheel", (e) => { e.preventDefault(); const r = stage.getBoundingClientRect(); zoomAt(Math.exp(-e.deltaY * 0.0015), e.clientX - r.left, e.clientY - r.top); }, { passive: false });
  let drag = null;
  stage.addEventListener("pointerdown", (e) => { drag = { px: e.clientX, py: e.clientY, x, y }; stage.setPointerCapture(e.pointerId); stage.classList.add("drag"); });
  stage.addEventListener("pointermove", (e) => { if (drag) { x = drag.x + e.clientX - drag.px; y = drag.y + e.clientY - drag.py; apply(); } });
  const endDrag = () => { drag = null; stage.classList.remove("drag"); };
  stage.addEventListener("pointerup", endDrag); stage.addEventListener("pointercancel", endDrag);
  stage.addEventListener("dblclick", (e) => { const r = stage.getBoundingClientRect(); if (Math.abs(s - fit) < 0.01) zoomAt(1 / s, e.clientX - r.left, e.clientY - r.top); else doFit(); });
  const onKey = (e) => {
    if (e.key === "Escape") { e.preventDefault(); closeViewer(); }
    else if (e.key === "+" || e.key === "=") zoomMid(1.25);
    else if (e.key === "-") zoomMid(0.8);
    else if (e.key === "0") doFit();
    else if (e.key === "1") actual();
  };
  const onResize = () => { if (nw) doFit(); };
  const prevFocus = document.activeElement;
  const ov = h("div", { class: "viewer", role: "dialog", "aria-modal": "true", "aria-label": m.file.name },
    h("div", { class: "vbar" },
      h("span", { class: "vname" }, m.file.name, h("span", { style: "margin-left:8px;opacity:.7" }, fmtSize(m.file.size))),
      btn("−", "Zoom out (-)", () => zoomMid(0.8)), label, btn("+", "Zoom in (+)", () => zoomMid(1.25)),
      btn("Fit", "Fit to window (0)", doFit), btn("100%", "Actual size (1)", actual),
      btn("Save", "Save to Downloads", () => saveFile(m)),
      btn("Close", "Close (Esc)", closeViewer)),
    stage);
  document.body.append(ov);
  document.addEventListener("keydown", onKey, true); window.addEventListener("resize", onResize);
  ov.querySelector(".vbar button:last-child").focus();
  viewer = { close() { document.removeEventListener("keydown", onKey, true); window.removeEventListener("resize", onResize); ov.remove(); if (prevFocus && prevFocus.focus) prevFocus.focus(); } };
}

/* ---- Paste a picture from the clipboard: it is shown first, with a caption box, and sent only when you press Send. */
let pasteDlg = null;
function closePaste() { if (pasteDlg) { pasteDlg.close(); pasteDlg = null; } }
function pastedImage(e) {
  const cd = e.clipboardData; if (!cd) return null;
  if (cd.getData("text/plain")) return null; // text (even with a picture alongside) pastes as text
  for (const f of cd.files || []) if (f.type.startsWith("image/")) return f;
  return null;
}
function openPaste(file) {
  closePaste();
  const group = st.chat ? null : st.group;
  if (group && st.gdetail && st.gdetail.large) return toast("Files are not available in large groups yet.");
  const type = file.type.split("/")[1].split("+")[0] || "png";
  const ext = type === "jpeg" ? "jpg" : type;
  const d = new Date(), p2 = (n) => String(n).padStart(2, "0");
  const named = new File([file], `pasted-image-${d.getFullYear()}${p2(d.getMonth() + 1)}${p2(d.getDate())}-${p2(d.getHours())}${p2(d.getMinutes())}${p2(d.getSeconds())}.${ext}`, { type: file.type });
  const url = URL.createObjectURL(named);
  const caption = h("input", { type: "text", placeholder: "Add a caption (optional)", "aria-label": "Caption", style: "width:100%" });
  const to = st.chat ? st.chat : (st.gdetail && st.gdetail.name) || "the group";
  const send = async () => {
    const text = caption.value; closePaste();
    if (st.chat) await sendFile(st.chat, named, text); else await sendGroupFile(group, named, text);
  };
  const onKey = (e) => { if (e.key === "Escape") { e.preventDefault(); closePaste(); } else if (e.key === "Enter") { e.preventDefault(); send(); } };
  const ov = h("div", { class: "viewer paste", role: "dialog", "aria-modal": "true", "aria-label": "Send pasted image" },
    h("div", { class: "pbox" },
      h("div", { style: "font-weight:600" }, `Send this image to ${to}?`),
      h("div", { class: "pimg" }, h("img", { src: url, alt: "Pasted image", draggable: "false" })),
      h("div", { style: "opacity:.75;font-size:12px" }, `${named.name} · ${fmtSize(named.size)} · encrypted before it leaves this computer`),
      caption,
      h("div", { style: "display:flex;gap:8px;justify-content:flex-end" },
        h("button", { class: "btn", type: "button", onclick: closePaste }, "Cancel"),
        h("button", { class: "btn primary", type: "button", onclick: send }, "Send image"))));
  document.body.append(ov);
  document.addEventListener("keydown", onKey, true);
  caption.focus();
  pasteDlg = { close() { document.removeEventListener("keydown", onKey, true); URL.revokeObjectURL(url); ov.remove(); } };
}
document.addEventListener("paste", (e) => {
  if ($("app").hidden || st.section !== "chats" || st.screen || !(st.chat || st.group) || pasteDlg || viewer) return;
  const f = pastedImage(e); if (!f) return;
  e.preventDefault(); openPaste(f);
});
