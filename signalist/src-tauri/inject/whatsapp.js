// WhatsApp-specific half of the inject script. Concatenated after
// `inject/common.js` inside one IIFE by `inject_script!` in lib.rs — everything
// shared (IPC, fetch patch, external links, debounce, theme) lives there.

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
      if (seen.has(span)) continue;
      const rect = span.getBoundingClientRect();
      if (rect.width < 1 || rect.height < 1 || rect.width > 60) continue;
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

signalistInit({
  messenger: 'whatsapp',
  getUnreadCount: getUnreadCount,
  // Hosts that belong to WhatsApp itself — navigation to these stays inside the webview.
  internalHostRe: /(^|\.)(whatsapp\.com|whatsapp\.net)$/i,
  chatListSelectors: ['#pane-side', '#side', '[aria-label="Chat list"]'],
  chatListAttributeFilter: ['aria-label', 'class'],
  observeBodyClass: false,
  initialCheckDelayMs: 1000,
  observeThemeChanges: true,
});
