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
})();
