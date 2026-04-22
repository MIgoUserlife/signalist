(function () {
  'use strict';

  const MESSENGER = 'telegram';
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
    const selectors = [
      '.chat-list-item .badge',
      '.chat-list-item .DialogBadge',
      '.chat-list-item [class*="Badge"]',
      '.chat-list-item [class*="badge"]',
      '.chat-list-item .rp',
      '.chat-list [class*="unread"]',
      '.Transition_slide-active [class*="badge"]',
      '.Transition_slide-active [class*="Badge"]',
      '.sidebar [class*="badge"]',
      '.sidebar [class*="Badge"]',
    ];

    for (const selector of selectors) {
      const nodes = document.querySelectorAll(selector);
      for (const node of nodes) {
        const text = (node.textContent || '').trim();
        if (/^\d+$/.test(text)) {
          const num = parseInt(text, 10);
          if (!isNaN(num) && num > 0) {
            total += num;
          }
        }
      }
    }
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
      console.error('[Signalist Inject] Telegram unread check error:', e);
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

    // 2. Observe chat-list containers for badge mutations.
    const chatListSelectors = [
      '.chat-list',
      '.Transition_slide-active.left-column',
      '.Transition_slide-active',
      '.sidebar',
      '[class*="chat-list"]',
    ];

    function observeChatList() {
      for (const sel of chatListSelectors) {
        const el = document.querySelector(sel);
        if (el) {
          new MutationObserver(checkAndUpdate).observe(el, {
            childList: true,
            subtree: true,
            attributes: true,
            attributeFilter: ['class', 'data-id'],
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

    // 3. Observe body class changes (sidebar visibility toggles).
    try {
      new MutationObserver(checkAndUpdate).observe(document.body, {
        attributes: true,
        attributeFilter: ['class'],
      });
    } catch (e) {
      /* ignore */
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => {
      setupObservers();
      setTimeout(checkAndUpdate, 500);
    });
  } else {
    setupObservers();
    setTimeout(checkAndUpdate, 500);
  }
})();
