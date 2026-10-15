// Tell the background worker when Alt is held, so Alt+click can bypass interception.
(() => {
  let last = false;
  const send = (down) => {
    if (down === last) return;
    last = down;
    try { chrome.runtime.sendMessage({ type: "alt", down }); } catch (_) {}
  };
  addEventListener("keydown", (e) => { if (e.key === "Alt") send(true); }, true);
  addEventListener("keyup", (e) => { if (e.key === "Alt") send(false); }, true);
  addEventListener("blur", () => send(false));
  addEventListener("mousedown", (e) => send(e.altKey), true);
})();
