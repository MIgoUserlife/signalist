// Shared machinery for every inject script.
//
// This file is not a standalone script: `inject_script!` in lib.rs concatenates
// it with one messenger/shortcut file inside a single outer IIFE, so both halves
// share one lexical scope and nothing is exposed on `window` (an embedded
// third-party page would see any global we created). The per-view file defines
// only what is genuinely specific to it and ends with a `signalistInit({…})`
// call describing which features it wants.

const DEBOUNCE_MS = 300;
const MAX_DELAY_MS = 2000;

// Filled in by signalistInit().
let MESSENGER = null;
let INTERNAL_HOST_RE = null;
let readUnreadCount = null;

let _pendingCount = null;
let _pollTimer = null;
let _debounceTimer = null;
let _lastFireTime = Date.now();
let lastCount = -1;

// The last count handed to the backend. A messenger's getUnreadCount() may need
// it to sanity-check a DOM reading against the previous one.
function lastReportedCount() {
  return lastCount;
}

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

// --- External links ---------------------------------------------------------

function isExternalUrl(parsed) {
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return false;
  if (INTERNAL_HOST_RE.test(parsed.hostname)) return false;
  return parsed.origin !== location.origin;
}

function sendOpenInBrowser(url) {
  (function tryInvoke() {
    const invoke = resolveInvoke();
    if (invoke) {
      invoke('open_in_browser', { url: url }).catch(() => {});
    } else {
      setTimeout(tryInvoke, 100);
    }
  })();
}

function patchWindowOpen() {
  const _open = window.open.bind(window);
  window.open = function (url, target, features) {
    if (typeof url === 'string') {
      try {
        const parsed = new URL(url, location.href);
        if (isExternalUrl(parsed)) {
          sendOpenInBrowser(parsed.href);
          return null;
        }
      } catch (_) {}
    }
    return _open(url, target, features);
  };
}

// WKWebView does not open `<a target="_blank">` links (no `window.open` call,
// no in-frame navigation to hit on_navigation), so external links silently do
// nothing. Intercept clicks on anchors and hand external URLs to the OS browser.
function patchLinkClicks() {
  document.addEventListener(
    'click',
    function (e) {
      if (e.defaultPrevented || e.button !== 0) return;
      // HTMLAnchorElement already exposes protocol/hostname/origin, so it
      // satisfies isExternalUrl() directly — no URL allocation per click.
      const anchor = e.target && e.target.closest && e.target.closest('a[href]');
      if (!anchor || !isExternalUrl(anchor)) return;
      e.preventDefault();
      e.stopPropagation();
      sendOpenInBrowser(anchor.href);
    },
    true,
  );
}

// --- Unread reporting -------------------------------------------------------

// Retries after a failed send by undoing the optimistic `lastCount` update,
// so the next safety-net tick re-reads and re-sends the current count.
// Bounded: a permanent failure (denied ACL, wrong capability label) would
// otherwise turn every 5s tick into a full DOM scan plus a failing invoke,
// forever. The boot race this exists for clears in a tick or two.
const MAX_SEND_RETRIES = 5;
let _failedSends = 0;

function failSend() {
  if (++_failedSends > MAX_SEND_RETRIES) return;
  lastCount = -1;
}

function tryFlush() {
  const invoke = resolveInvoke();
  if (!invoke) return false;
  if (_pendingCount === null) return true;
  const c = _pendingCount;
  _pendingCount = null;
  try {
    const p = invoke('update_unread_count', { messenger: MESSENGER, count: c });
    if (p && typeof p.then === 'function') {
      p.then(
        function () { _failedSends = 0; },
        function (e) {
          // checkAndUpdate() already recorded `c` as sent and the 5s safety
          // net short-circuits on count === lastCount, so without this the
          // badge would stay pinned at its last successful value.
          console.error('[Signalist Inject] invoke FAILED for', MESSENGER, e);
          failSend();
        }
      );
    }
  } catch (e) {
    console.error('[Signalist Inject] Tauri invoke threw:', e);
    failSend();
  }
  return true;
}

function invokeTauri(count) {
  _pendingCount = count; // always overwrite — we only care about the latest value
  if (tryFlush()) return;
  // internals not ready yet — start polling
  if (_pollTimer) return;
  _pollTimer = setInterval(function () {
    if (tryFlush()) {
      clearInterval(_pollTimer);
      _pollTimer = null;
    }
  }, 200);
}

// Leading "(N)" in the document title — the cheapest unread source when a
// messenger puts a message count there.
function getTitleCount() {
  const match = document.title.match(/^\((\d+)\)/);
  if (match) {
    const num = parseInt(match[1], 10);
    return isNaN(num) ? 0 : num;
  }
  return 0;
}

function checkAndUpdate() {
  try {
    const count = readUnreadCount();
    if (count !== lastCount) {
      lastCount = count;
      invokeTauri(count);
    }
  } catch (e) {
    console.error('[Signalist Inject] ' + MESSENGER + ' unread check error:', e);
  }
}

function debouncedCheckAndUpdate() {
  clearTimeout(_debounceTimer);
  const now = Date.now();
  const elapsed = now - _lastFireTime;

  if (elapsed >= MAX_DELAY_MS) {
    // Max delay exceeded — fire immediately
    checkAndUpdate();
    _lastFireTime = Date.now();
  } else {
    const delay = Math.min(DEBOUNCE_MS, MAX_DELAY_MS - elapsed);
    _debounceTimer = setTimeout(() => {
      checkAndUpdate();
      _lastFireTime = Date.now();
    }, delay);
  }
}

// --- Theme ------------------------------------------------------------------

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

// --- Fetch patch (Tauri #15216 workaround) ----------------------------------
// Strict site CSPs (web.telegram.org, web.whatsapp.com, Linear's `connect-src`)
// block fetch("ipc://...") at a level that never propagates into JS rejection
// handlers, so Tauri's fetch-first IPC hangs forever and never falls back to
// window.ipc.postMessage. Intercepting fetch and immediately rejecting ipc://
// URLs forces Tauri's own fallback path (postMessage), which works reliably
// through the CSP.
// Triple-layer patch: window.fetch assignment + Object.defineProperty + globalThis.fetch
//
// Note what this does *not* cover: Tauri prepends plugin init scripts to the
// ones set on the builder (manager/webview.rs, "Prepend all_initialization_
// scripts"), and the notification plugin calls `is_permission_granted` inside
// its own IIFE — so that one call is already in flight before this patch runs
// and still logs a CSP violation. Tauri falls back on its own there; the patch
// covers every call after it, ours and the page's alike.
function patchFetch() {
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
}

// --- Observers --------------------------------------------------------------

function setupObservers(config) {
  // 1. Observe <title> for SPA title changes.
  const titleEl = document.querySelector('title');
  if (titleEl) {
    new MutationObserver(debouncedCheckAndUpdate).observe(titleEl, {
      childList: true,
    });
  }

  // 2. Observe the chat-list container for badge mutations. Attach to the first
  //    selector that matches — these often match overlapping nodes (WhatsApp's
  //    #pane-side contains #side), and attaching to all would create redundant
  //    observers that double-trigger the callback.
  function observeChatList() {
    for (const sel of config.chatListSelectors) {
      const el = document.querySelector(sel);
      if (el) {
        new MutationObserver(debouncedCheckAndUpdate).observe(el, {
          childList: true,
          subtree: true,
          attributes: true,
          attributeFilter: config.chatListAttributeFilter,
          characterData: true,
        });
        console.log('[Signalist Inject] ' + MESSENGER + ' observer attached');
        return true;
      }
    }
    return false;
  }

  if (!observeChatList()) {
    const fallback = new MutationObserver(() => {
      if (observeChatList()) {
        fallback.disconnect();
      }
    });
    fallback.observe(document.body, { childList: true, subtree: true });
  }

  // 3. Observe body class changes (sidebar visibility toggles).
  if (config.observeBodyClass) {
    try {
      new MutationObserver(debouncedCheckAndUpdate).observe(document.body, {
        attributes: true,
        attributeFilter: ['class'],
      });
    } catch (e) {
      /* ignore */
    }
  }

  // 4. Safety net: re-check every 5s regardless of mutations — debounce coalesces rapid calls
  setInterval(debouncedCheckAndUpdate, 5000);
}

function observeThemeChanges() {
  // <html> class/style carries in-app theme switches.
  try {
    new MutationObserver(detectAndReportTheme).observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['class', 'style', 'data-color-scheme'],
    });
  } catch (e) {
    /* ignore */
  }
}

// --- Entry point ------------------------------------------------------------

// config:
//   messenger              — backend messenger label; omit to disable unread reporting
//   getUnreadCount         — () => number, required with `messenger`
//   chatListSelectors      — selectors tried in order for the chat-list observer
//   chatListAttributeFilter— attributes that observer reacts to
//   observeBodyClass       — also watch <body class> (Telegram's sidebar toggles)
//   initialCheckDelayMs    — delay of the first unread read after load
//   internalHostRe         — hosts that stay inside the webview; omit to leave
//                            link handling to the page (custom shortcuts)
//   observeThemeChanges    — watch <html> for in-app theme switches
function signalistInit(config) {
  MESSENGER = config.messenger || null;
  INTERNAL_HOST_RE = config.internalHostRe || null;
  readUnreadCount = config.getUnreadCount || null;

  patchFetch();

  if (INTERNAL_HOST_RE) {
    patchWindowOpen();
    patchLinkClicks();
  }

  document.addEventListener('visibilitychange', function() {
    if (!document.hidden) detectAndReportTheme();
  });

  function start() {
    if (config.observeThemeChanges) observeThemeChanges();
    setTimeout(detectAndReportTheme, 2000);
    if (!MESSENGER) return;
    setupObservers(config);
    setTimeout(checkAndUpdate, config.initialCheckDelayMs);
    setTimeout(debouncedCheckAndUpdate, 3000);
  }

  if (MESSENGER) {
    console.log('[Signalist Inject] ' + MESSENGER + ' fetch() patched (Tauri #15216 workaround)');
    console.log('[Signalist Inject] ' + MESSENGER + '.js loaded, TAURI_INTERNALS at load:', !!window.__TAURI_INTERNALS__);
  }

  // setupObservers() needs document.body; the shortcut path does not, but the
  // 2s theme read is harmless either way.
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', start);
  } else {
    start();
  }
}
