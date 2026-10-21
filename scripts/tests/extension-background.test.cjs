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

function worker({ cancelFails = false, nativeOk = true, fallback = true, badFilename = false } = {}) {
  const calls = { cancel: [], erase: [], native: [], download: [], cookies: [] };
  let onCreated;
  let onContextMenu;
  const chrome = {
    i18n: { getMessage: (key) => key },
    storage: { sync: { get: async (defaults) => ({ ...defaults, fallback }) } },
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
