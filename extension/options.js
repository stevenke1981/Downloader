const DEFAULTS = { intercept: true, minSizeKB: 0, include: "", exclude: "html,htm,php,json,xml,txt,crx", altBypass: true, fallback: true };
const t = (k, a) => chrome.i18n.getMessage(k, a) || k;
document.querySelectorAll("[data-i18n]").forEach((e) => (e.textContent = t(e.dataset.i18n)));
const status = (s) => (document.getElementById("status").textContent = s);

chrome.storage.sync.get(DEFAULTS, (o) => {
  for (const [k, v] of Object.entries(o)) {
    const el = document.getElementById(k);
    if (!el) continue;
    if (el.type === "checkbox") el.checked = v; else el.value = v;
  }
});

document.getElementById("save").onclick = () => {
  const o = {};
  for (const k of Object.keys(DEFAULTS)) {
    const el = document.getElementById(k);
    o[k] = el.type === "checkbox" ? el.checked : el.type === "number" ? Math.max(0, Number(el.value) || 0) : el.value.trim();
  }
  chrome.storage.sync.set(o, () => status(t("optSaved")));
};

document.getElementById("check").onclick = () => {
  chrome.runtime.sendMessage({ type: "ping" }, (r) => {
    if (r && r.ok) status(t("statusOk", [r.version || ""]));
    else status(t("statusErr", [(r && r.error) || chrome.runtime.lastError?.message || "?"]));
  });
};
