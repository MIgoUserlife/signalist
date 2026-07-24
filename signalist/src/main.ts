import { createApp } from "vue";
import App from "./App.vue";
import "./style.css";
import { invoke } from "@tauri-apps/api/core";

window.onerror = (message, source, lineno, colno, error) => {
  invoke("log_js_error", {
    source: "onerror",
    message: String(message),
    stack: error?.stack ?? `${source}:${lineno}:${colno}`,
  }).catch(() => {});
  return false;
};

window.addEventListener("unhandledrejection", (event) => {
  const reason = event.reason;
  invoke("log_js_error", {
    source: "unhandledrejection",
    message: reason instanceof Error ? reason.message : String(reason),
    stack: reason instanceof Error ? (reason.stack ?? "") : "",
  }).catch(() => {});
});

// У production прибираємо нативне меню WebKit (Reload / Inspect Element / Autofill).
// Поля вводу лишаємо з меню, щоб не втратити Cut/Copy/Paste.
if (!import.meta.env.DEV) {
  document.addEventListener(
    "contextmenu",
    (event) => {
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, [contenteditable='true']")) return;
      event.preventDefault();
    },
    { capture: true },
  );
}

createApp(App).mount("#app");
