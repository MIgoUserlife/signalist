// Telegram-specific half of the inject script. Concatenated after
// `inject/common.js` inside one IIFE by `inject_script!` in lib.rs — everything
// shared (IPC, fetch patch, external links, debounce, theme) lives there.

function getDomCount() {
  const seen = new Set();
  let total = 0;
  const selectors = [
    '.ListItem.Chat .chat-badge-transition.shown span',
    '.chat-list .chat-badge-transition.shown span',
    '.chat-list-item .badge',
  ];

  for (const selector of selectors) {
    const nodes = document.querySelectorAll(selector);
    for (const node of nodes) {
      if (seen.has(node)) continue;
      seen.add(node);
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
  const domCount = getDomCount();
  // Guard: if DOM count is many times higher than the last reported count while
  // the title is 0, it's a muted-badge artifact during a title transition, not
  // a genuine increase.
  const lastCount = lastReportedCount();
  if (lastCount > 0 && domCount > lastCount * 4) return 0;
  return domCount;
}

signalistInit({
  messenger: 'telegram',
  getUnreadCount: getUnreadCount,
  // Hosts that belong to Telegram itself — navigation to these stays inside the webview.
  internalHostRe: /(^|\.)(telegram\.org|t\.me)$/i,
  chatListSelectors: [
    '#LeftColumn',             // Telegram Web A - stable ID
    '.chat-list',              // Current active chat-list
    '.left-column',
    '.Transition_slide-active',
    '.sidebar',
  ],
  chatListAttributeFilter: ['class', 'data-id', 'aria-label'],
  observeBodyClass: true,
  initialCheckDelayMs: 500,
  observeThemeChanges: true,
});
