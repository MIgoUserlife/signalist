(() => {
  try {
    Object.defineProperty(navigator, 'webdriver', {
      get: () => undefined,
      configurable: true,
    });
  } catch (_) {}

  try {
    if (!window.chrome) window.chrome = {};
    if (!window.chrome.runtime) {
      window.chrome.runtime = {
        PlatformOs: { MAC: 'mac' },
        PlatformArch: { X86_64: 'x86-64' },
        id: undefined,
      };
    }
  } catch (_) {}

  try {
    if ((navigator.languages || []).length < 2) {
      Object.defineProperty(navigator, 'languages', {
        get: () => ['en-US', 'en'],
        configurable: true,
      });
    }
  } catch (_) {}

  try {
    if (!navigator.plugins || navigator.plugins.length === 0) {
      const fakePlugins = [
        { name: 'PDF Viewer' },
        { name: 'Chrome PDF Viewer' },
        { name: 'WebKit built-in PDF' },
      ];
      Object.defineProperty(navigator, 'plugins', {
        get: () => fakePlugins,
        configurable: true,
      });
    }
  } catch (_) {}

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
