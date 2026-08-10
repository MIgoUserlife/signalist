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
})();
