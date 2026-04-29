(function () {
  'use strict';

  const MESSENGER = 'whatsapp';
  const DEBOUNCE_MS = 300;
  const MAX_DELAY_MS = 2000;

  let _pendingCount = null;
  let _pollTimer = null;
  let _debounceTimer = null;
  let _lastFireTime = Date.now();
  let lastCount = -1;

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
          function () {},
          function (e) { console.error('[Signalist Inject] invoke FAILED for', MESSENGER, e); }
        );
      }
    } catch (e) {
      console.error('[Signalist Inject] Tauri invoke threw:', e);
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

  function getTitleCount() {
    const match = document.title.match(/^\((\d+)\)/);
    if (match) {
      const num = parseInt(match[1], 10);
      return isNaN(num) ? 0 : num;
    }
    return 0;
  }

  function getDomCount() {
    const container =
      document.querySelector('#pane-side') ||
      document.querySelector('#side') ||
      document.querySelector('[aria-label="Chat list"]');
    if (!container) return 0;

    const seen = new Set();
    let total = 0;
    const rows = container.querySelectorAll('[role="row"]');

    // Strategy 1: aria-label "N unread message(s)" on the row (English WhatsApp)
    rows.forEach(row => {
      if (seen.has(row)) return;
      seen.add(row);
      const label = row.getAttribute('aria-label') || '';
      const match = label.match(/(\d+)\s+unread message/i);
      if (match) {
        total += parseInt(match[1], 10);
      }
    });

    if (total > 0) return total;

    // Strategy 2: visible digit-only badge spans inside chat rows (language-independent)
    rows.forEach(row => {
      for (const span of row.querySelectorAll('span')) {
        const text = (span.textContent || '').trim();
        if (!/^\d+$/.test(text)) continue;
        const num = parseInt(text, 10);
        if (num <= 0 || num > 9999) continue;
        const rect = span.getBoundingClientRect();
        if (rect.width < 1 || rect.height < 1 || rect.width > 60) continue;
        if (seen.has(span)) continue;
        seen.add(span);
        total += num;
        break; // one badge per row
      }
    });

    return total;
  }

  function getUnreadCount() {
    // WhatsApp title shows chat count ("(3) WhatsApp"), not message count.
    // getDomCount() sums aria-label "N unread messages" per row — the correct metric.
    return getDomCount();
  }

  function checkAndUpdate() {
    try {
      const count = getUnreadCount();
      if (count !== lastCount) {
        lastCount = count;
        invokeTauri(count);
      }
    } catch (e) {
      console.error('[Signalist Inject] WhatsApp unread check error:', e);
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

  function setupObservers() {
    // 1. Observe <title> for SPA title changes.
    const titleEl = document.querySelector('title');
    if (titleEl) {
      new MutationObserver(debouncedCheckAndUpdate).observe(titleEl, {
        childList: true,
      });
    }

    // 2. Observe side-panel / chat-list containers.
    function observeChatList() {
      const containers = [
        document.querySelector('#pane-side'),
        document.querySelector('#side'),
        document.querySelector('[aria-label="Chat list"]'),
      ];

      for (const el of containers) {
        if (el) {
          new MutationObserver(debouncedCheckAndUpdate).observe(el, {
            childList: true,
            subtree: true,
            attributes: true,
            attributeFilter: ['aria-label', 'class'],
            characterData: true,
          });
          console.log('[Signalist Inject] whatsapp observer attached');
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

    // Safety net: re-check every 5s regardless of mutations — debounce coalesces rapid calls
    setInterval(debouncedCheckAndUpdate, 5000);
  }

  // --- Fetch patch (Tauri #15216 workaround) ---
  // Strict CSPs on web.whatsapp.com block fetch("ipc://...") at a level that
  // never propagates into JS rejection handlers, so Tauri's fetch-first IPC
  // hangs forever and never falls back to window.ipc.postMessage.
  // Intercepting fetch and immediately rejecting ipc:// URLs forces Tauri's
  // own fallback path (postMessage), which works reliably through the CSP.
  // Triple-layer patch: window.fetch assignment + Object.defineProperty + globalThis.fetch
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
    console.log('[Signalist Inject] ' + MESSENGER + ' fetch() patched (Tauri #15216 workaround)');
  })();

  console.log('[Signalist Inject] ' + MESSENGER + '.js loaded, TAURI_INTERNALS at load:', !!window.__TAURI_INTERNALS__);

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => {
      setupObservers();
      setTimeout(checkAndUpdate, 1000);
      setTimeout(debouncedCheckAndUpdate, 3000);
    });
  } else {
    setupObservers();
    setTimeout(checkAndUpdate, 1000);
    setTimeout(debouncedCheckAndUpdate, 3000);
  }
})();
