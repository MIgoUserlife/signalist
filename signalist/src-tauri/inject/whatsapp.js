(function () {
  'use strict';

  const MESSENGER = 'whatsapp';
  let lastCount = -1;

  function invokeTauri(count) {
    if (typeof window.__TAURI__ !== 'undefined' && window.__TAURI__.invoke) {
      try {
        window.__TAURI__.invoke('update_unread_count', {
          messenger: MESSENGER,
          count: count,
        });
      } catch (e) {
        console.error('[Signalist Inject] Tauri invoke failed:', e);
      }
    }
  }

  function getTitleCount() {
    const match = document.title.match(/^\(([\d\s,]+)\)\s*[^\)]*$/);
    if (match) {
      const num = parseInt(match[1].replace(/[\s,]/g, ''), 10);
      return isNaN(num) ? 0 : num;
    }
    return 0;
  }

  function getDomCount() {
    let total = 0;
    const rows = document.querySelectorAll(
      '#pane-side [role="row"], ' +
        '#side [role="row"], ' +
        '[aria-label="Chat list"] [role="row"], ' +
        '#side [aria-label="Chat list"] > div > div'
    );

    rows.forEach((row) => {
      const label = row.getAttribute('aria-label') || '';
      const match = label.match(/(\d+)\s+unread message/);
      if (match) {
        total += parseInt(match[1], 10);
      }
    });

    return total;
  }

  function getUnreadCount() {
    const titleCount = getTitleCount();
    if (titleCount > 0) return titleCount;
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

  function setupObservers() {
    // 1. Observe <title> for SPA title changes.
    const titleEl = document.querySelector('title');
    if (titleEl) {
      new MutationObserver(checkAndUpdate).observe(titleEl, {
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
          new MutationObserver(checkAndUpdate).observe(el, {
            childList: true,
            subtree: true,
            attributes: true,
            attributeFilter: ['aria-label', 'class'],
          });
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
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => {
      setupObservers();
      setTimeout(checkAndUpdate, 1000);
    });
  } else {
    setupObservers();
    setTimeout(checkAndUpdate, 1000);
  }
})();
