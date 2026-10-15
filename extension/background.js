// Downloader browser integration (MV3 service worker).
const HOST = "io.github.stevenke1981.downloader";
const DEFAULTS = {
  intercept: true,
  minSizeKB: 0,
  include: "",
  exclude: "html,htm,php,json,xml,txt,crx",
  altBypass: true,
  fallback: true,
};
const t = (k, a) => chrome.i18n.getMessage(k, a) || k;

let altDownAt = 0;           // last time Alt was seen held
const passThrough = new Set(); // URLs we handed back to the browser

async function options() {
  return { ...DEFAULTS, ...(await chrome.storage.sync.get(DEFAULTS)) };
}

function native(msg) {
  return new Promise((resolve) => {
    try {
      chrome.runtime.sendNativeMessage(HOST, msg, (resp) => {
        if (chrome.runtime.lastError) resolve({ ok: false, error: chrome.runtime.lastError.message });
        else resolve(resp || { ok: false, error: "empty reply" });
      });
    } catch (e) {
      resolve({ ok: false, error: String(e) });
    }
  });
}

async function sendToApp(downloads) {
  const list = downloads.filter((d) => /^https?:\/\//i.test(d.url));
  if (!list.length) return { ok: false, error: "no http(s) URL" };
  return native({ type: "add", downloads: list });
}

function extOf(name) {
  const clean = (name || "").split(/[?#]/)[0];
  const base = clean.split(/[\\/]/).pop() || "";
  const i = base.lastIndexOf(".");
  return i > 0 ? base.slice(i + 1).toLowerCase() : "";
}
const list = (s) => s.split(/[,\s;]+/).map((x) => x.replace(/^\./, "").toLowerCase()).filter(Boolean);

function badge(text, color) {
  chrome.action.setBadgeBackgroundColor({ color });
  chrome.action.setBadgeText({ text });
  setTimeout(() => chrome.action.setBadgeText({ text: "" }), 4000);
}

// ---------------- context menus ----------------
chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: "link", title: t("menuLink"), contexts: ["link"] });
    chrome.contextMenus.create({ id: "image", title: t("menuImage"), contexts: ["image"] });
    chrome.contextMenus.create({ id: "media", title: t("menuMedia"), contexts: ["video", "audio"] });
    chrome.contextMenus.create({ id: "page", title: t("menuPage"), contexts: ["page", "frame"] });
    chrome.contextMenus.create({ id: "all", title: t("menuAllLinks"), contexts: ["page", "frame", "selection", "link"] });
  });
});

async function allLinks(tabId) {
  const res = await chrome.scripting.executeScript({
    target: { tabId, allFrames: true },
    func: () => {
      const out = new Set();
      document.querySelectorAll("a[href]").forEach((a) => out.add(a.href));
      document.querySelectorAll("img[src], video[src], audio[src], source[src]").forEach((e) => out.add(e.src));
      return [...out];
    },
  });
  const urls = new Set();
  for (const r of res || []) for (const u of r.result || []) if (/^https?:\/\//i.test(u)) urls.add(u.split("#")[0]);
  return [...urls];
}

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const referrer = info.pageUrl || tab?.url;
  let downloads = [];
  if (info.menuItemId === "link") downloads = [{ url: info.linkUrl, referrer }];
  else if (info.menuItemId === "image" || info.menuItemId === "media") downloads = [{ url: info.srcUrl, referrer }];
  else if (info.menuItemId === "page") downloads = [{ url: info.frameUrl || info.pageUrl, referrer }];
  else if (info.menuItemId === "all" && tab?.id != null) {
    const urls = await allLinks(tab.id).catch(() => []);
    if (!urls.length) return badge("0", "#888");
    downloads = urls.map((url) => ({ url, referrer }));
  }
  const r = await sendToApp(downloads);
  badge(r.ok ? String(downloads.length) : "!", r.ok ? "#1e88e5" : "#d32f2f");
});

// ---------------- download interception ----------------
chrome.runtime.onMessage.addListener((m, _sender, reply) => {
  if (m?.type === "alt") altDownAt = m.down ? Date.now() : 0;
  if (m?.type === "ping") native({ type: "ping" }).then(reply);
  return m?.type === "ping";
});

chrome.downloads.onCreated.addListener(async (item) => {
  const o = await options();
  const url = item.finalUrl || item.url;
  if (!o.intercept || !/^https?:\/\//i.test(url) || item.state !== "in_progress") return;
  if (passThrough.delete(url)) return;
  if (o.altBypass && altDownAt && Date.now() - altDownAt < 5000) return;
  const size = item.totalBytes > 0 ? item.totalBytes : item.fileSize;
  if (o.minSizeKB > 0 && size > 0 && size < o.minSizeKB * 1024) return;
  const name = item.filename || url;
  const ext = extOf(name) || extOf(url);
  const inc = list(o.include), exc = list(o.exclude);
  if (inc.length && !inc.includes(ext)) return;
  if (ext && exc.includes(ext)) return;

  // Take the download away from the browser.
  try { await chrome.downloads.cancel(item.id); } catch (_) {}
  try { await chrome.downloads.erase({ id: item.id }); } catch (_) {}
  const filename = item.filename ? item.filename.split(/[\\/]/).pop() : undefined;
  const r = await sendToApp([{ url, referrer: item.referrer || undefined, filename }]);
  if (r.ok) {
    badge("1", "#1e88e5");
  } else if (o.fallback) {
    passThrough.add(url);
    chrome.downloads.download({ url });
    badge("!", "#f39c12");
  } else {
    badge("!", "#d32f2f");
  }
});
