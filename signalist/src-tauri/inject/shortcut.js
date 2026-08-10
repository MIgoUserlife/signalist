// Injected into custom shortcut webviews. Reports the page's theme to the
// sidebar and nothing else.
//
// This script deliberately contains NO anti-bot patches. It used to spoof
// `navigator.webdriver`, `window.chrome.runtime`, `navigator.languages` and
// `navigator.plugins` — all shaped for a Chrome UA, while shortcuts actually
// run under a Safari UA (see `safari_user_agent()` in lib.rs). Real Safari has
// no `window.chrome` at all, and the overrides landed as non-native getters on
// the `navigator` instance, so they read as tampering rather than cover for it.
// Cloudflare Turnstile rejected the result ("Verification failed"), which broke
// email login on Linear and every other site behind Turnstile. A plain
// WKWebView with an honest Safari UA passes these checks on its own.
(() => {
  // --- Theme detection ---
  function resolveInvoke() {
    if (window.__TAURI_INTERNALS__ && typeof window.__TAURI_INTERNALS__.invoke === 'function') {
      return window.__TAURI_INTERNALS__.invoke.bind(window.__TAURI_INTERNALS__);
    }
    if (window.__TAURI__) {
      if (window.__TAURI__.core && typeof window.__TAURI__.core.invoke === 'function') {
        return window.__TAURI__.core.invoke.bind(window.__TAURI__.core);
      }
      if (typeof window.__TAURI__.invoke === 'function') {
        return window.__TAURI__.invoke.bind(window.__TAURI__);
      }
    }
    return null;
  }

  function detectAndReportTheme() {
    const invoke = resolveInvoke();
    if (!invoke) return;
    const bg = getComputedStyle(document.documentElement).backgroundColor;
    const m = bg.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
    if (!m) return;
    const lum = (0.299 * +m[1] + 0.587 * +m[2] + 0.114 * +m[3]) / 255;
    const p = invoke('update_sidebar_theme_from_webview', { isDark: lum < 0.5 });
    if (p && typeof p.then === 'function') p.then(function(){}, function(){});
  }

  document.addEventListener('visibilitychange', function() {
    if (!document.hidden) detectAndReportTheme();
  });

  setTimeout(detectAndReportTheme, 2000);

  // --- Fetch patch (Tauri #15216 workaround) ---
  // Same problem the messenger scripts hit, and for the same reason: a strict
  // site CSP (Linear's `connect-src` is a typical one) blocks fetch("ipc://…")
  // at a level that never reaches a JS rejection handler, so Tauri's fetch-first
  // IPC hangs instead of falling back to window.ipc.postMessage. Rejecting those
  // URLs ourselves forces the fallback, which the CSP doesn't touch.
  //
  // Note what this does *not* cover: Tauri prepends plugin init scripts to the
  // ones set on the builder (manager/webview.rs, "Prepend all_initialization_
  // scripts"), and the notification plugin calls `is_permission_granted` inside
  // its own IIFE — so that one call is already in flight before this patch runs
  // and still logs a CSP violation. Tauri falls back on its own there; the patch
  // covers every call after it, ours and the page's alike.
  (function patchFetch() {
    const _origFetch = window.fetch.bind(window);
    const patched = function(url) {
      let urlStr = '';
      try {
        if (typeof url === 'string') urlStr = url;
        else if (url instanceof URL) urlStr = url.href;
        else if (url && typeof url.url === 'string') urlStr = url.url; // Request
      } catch (_e) {}
      if (urlStr.indexOf('ipc://') === 0 || urlStr.indexOf('http://ipc.localhost') === 0) {
        return Promise.reject(new TypeError('[Signalist] fetch(ipc://) forced-reject (Tauri #15216 workaround)'));
      }
      return _origFetch.apply(this, arguments);
    };
    try { window.fetch = patched; } catch (_e) {}
    try { Object.defineProperty(window, 'fetch', { value: patched, writable: true, configurable: true }); } catch (_e) {}
    try { globalThis.fetch = patched; } catch (_e) {}
  })();
})();
