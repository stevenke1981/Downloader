// Downloader browser integration (MV3 service worker).
const HOST = "io.github.stevenke1981.downloader";
const DEFAULTS = {
  intercept: true,
  minSizeKB: 0,
  include: "",
  exclude: "html,htm,php,json,xml,txt,crx",
  altBypass: true,
  fallback: true,
  sendCookies: true,
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

// The app's HTTP library rejects header values outside visible ASCII, which would fail the whole download,
// so such cookies are skipped. A cookie string over MAX_COOKIE_CHARS is dropped entirely, and a batch whose
// cookies together exceed MAX_BATCH_COOKIE_CHARS is sent without cookies (native messaging caps messages at 4 MB).
const COOKIE_TOKEN_RE = /^[\x21-\x7E]*$/;
const MAX_COOKIE_CHARS = 8192;
const MAX_BATCH_COOKIE_CHARS = 3000000;

// Cookie header value for a URL ("k1=v1; k2=v2"), or undefined when there are none or the lookup fails.
async function cookieFor(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url });
    const usable = (cookies || []).filter((c) => COOKIE_TOKEN_RE.test(c.name) && COOKIE_TOKEN_RE.test(c.value));
    if (!usable.length) return undefined;
    const header = usable.map((c) => `${c.name}=${c.value}`).join("; ");
    return header.length > MAX_COOKIE_CHARS ? undefined : header;
  } catch (_) {
    return undefined;
  }
}

const originOf = (u) => {
  try { return new URL(u).origin; } catch (_) { return u; }
};

// Attach each download's cookies (when enabled) so the app can fetch logged-in URLs.
// A single download uses its exact URL; a batch looks cookies up once per origin.
async function withCookies(list) {
  const single = list.length === 1;
  const cache = new Map(); // key -> Promise<cookie string | undefined>
  const attached = await Promise.all(list.map(async (d) => {
    const key = single ? d.url : originOf(d.url);
    if (!cache.has(key)) cache.set(key, cookieFor(single ? d.url : `${key}/`));
    const cookie = await cache.get(key);
    return cookie ? { ...d, cookie } : d;
  }));
  const total = attached.reduce((sum, d) => sum + (d.cookie ? d.cookie.length : 0), 0);
  return total > MAX_BATCH_COOKIE_CHARS ? list : attached;
}

async function sendToApp(downloads) {
  let list = downloads.filter((d) => /^https?:\/\//i.test(d.url));
  if (!list.length) return { ok: false, error: "no http(s) URL" };
  if ((await options()).sendCookies) list = await withCookies(list);
  return native({ type: "add", downloads: list });
}

// Let the browser fetch a URL when the app is unreachable. chrome.downloads.download only accepts a bare
// relative filename, so retry without it if Chrome rejects the name.
async function startBrowserDownload(url, filename) {
  if (filename) {
    try { await chrome.downloads.download({ url, filename }); return; } catch (_) {}
  }
  try { await chrome.downloads.download({ url }); } catch (_) { passThrough.delete(url); }
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

// Links on the page that look like checksum files (sha256sums.txt, x.iso.sha256, ...).
const SUMS_RE = /(\/(sha(1|224|256|384|512)|md5|b2)sums?(\.txt)?|\/checksums?(\.txt|\.sha256)?|\.(sha1|sha256|sha512|md5|b2)(sum)?)$/i;
async function checksumLinks(tabId) {
  if (tabId == null || tabId < 0) return [];
  try {
    const res = await chrome.scripting.executeScript({
      target: { tabId, allFrames: true },
      func: () => [...document.querySelectorAll("a[href]")].map((a) => a.href),
    });
    const out = new Set();
    for (const r of res || []) for (const u of r.result || []) if (SUMS_RE.test(u.split(/[?#]/)[0])) out.add(u.split("#")[0]);
    return [...out].slice(0, 50);
  } catch (_) {
    return [];
  }
}
async function tabForPage(url) {
  if (!url) return null;
  const tabs = await chrome.tabs.query({});
  return tabs.find((t) => t.url === url) || tabs.find((t) => t.active && t.url && url.startsWith(t.url.split("#")[0])) || null;
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
  const sums = await checksumLinks(tab?.id);
  if (sums.length) downloads = downloads.map((d) => ({ ...d, checksum_urls: sums }));
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
  const filename = (item.filename || "").split(/[\\/]/).pop() || undefined;
  const tab = await tabForPage(item.referrer).catch(() => null);
  const checksum_urls = await checksumLinks(tab?.id);
  const r = await sendToApp([{ url, referrer: item.referrer || undefined, filename, checksum_urls }]);
  if (r.ok) {
    badge("1", "#1e88e5");
  } else if (o.fallback) {
    passThrough.add(url);
    await startBrowserDownload(url, filename);
    badge("!", "#f39c12");
  } else {
    badge("!", "#d32f2f");
  }
});
