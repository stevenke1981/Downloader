// Local regression tests for the actual MV3 service worker. No browser/profile writes.
const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "../../extension/background.js"), "utf8");
const item = {
  id: 7, url: "https://example.test/files/archive.zip", state: "in_progress",
  filename: "C:\\Downloads\\archive.zip", totalBytes: 2048,
  referrer: "https://example.test/downloads",
};

function worker({ cancelFails = false, nativeOk = true, fallback = true, badFilename = false, settings = {} } = {}) {
  const calls = { cancel: [], erase: [], native: [], download: [], cookies: [] };
  let onCreated;
  let onContextMenu;
  const chrome = {
    i18n: { getMessage: (key) => key },
    storage: { sync: { get: async (defaults) => ({ ...defaults, fallback, ...settings }) } },
    cookies: { getAll: async ({ url }) => { calls.cookies.push(url); return [{ name: "sid", value: "abc" }]; } },
    runtime: {
      onInstalled: { addListener() {} }, onMessage: { addListener() {} },
      sendNativeMessage(host, msg, reply) {
        calls.native.push(structuredClone({ host, msg }));
        reply({ ok: nativeOk, error: nativeOk ? undefined : "host unavailable" });
      },
    },
    contextMenus: { onClicked: { addListener(fn) { onContextMenu = fn; } } },
    tabs: { query: async () => [] },
    scripting: { executeScript: async () => [] },
    action: { setBadgeText() {}, setBadgeBackgroundColor() {} },
    downloads: {
      onCreated: { addListener(fn) { onCreated = fn; } },
      async cancel(id) { calls.cancel.push(id); if (cancelFails) throw new Error("already completed"); },
      async erase(query) { calls.erase.push(structuredClone(query)); },
      async download(opts) {
        calls.download.push(structuredClone(opts));
        if (badFilename && opts.filename) throw new Error("invalid filename");
        return 8;
      },
    },
  };
  vm.runInNewContext(source, { chrome, URL, setTimeout() {} }, { filename: "background.js" });
  return { calls, onCreated, onContextMenu };
}

test("failed cancellation leaves browser history and never starts a duplicate", async () => {
  const w = worker({ cancelFails: true });
  await w.onCreated(item);
  assert.deepEqual(w.calls.cancel, [7]);
  assert.deepEqual(w.calls.erase, []);
  assert.deepEqual(w.calls.native, []);
  assert.deepEqual(w.calls.download, []);
});

test("successful interception forwards filename, referrer and login cookie once", async () => {
  const w = worker();
  await w.onCreated(item);
  assert.deepEqual(w.calls.erase, [{ id: 7 }]);
  assert.equal(w.calls.native.length, 1);
  const download = w.calls.native[0].msg.downloads[0];
  assert.equal(download.url, item.url);
  assert.equal(download.filename, "archive.zip");
  assert.equal(download.referrer, item.referrer);
  assert.equal(download.cookie, "sid=abc");
  assert.deepEqual(w.calls.download, []);
});

test("native failure restarts in browser once and bypasses its new onCreated event", async () => {
  const w = worker({ nativeOk: false });
  await w.onCreated(item);
  assert.deepEqual(w.calls.download, [{ url: item.url, filename: "archive.zip" }]);
  await w.onCreated({ ...item, id: 8 });
  assert.deepEqual(w.calls.cancel, [7]);
  assert.equal(w.calls.native.length, 1);
  assert.equal(w.calls.download.length, 1);
});

test("browser fallback retries without a rejected filename", async () => {
  const w = worker({ nativeOk: false, badFilename: true });
  await w.onCreated(item);
  assert.deepEqual(w.calls.download, [
    { url: item.url, filename: "archive.zip" }, { url: item.url },
  ]);
});

test("disabled fallback never restarts a browser transfer", async () => {
  const w = worker({ nativeOk: false, fallback: false });
  await w.onCreated(item);
  assert.deepEqual(w.calls.download, []);
});

test("cookie lookup uses origin and path while forwarding the complete download URL", async () => {
  const w = worker();
  // Exercise the original worker functions in a context-menu batch.
  const single = { menuItemId: "link", linkUrl: item.url + "?download=1", pageUrl: item.referrer };
  await w.onContextMenu(single, { id: 2 });
  assert.deepEqual(w.calls.cookies, [item.url]);
  assert.equal(w.calls.native[0].msg.downloads[0].url, single.linkUrl);
});

const driveItem = {
  ...item,
  url: "https://drive.google.com/uc?export=download&id=public-file&resourcekey=share-key",
  finalUrl: "https://drive.usercontent.google.com/download?id=public-file&confirm=t&uuid=download-token",
  referrer: "https://drive.google.com/file/d/public-file/view",
  filename: "C:\\Downloads\\shared-archive.zip",
  mime: "application/zip",
};

test("Drive login redirect stays in the browser without exporting account cookies", async () => {
  const w = worker();
  await w.onCreated({
    ...driveItem,
    finalUrl: "https://accounts.google.com/v3/signin/identifier?continue=https%3A%2F%2Fdrive.google.com",
    filename: "C:\\Downloads\\identifier",
    mime: "text/html",
  });
  assert.deepEqual(w.calls.cancel, []);
  assert.deepEqual(w.calls.erase, []);
  assert.deepEqual(w.calls.native, []);
  assert.deepEqual(w.calls.cookies, []);
});

test("direct Google account endpoints are never intercepted as files", async () => {
  const w = worker();
  await w.onCreated({
    ...item,
    url: "https://accounts.google.com/ServiceLogin",
    filename: "C:\\Downloads\\ServiceLogin",
  });
  assert.deepEqual(w.calls.cancel, []);
  assert.deepEqual(w.calls.native, []);
  assert.deepEqual(w.calls.cookies, []);
});

test("Drive HTML response without an HTML filename stays in the browser", async () => {
  const w = worker();
  await w.onCreated({
    ...driveItem,
    filename: "C:\\Downloads\\download",
    mime: "text/html; charset=utf-8",
  });
  assert.deepEqual(w.calls.cancel, []);
  assert.deepEqual(w.calls.erase, []);
  assert.deepEqual(w.calls.native, []);
  assert.deepEqual(w.calls.cookies, []);
});

test("resolved Drive media keeps its final URL, matching cookie scope and response filename", async () => {
  const w = worker();
  await w.onCreated(driveItem);
  assert.deepEqual(w.calls.cancel, [7]);
  assert.deepEqual(w.calls.cookies, ["https://drive.usercontent.google.com/download"]);
  const download = w.calls.native[0].msg.downloads[0];
  assert.equal(download.url, driveItem.finalUrl);
  assert.equal(download.filename, "shared-archive.zip");
  assert.equal(download.referrer, driveItem.referrer);
  assert.equal(download.cookie, "sid=abc");
});

test("unresolved Drive downloads keep the original link, resource key and cookie scope", async () => {
  const w = worker();
  const url = "https://drive.google.com/file/d/public-file/view?resourcekey=share-key";
  await w.onCreated({ ...driveItem, url, finalUrl: driveItem.url });
  const download = w.calls.native[0].msg.downloads[0];
  assert.equal(download.url, url);
  assert.deepEqual(w.calls.cookies, ["https://drive.google.com/file/d/public-file/view"]);
  assert.equal(download.filename, "shared-archive.zip");
});

test("explicit HTML filenames on Drive still follow the user's file type options", async () => {
  const w = worker({ settings: { exclude: "", include: "html" } });
  await w.onCreated({ ...driveItem, filename: "C:\\Downloads\\manual.html", mime: "text/html" });
  assert.deepEqual(w.calls.cancel, [7]);
  assert.equal(w.calls.native[0].msg.downloads[0].filename, "manual.html");
});

test("Drive browser fallback uses resolved media and bypasses a redirected retry once", async () => {
  const w = worker({ nativeOk: false });
  await w.onCreated(driveItem);
  assert.deepEqual(w.calls.download, [{ url: driveItem.finalUrl, filename: "shared-archive.zip" }]);
  await w.onCreated({ ...driveItem, id: 8, url: driveItem.finalUrl, finalUrl: driveItem.finalUrl + "&retry=1" });
  assert.deepEqual(w.calls.cancel, [7]);
  assert.equal(w.calls.native.length, 1);
  assert.equal(w.calls.download.length, 1);
});

test("a Drive folder ZIP uses the browser-resolved media URL and its cookies", async () => {
  const w = worker();
  const finalUrl = "https://doc-01-docs.googleusercontent.com/docs/securesc/zip-bundle?e=download";
  await w.onCreated({
    ...driveItem,
    finalUrl,
    referrer: "https://drive.google.com/drive/folders/shared-folder",
    filename: "C:\\Downloads\\folder-archive.zip",
  });
  const download = w.calls.native[0].msg.downloads[0];
  assert.equal(download.url, finalUrl);
  assert.equal(download.filename, "folder-archive.zip");
  assert.equal(download.referrer, "https://drive.google.com/drive/folders/shared-folder");
  assert.deepEqual(w.calls.cookies, ["https://doc-01-docs.googleusercontent.com/docs/securesc/zip-bundle"]);
});

test("browser-generated Drive blob ZIPs remain with the browser", async () => {
  const w = worker();
  const url = "blob:https://drive.google.com/folder-zip";
  await w.onCreated({
    ...driveItem,
    url,
    finalUrl: url,
    referrer: "https://drive.google.com/drive/folders/shared-folder",
    filename: "C:\\Downloads\\folder-archive.zip",
  });
  assert.deepEqual(w.calls.cancel, []);
  assert.deepEqual(w.calls.native, []);
  assert.deepEqual(w.calls.cookies, []);
});

test("ordinary redirects keep using their final media URL and matching cookie scope", async () => {
  const w = worker();
  const finalUrl = "https://cdn.example.test/objects/archive.zip?token=download-token";
  await w.onCreated({ ...item, finalUrl });
  assert.equal(w.calls.native[0].msg.downloads[0].url, finalUrl);
  assert.deepEqual(w.calls.cookies, ["https://cdn.example.test/objects/archive.zip"]);
});
