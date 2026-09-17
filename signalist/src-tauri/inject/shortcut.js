// Shortcut-specific half of the inject script, concatenated after
// `inject/common.js` inside one IIFE by `inject_script!` in lib.rs. A custom
// shortcut reports the page's theme and fixes external file drags — no unread
// counts, and no link interception: a shortcut is a whole site, so its links
// stay inside the webview.
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

// WKWebView on macOS hides an external drag's DataTransfer metadata until
// `drop`. GitHub, GitLab and many drop-zone libraries inspect `types` during
// `dragenter`/`dragover`; when it is empty they reject the drag before the
// later event can expose its files. Preserve real metadata when WebKit
// provides it, and advertise the only relevant missing type while hovering.
function exposeExternalFileDrag(event) {
  const transfer = event.dataTransfer;
  if (!transfer || transfer.types.length || transfer.files.length) return;
  try {
    Object.defineProperty(transfer, 'types', {
      configurable: true,
      value: ['Files'],
    });
  } catch (_e) {}
}

document.addEventListener('dragenter', exposeExternalFileDrag, true);
document.addEventListener('dragover', exposeExternalFileDrag, true);

signalistInit({
  observeThemeChanges: false,
});
