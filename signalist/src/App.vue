<script setup lang="ts">
import { ref, reactive, computed, watch, onMounted, onUnmounted, type Ref, type CSSProperties } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { isPermissionGranted, requestPermission, onAction } from "@tauri-apps/plugin-notification";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import logoIcon from './assets/logo.svg?raw';
import telegramIcon from './assets/icons/telegram.svg?raw';
import whatsappIcon from './assets/icons/whatsapp.svg?raw';
import { MESSENGER_CATALOG, type CatalogEntry } from './messengerCatalog';

const _allIconsRaw = import.meta.glob('./assets/icons/*.svg', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;
const iconMap: Record<string, string> = Object.fromEntries(
  Object.entries(_allIconsRaw)
    .map(([path, svg]) => [path.replace('./assets/icons/', '').replace('.svg', ''), svg])
    .filter(([key]) => key !== 'telegram' && key !== 'whatsapp')
);

const _params = new URLSearchParams(window.location.search);
const _view = _params.get("view");
const isDialogView = _view === "add-shortcut";
const isEditDialogView = _view === "edit-shortcut";
const isAddMessengerView = _view === "add-messenger";
const isBugReportView = _view === "bug-report";
const isConfirmDeleteView = _view === "confirm-delete";
const editShortcutId = _params.get("id") ?? "";
const confirmDeleteId = _params.get("id") ?? "";
const confirmDeleteKind = _params.get("kind") === "messenger" ? "messenger" : "shortcut";

// True only when running via `npm run tauri:dev` (Vite dev server); false in
// production builds. Drives the DEV badge that flags this as the dev instance.
const isDev = import.meta.env.DEV;

if (!isDialogView && !isEditDialogView && !isAddMessengerView && !isBugReportView && !isConfirmDeleteView) {
  document.documentElement.classList.add('sidebar-view');
}

// Defer window.close() to the next tick. Synchronous close() from a JS event
// handler crashes WKWebView on macOS 26 in WebPageProxy::dispatchSetObscuredContentInsets
// because a runloop work item still references the now-freed page proxy.
function closeDialogWindow() {
  setTimeout(() => { getCurrentWebviewWindow().close(); }, 0);
}

// ── Shared state (used by both views) ──────────────────────────────────────
interface CustomShortcut {
  id: string;
  name: string;
  url: string;
  icon?: string;
  color?: string;
}

interface UserMessenger {
  id: string;
  name: string;
  url: string;
  icon?: string;
  preload?: boolean;
}

const newShortcutName = ref("");
const newShortcutUrl = ref("");
const addError = ref("");
const isAdding = ref(false);
const newShortcutIcon = ref<string | null>(null);
const newShortcutColor = ref<string | null>(null);

// Mid-tone hues, picked to stay legible on both the dark (#1e1b18) and the
// light (#f2efe8) surface. Every icon SVG paints with currentColor, so tinting
// the button is enough. `null` means "follow the theme accent".
const SHORTCUT_COLORS = [
  { name: 'Red',    value: '#e05561' },
  { name: 'Orange', value: '#e08c3c' },
  { name: 'Yellow', value: '#d4a72c' },
  { name: 'Green',  value: '#4caf50' },
  { name: 'Teal',   value: '#2bb3a3' },
  { name: 'Blue',   value: '#4a9eff' },
  { name: 'Purple', value: '#a06cd5' },
  { name: 'Pink',   value: '#e0669e' },
] as const;

const QUICK_PRESETS = [
  { name: 'Claude',        url: 'https://claude.ai/new',    icon: 'claude'      },
  { name: 'ChatGPT',       url: 'https://chatgpt.com/',     icon: 'openai'      },
  { name: 'Claude Design', url: 'https://claude.ai/design', icon: 'color_lens'  },
] as const;

function applyPreset(p: typeof QUICK_PRESETS[number]) {
  newShortcutName.value = p.name;
  newShortcutUrl.value  = p.url;
  newShortcutIcon.value = p.icon;
}

async function submitShortcut(mode: "add" | "edit") {
  addError.value = "";
  const name = newShortcutName.value.trim();
  let url = newShortcutUrl.value.trim();
  if (!name) { addError.value = "Name is required"; return; }
  if (!url) { addError.value = "URL is required"; return; }
  if (!/^https?:\/\//i.test(url)) url = "https://" + url;
  isAdding.value = true;
  try {
    const color = newShortcutColor.value ?? null;
    if (mode === "add") {
      await invoke<CustomShortcut>("add_custom_shortcut", { name, url, icon: newShortcutIcon.value ?? null, color });
    } else {
      await invoke<CustomShortcut>("update_custom_shortcut", { id: editShortcutId, name, url, icon: newShortcutIcon.value ?? null, color });
    }
    closeDialogWindow();
  } catch (e) {
    addError.value = String(e);
  } finally {
    isAdding.value = false;
  }
}

// ── Sidebar-only state ──────────────────────────────────────────────────────
const activeMessenger = ref("telegram");

interface MessengerConfig {
  label: string;
  displayName: string;
  icon: string;
}

const messengers: MessengerConfig[] = [
  { label: "telegram", displayName: "Telegram", icon: telegramIcon },
  { label: "whatsapp", displayName: "WhatsApp", icon: whatsappIcon },
];

const unreadCounts = reactive<Record<string, number>>(
  Object.fromEntries(messengers.map(m => [m.label, 0]))
);

const currentHotkey = ref("Super+Shift+S");
const isRecordingHotkey = ref(false);
const hotkeyError = ref("");

const autostartEnabled = ref(false);
const silenceEnabled = ref(false);
const settingsOpen = ref(false);
const settingsWrapper = ref<HTMLElement | null>(null);

// ── Theme ──────────────────────────────────────────────────────────────────
type ThemeMode = 'system' | 'light' | 'dark' | 'auto';
const THEME_MODES: ThemeMode[] = ['system', 'light', 'dark', 'auto'];
const themeMode = ref<ThemeMode>(
  (localStorage.getItem('themeMode') as ThemeMode | null) ?? 'system'
);
const autoIsDark = ref(true);
const systemIsDark = ref(window.matchMedia('(prefers-color-scheme: dark)').matches);

const resolvedTheme = computed(() => {
  if (themeMode.value === 'light') return 'light';
  if (themeMode.value === 'dark') return 'dark';
  if (themeMode.value === 'system') return systemIsDark.value ? 'dark' : 'light';
  return autoIsDark.value ? 'dark' : 'light';
});

const themeModeLabel = computed(() => {
  switch (themeMode.value) {
    case 'light': return 'LITE';
    case 'dark': return 'DARK';
    case 'auto':  return 'AUTO';
    default:      return 'SYS';
  }
});

function cycleTheme() {
  const i = THEME_MODES.indexOf(themeMode.value);
  themeMode.value = THEME_MODES[(i + 1) % THEME_MODES.length];
}

watch(resolvedTheme, (t) => {
  document.documentElement.setAttribute('data-theme', t);
}, { immediate: true });

watch(themeMode, (m) => {
  localStorage.setItem('themeMode', m);
});

function onDocumentClick(e: MouseEvent) {
  if (settingsWrapper.value && !settingsWrapper.value.contains(e.target as Node)) {
    settingsOpen.value = false;
  }
}

watch(settingsOpen, (open) => {
  if (open) {
    document.addEventListener("click", onDocumentClick);
  } else {
    document.removeEventListener("click", onDocumentClick);
  }
});

const appVersion = ref("");
const updateAvailable = ref(false);
const updateVersion = ref("");
const isInstalling = ref(false);
let cachedUpdate: Awaited<ReturnType<typeof check>> | null = null;

const customShortcuts = ref<CustomShortcut[]>([]);
const userMessengers = ref<UserMessenger[]>([]);
const isAddingMessenger = ref(false);

function shortcutInitial(name: string): string {
  return name.trim().charAt(0).toUpperCase() || "?";
}

function shortcutLabel(id: string): string {
  return `custom-${id}`;
}

function formatHotkeyDisplay(hotkey: string): string {
  return hotkey
    .split("+")
    .map((part) => {
      switch (part) {
        case "Super": return "⌘";
        case "Shift": return "⇧";
        case "Alt": return "⌥";
        case "Control": return "⌃";
        case "Space": return "␣";
        default: return part;
      }
    })
    .join("");
}

function captureHotkey(e: KeyboardEvent) {
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape") {
    isRecordingHotkey.value = false;
    return;
  }
  const mods: string[] = [];
  if (e.metaKey) mods.push("Super");
  if (e.ctrlKey) mods.push("Control");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (["Meta", "Control", "Alt", "Shift"].includes(e.key)) {
    window.addEventListener("keydown", captureHotkey, { once: true });
    return;
  }
  if (mods.length === 0) {
    isRecordingHotkey.value = false;
    return;
  }
  const key = e.key === " " ? "Space" : e.key.length === 1 ? e.key.toUpperCase() : e.key;
  saveHotkey([...mods, key].join("+"));
}

function startRecordingHotkey() {
  isRecordingHotkey.value = true;
  hotkeyError.value = "";
  window.addEventListener("keydown", captureHotkey, { once: true });
}

async function installUpdate() {
  isInstalling.value = true;
  try {
    if (cachedUpdate?.available) {
      await cachedUpdate.downloadAndInstall();
      await relaunch();
    }
  } catch (e) {
    console.error("Update install failed:", e);
    isInstalling.value = false;
  }
}

async function toggleBoolSetting(command: string, state: Ref<boolean>, label: string) {
  const next = !state.value;
  try {
    await invoke(command, { enable: next });
    state.value = next;
  } catch (e) {
    console.error(`Failed to toggle ${label}:`, e);
  }
}

const toggleAutostart = () => toggleBoolSetting("set_autostart", autostartEnabled, "autostart");
const toggleSilence   = () => toggleBoolSetting("set_silence_mode", silenceEnabled, "silence mode");

async function saveHotkey(shortcut: string) {
  isRecordingHotkey.value = false;
  try {
    await invoke("set_global_shortcut", { shortcut });
    currentHotkey.value = shortcut;
  } catch {
    hotkeyError.value = "Failed to register";
    setTimeout(() => { hotkeyError.value = ""; }, 2000);
  }
}

async function loadCustomShortcuts() {
  try {
    customShortcuts.value = await invoke<CustomShortcut[]>("list_custom_shortcuts");
  } catch (e) {
    console.warn("Failed to load custom shortcuts:", e);
  }
}

async function loadUserMessengers() {
  try {
    userMessengers.value = await invoke<UserMessenger[]>("list_user_messengers");
  } catch (e) {
    console.warn("Failed to load user messengers:", e);
  }
}

async function openUserMessenger(m: UserMessenger) {
  await invoke("open_custom_shortcut", { id: m.id, url: m.url });
  activeMessenger.value = shortcutLabel(m.id);
}

async function switchToUserMessenger(m: UserMessenger) {
  activeMessenger.value = shortcutLabel(m.id);
  try {
    await invoke("switch_messenger", { messenger: shortcutLabel(m.id) });
  } catch {
    await openUserMessenger(m);
  }
}

// Removal is destructive and the × sits on the corner of a 40px icon, so it is
// far too easy to hit by accident. Both × buttons only open the confirmation
// window; the actual removal runs here once it reports back.
function askRemoveUserMessenger(id: string) {
  invoke("open_confirm_delete_window", { kind: "messenger", id })
    .catch(e => console.error("Failed to open delete confirmation:", e));
}

function askRemoveShortcut(id: string) {
  invoke("open_confirm_delete_window", { kind: "shortcut", id })
    .catch(e => console.error("Failed to open delete confirmation:", e));
}

async function removeUserMessenger(id: string) {
  await invoke("remove_user_messenger", { id });
  userMessengers.value = userMessengers.value.filter(m => m.id !== id);
  if (activeMessenger.value === shortcutLabel(id)) {
    await openMessenger("telegram");
  }
}

function isMessengerAdded(entry: CatalogEntry): boolean {
  return userMessengers.value.some(m => m.url === entry.url);
}

async function addFromCatalog(entry: CatalogEntry) {
  isAddingMessenger.value = true;
  addError.value = "";
  try {
    const m = await invoke<UserMessenger>("add_user_messenger", {
      name: entry.name,
      url: entry.url,
      icon: entry.icon,
      preload: newMessengerPreload.value,
    });
    await emit("messenger-added", m);
    // The window stays open so the freshly added messenger shows up in the
    // startup list below and its checkbox can still be flipped.
    userMessengers.value.push(m);
  } catch (e) {
    addError.value = String(e);
  } finally {
    isAddingMessenger.value = false;
  }
}

// ── Startup preloading ──────────────────────────────────────────────────────
// A messenger that was never opened has no webview, so its inject script never
// runs and no unread counts or notifications arrive until the first click.
// Preloading creates the webview hidden at launch; it costs one background
// WebContent process each, hence the opt-in per messenger.
const builtinPreload = ref<Record<string, boolean>>({});
const newMessengerPreload = ref(false);

async function loadBuiltinPreload() {
  try {
    builtinPreload.value = await invoke<Record<string, boolean>>("get_builtin_preload");
  } catch (e) {
    console.warn("Failed to load preload settings:", e);
  }
}

async function toggleBuiltinPreload(label: string) {
  const next = !builtinPreload.value[label];
  try {
    await invoke("set_builtin_preload", { messenger: label, enable: next });
    builtinPreload.value[label] = next;
  } catch (e) {
    addError.value = String(e);
  }
}

async function toggleUserPreload(m: UserMessenger) {
  const next = !m.preload;
  try {
    await invoke("set_user_messenger_preload", { id: m.id, enable: next });
    m.preload = next;
  } catch (e) {
    addError.value = String(e);
  }
}

async function openCustomShortcut(sc: CustomShortcut) {
  await invoke("open_custom_shortcut", { id: sc.id, url: sc.url });
  activeMessenger.value = shortcutLabel(sc.id);
}

async function switchToCustom(sc: CustomShortcut) {
  activeMessenger.value = shortcutLabel(sc.id);
  try {
    await invoke("switch_messenger", { messenger: shortcutLabel(sc.id) });
  } catch {
    await openCustomShortcut(sc);
  }
}

// ── Drag-to-reorder (custom shortcuts zone only) ────────────────────────────
// Pointer events, not the HTML5 drag-and-drop API: the sidebar webview keeps
// Tauri's OS-level drag-drop handler enabled, and that handler swallows `drop`
// before it reaches the page — the same cause as the messenger file-drop fix.
const SHORTCUT_STEP = 44; // h-10 button (40px) + gap-1 (4px)
const DRAG_THRESHOLD = 4; // dead zone so a sloppy click still opens the shortcut

const dragIndex = ref<number | null>(null);
const dragTargetIndex = ref<number | null>(null);
const dragOffset = ref(0);
let dragStartY = 0;
let dragMoved = false;
let suppressClick = false;

function onShortcutPointerDown(e: PointerEvent, index: number) {
  suppressClick = false;
  if (e.button !== 0 || customShortcuts.value.length < 2) return;
  dragStartY = e.clientY;
  dragMoved = false;
  dragIndex.value = index;
  dragTargetIndex.value = index;
  dragOffset.value = 0;
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
}

function onShortcutPointerMove(e: PointerEvent) {
  if (dragIndex.value === null) return;
  const dy = e.clientY - dragStartY;
  if (!dragMoved && Math.abs(dy) < DRAG_THRESHOLD) return;
  dragMoved = true;
  dragOffset.value = dy;
  const last = customShortcuts.value.length - 1;
  const target = dragIndex.value + Math.round(dy / SHORTCUT_STEP);
  dragTargetIndex.value = Math.min(last, Math.max(0, target));
}

async function onShortcutPointerUp(e: PointerEvent) {
  const from = dragIndex.value;
  const to = dragTargetIndex.value;
  const el = e.currentTarget as HTMLElement;
  if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);

  // Cleared before the await so the transition classes drop in the same render
  // as the transforms — otherwise the settled items animate from a stale offset.
  dragIndex.value = null;
  dragTargetIndex.value = null;
  dragOffset.value = 0;
  if (!dragMoved) return;
  dragMoved = false;
  suppressClick = true; // the click that follows this pointerup is not a real click

  if (from === null || to === null || from === to) return;
  const previous = customShortcuts.value;
  const list = [...previous];
  const [moved] = list.splice(from, 1);
  list.splice(to, 0, moved);
  customShortcuts.value = list;
  try {
    await invoke("reorder_custom_shortcuts", { ids: list.map(sc => sc.id) });
  } catch (err) {
    console.error("Failed to reorder shortcuts:", err);
    customShortcuts.value = previous;
  }
}

function shortcutDragStyle(index: number): CSSProperties | undefined {
  const from = dragIndex.value;
  const to = dragTargetIndex.value;
  if (from === null || to === null) return undefined;
  if (index === from) {
    return { transform: `translateY(${dragOffset.value}px) scale(1.08)`, transition: "none" };
  }
  // Items between the grabbed slot and the drop slot slide one step to open the gap
  if (from < to && index > from && index <= to) return { transform: `translateY(${-SHORTCUT_STEP}px)` };
  if (from > to && index < from && index >= to) return { transform: `translateY(${SHORTCUT_STEP}px)` };
  return undefined;
}

function onShortcutClick(sc: CustomShortcut) {
  if (suppressClick) {
    suppressClick = false;
    return;
  }
  switchToCustom(sc);
}

async function removeShortcut(id: string) {
  await invoke("remove_custom_shortcut", { id });
  customShortcuts.value = customShortcuts.value.filter(sc => sc.id !== id);
  if (activeMessenger.value === shortcutLabel(id)) {
    await openMessenger("telegram");
  }
}

// ── Confirm-delete dialog view ─────────────────────────────────────────────
// Runs in its own window. It never deletes anything itself: it looks the target
// up by id purely to name it in the prompt, then hands the decision back to the
// sidebar, which owns both the list state and the active-view fallback.
const confirmDeleteName = ref("");
const confirmDeleteMissing = ref(false);
const confirmDeleteBusy = ref(false);

async function loadConfirmDeleteTarget() {
  try {
    if (confirmDeleteKind === "messenger") {
      const list = await invoke<UserMessenger[]>("list_user_messengers");
      confirmDeleteName.value = list.find(m => m.id === confirmDeleteId)?.name ?? "";
    } else {
      const list = await invoke<CustomShortcut[]>("list_custom_shortcuts");
      confirmDeleteName.value = list.find(s => s.id === confirmDeleteId)?.name ?? "";
    }
  } catch (e) {
    console.warn("Failed to load delete target:", e);
  }
  // Already gone (removed elsewhere, or a stale window) — nothing left to confirm.
  confirmDeleteMissing.value = confirmDeleteName.value === "";
}

async function approveDelete() {
  if (confirmDeleteBusy.value) return;
  confirmDeleteBusy.value = true;
  try {
    await emit("confirm-delete-approved", { kind: confirmDeleteKind, id: confirmDeleteId });
  } catch (e) {
    console.error("Failed to confirm deletion:", e);
    confirmDeleteBusy.value = false;
    return;
  }
  closeDialogWindow();
}

const unlisteners: UnlistenFn[] = [];

const bugReportLogs = ref("");
const bugReportLoading = ref(false);
const bugReportCopied = ref(false);

async function loadBugReportLogs() {
  bugReportLoading.value = true;
  try {
    bugReportLogs.value = await invoke<string>("get_recent_logs", { lines: 200 });
  } catch (e) {
    bugReportLogs.value = `Failed to load logs: ${String(e)}`;
  } finally {
    bugReportLoading.value = false;
  }
}

async function copyLogs() {
  await navigator.clipboard.writeText(bugReportLogs.value);
  bugReportCopied.value = true;
  setTimeout(() => { bugReportCopied.value = false; }, 2000);
}

function openGithubIssue() {
  const body = encodeURIComponent(
    `**Describe the bug**\n\n<!-- What happened? -->\n\n**App version**\n${appVersion.value || 'unknown'}\n\n**Recent logs**\n\`\`\`\n${bugReportLogs.value.slice(-3000)}\n\`\`\``
  );
  window.open(
    `https://github.com/laskarzhevsky/signalist/issues/new?labels=bug&title=Bug+report&body=${body}`,
    "_blank"
  );
}

onMounted(async () => {
  if (isBugReportView) {
    try { appVersion.value = await getVersion(); } catch {}
    await loadBugReportLogs();
    return;
  }

  if (isEditDialogView) {
    try {
      const shortcuts = await invoke<CustomShortcut[]>("list_custom_shortcuts");
      const sc = shortcuts.find(s => s.id === editShortcutId);
      if (sc) {
        newShortcutName.value = sc.name;
        newShortcutUrl.value = sc.url;
        newShortcutIcon.value = sc.icon ?? null;
        newShortcutColor.value = sc.color ?? null;
      }
    } catch (e) {
      console.warn("Failed to load shortcut for editing:", e);
    }
    return;
  }

  if (isAddMessengerView) {
    await loadUserMessengers();
    await loadBuiltinPreload();
    return;
  }

  if (isConfirmDeleteView) {
    await loadConfirmDeleteTarget();
    return;
  }

  if (isDialogView) {
    return;
  }

  try {
    if (!(await isPermissionGranted())) {
      await requestPermission();
    }
  } catch (e) {
    console.warn("Notification permission request failed:", e);
  }

  try {
    currentHotkey.value = await invoke<string>("get_global_shortcut");
  } catch (e) {
    console.warn("Failed to load hotkey:", e);
  }

  try {
    autostartEnabled.value = await invoke<boolean>("get_autostart");
  } catch (e) {
    console.warn("Failed to load autostart state:", e);
  }

  try {
    silenceEnabled.value = await invoke<boolean>("get_silence_mode");
  } catch (e) {
    console.warn("Failed to load silence mode:", e);
  }

  unlisteners.push(
    await listen<{ messenger: string; count: number }>("unread-update", (event) => {
      unreadCounts[event.payload.messenger] = event.payload.count;
    }),
    await listen<string>("active-messenger-changed", (event) => {
      activeMessenger.value = event.payload;
    }),
    await listen<CustomShortcut>("shortcut-added", async (event) => {
      customShortcuts.value.push(event.payload);
      await openCustomShortcut(event.payload);
    }),
    await listen<CustomShortcut>("shortcut-updated", (event) => {
      const idx = customShortcuts.value.findIndex(s => s.id === event.payload.id);
      if (idx !== -1) customShortcuts.value[idx] = event.payload;
    }),
    await listen<UserMessenger>("messenger-added", async (event) => {
      userMessengers.value.push(event.payload);
      await openUserMessenger(event.payload);
    }),
    await listen<{ kind: string; id: string }>("confirm-delete-approved", async (event) => {
      try {
        if (event.payload.kind === "messenger") {
          await removeUserMessenger(event.payload.id);
        } else {
          await removeShortcut(event.payload.id);
        }
      } catch (e) {
        console.error("Failed to remove item:", e);
      }
    }),
    await listen<boolean>("theme-update", (event) => {
      autoIsDark.value = event.payload;
    }),
  );

  // Which view is on screen is decided by the Rust-side startup preload, not
  // here. If it already picked one before this listener was attached, catch up;
  // otherwise the `active-messenger-changed` event above delivers it.
  try {
    const active = await invoke<string>("get_active_messenger");
    if (active) activeMessenger.value = active;
  } catch (e) {
    console.warn("Failed to read active messenger:", e);
  }

  try {
    const unlistenAction = await onAction(() => {
      invoke("show_window").catch(e => console.warn("show_window failed:", e));
    });
    unlisteners.push(() => unlistenAction.unregister());
  } catch (e) {
    console.warn("Failed to register notification action listener:", e);
  }

  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  const mqHandler = (e: MediaQueryListEvent) => { systemIsDark.value = e.matches; };
  mq.addEventListener('change', mqHandler);
  unlisteners.push(() => mq.removeEventListener('change', mqHandler));

  try {
    appVersion.value = await getVersion();
  } catch (e) {
    console.warn("Failed to get app version:", e);
  }

  try {
    cachedUpdate = await check();
    if (cachedUpdate?.available) {
      updateAvailable.value = true;
      updateVersion.value = cachedUpdate.version;
    }
  } catch (e) {
    console.warn("Update check failed:", e);
  }

  await loadCustomShortcuts();
  await loadUserMessengers();
});

onUnmounted(() => {
  unlisteners.forEach(fn => fn());
  document.removeEventListener("click", onDocumentClick);
});

async function openMessenger(label: string) {
  try {
    await invoke("open_messenger", { messenger: label });
    activeMessenger.value = label;
  } catch (e) {
    console.error(`Failed to open ${label}:`, e);
  }
}

async function switchMessenger(label: string) {
  try {
    await invoke("switch_messenger", { messenger: label });
    activeMessenger.value = label;
  } catch (e) {
    console.error(`Failed to switch to ${label}:`, e);
    await openMessenger(label);
  }
}

</script>

<template>
  <!-- ── Add Messenger view ─────────────────────────────────────────────── -->
  <div
    v-if="isAddMessengerView"
    class="h-screen flex flex-col bg-surface p-5 gap-4 select-none"
  >
    <h2 class="text-text-primary text-base font-semibold leading-none">Add Messenger</h2>

    <div class="grid grid-cols-2 gap-2">
      <button
        v-for="entry in MESSENGER_CATALOG"
        :key="entry.name"
        :disabled="isAddingMessenger || isMessengerAdded(entry)"
        :class="[
          'flex items-center gap-3 px-4 py-3 rounded-xl transition-colors',
          isMessengerAdded(entry)
            ? 'bg-surface text-text-muted cursor-not-allowed'
            : 'bg-surface-hover text-text-primary hover:bg-surface-active cursor-pointer',
          isAddingMessenger && !isMessengerAdded(entry) ? 'cursor-wait' : '',
        ]"
        @click="addFromCatalog(entry)"
      >
        <span v-if="iconMap[entry.icon]" class="flex h-6 w-6 shrink-0 [&>svg]:h-6 [&>svg]:w-6" v-html="iconMap[entry.icon]" />
        <span v-else class="flex h-6 w-6 shrink-0 items-center justify-center text-accent text-sm font-bold">{{ entry.name[0] }}</span>
        <span class="text-sm font-medium flex-1 text-left">{{ entry.name }}</span>
        <svg v-if="isMessengerAdded(entry)" xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" class="shrink-0 text-accent opacity-70"><path d="M20 6 9 17l-5-5"/></svg>
      </button>
    </div>

    <label class="flex items-center gap-2 -mt-1 cursor-pointer text-text-muted hover:text-text-primary transition-colors">
      <input
        v-model="newMessengerPreload"
        type="checkbox"
        class="h-3.5 w-3.5 accent-accent cursor-pointer"
      >
      <span class="text-[11px]">Load newly added messenger at startup</span>
    </label>

    <div class="w-full border-t border-glass-border" />

    <!-- Startup list: everything already in the sidebar, built-in or added -->
    <div class="flex flex-col gap-1 min-h-0 flex-1 overflow-y-auto">
      <h3 class="text-text-primary text-xs font-semibold uppercase tracking-wide opacity-70">
        Load at startup
      </h3>
      <p class="text-[11px] text-text-muted leading-relaxed mb-1">
        Loads the messenger in the background when the app starts, so unread badges and
        notifications arrive without opening it first. Each one keeps a background process
        running — enable only what you need.
      </p>

      <label
        v-for="m in messengers"
        :key="m.label"
        class="flex items-center gap-3 px-2 py-1.5 rounded-lg hover:bg-surface-hover cursor-pointer transition-colors"
      >
        <input
          type="checkbox"
          class="h-4 w-4 accent-accent cursor-pointer"
          :checked="builtinPreload[m.label] ?? true"
          @change="toggleBuiltinPreload(m.label)"
        >
        <span class="flex h-5 w-5 shrink-0 text-text-muted [&>svg]:h-5 [&>svg]:w-5" v-html="m.icon" />
        <span class="text-sm text-text-primary flex-1">{{ m.displayName }}</span>
      </label>

      <label
        v-for="m in userMessengers"
        :key="m.id"
        class="flex items-center gap-3 px-2 py-1.5 rounded-lg hover:bg-surface-hover cursor-pointer transition-colors"
      >
        <input
          type="checkbox"
          class="h-4 w-4 accent-accent cursor-pointer"
          :checked="m.preload ?? false"
          @change="toggleUserPreload(m)"
        >
        <span v-if="m.icon && iconMap[m.icon]" class="flex h-5 w-5 shrink-0 text-text-muted [&>svg]:h-5 [&>svg]:w-5" v-html="iconMap[m.icon]" />
        <span v-else class="flex h-5 w-5 shrink-0 items-center justify-center text-accent text-xs font-bold">{{ shortcutInitial(m.name) }}</span>
        <span class="text-sm text-text-primary flex-1">{{ m.name }}</span>
        <span class="text-[10px] text-text-muted opacity-60 shrink-0" title="Unread counts are only tracked for Telegram and WhatsApp">no badges</span>
      </label>
    </div>

    <p v-if="addError" class="text-[11px] text-red-400">{{ addError }}</p>

    <div class="flex items-center justify-between">
      <span class="text-[10px] text-text-muted opacity-60">Startup changes apply on next launch</span>
      <button
        class="px-4 py-2 rounded-lg text-sm text-text-muted hover:bg-surface-hover cursor-pointer transition-colors"
        @click="closeDialogWindow()"
      >Done</button>
    </div>
  </div>

  <!-- ── Bug Report view ──────────────────────────────────────────────────── -->
  <div
    v-else-if="isBugReportView"
    class="h-screen flex flex-col bg-surface p-5 gap-3 select-none"
  >
    <div class="flex items-center justify-between">
      <h2 class="text-text-primary text-base font-semibold leading-none">Bug Report</h2>
      <span v-if="appVersion" class="text-[11px] text-text-muted opacity-60">v{{ appVersion }}</span>
    </div>
    <p class="text-[11px] text-text-muted leading-relaxed">
      Review the logs below. Nothing is sent automatically — you decide what to include.
    </p>
    <div class="relative flex-1 min-h-0">
      <div v-if="bugReportLoading" class="flex items-center justify-center h-full text-text-muted text-sm">
        Loading logs…
      </div>
      <textarea
        v-else
        :value="bugReportLogs"
        readonly
        class="w-full h-full resize-none rounded-lg border border-glass-border bg-surface-hover px-3 py-2 text-[11px] text-text-muted font-mono outline-none leading-relaxed"
        style="color-scheme: dark;"
      />
    </div>
    <div class="flex gap-2">
      <button
        class="flex-1 px-3 py-2 rounded-lg text-sm text-text-muted hover:bg-surface-hover cursor-pointer transition-colors border border-glass-border"
        @click="copyLogs"
      >{{ bugReportCopied ? 'Copied!' : 'Copy logs' }}</button>
      <button
        class="flex-1 px-3 py-2 rounded-lg text-sm font-medium bg-accent text-surface hover:opacity-90 cursor-pointer transition-colors"
        @click="openGithubIssue"
      >Open GitHub Issue</button>
    </div>
    <button
      class="text-[11px] text-text-muted hover:text-text-primary cursor-pointer text-center transition-colors"
      @click="closeDialogWindow()"
    >Cancel</button>
  </div>

  <!-- ── Confirm delete view ──────────────────────────────────────────────── -->
  <div
    v-else-if="isConfirmDeleteView"
    class="h-screen flex flex-col bg-surface p-5 gap-3 select-none"
  >
    <h2 class="text-text-primary text-base font-semibold leading-none">
      {{ confirmDeleteKind === 'messenger' ? 'Delete messenger' : 'Delete shortcut' }}
    </h2>

    <p v-if="confirmDeleteMissing" class="text-sm text-text-muted leading-relaxed flex-1">
      This item is no longer in the sidebar.
    </p>
    <p v-else class="text-sm text-text-primary leading-relaxed flex-1">
      Remove <span class="font-semibold">{{ confirmDeleteName }}</span> from the sidebar?
      <span class="block mt-1 text-[11px] text-text-muted">This can’t be undone.</span>
    </p>

    <div class="flex gap-2 justify-end">
      <button
        class="px-4 py-2 rounded-lg text-sm text-text-muted hover:bg-surface-hover cursor-pointer transition-colors border border-glass-border"
        @click="closeDialogWindow()"
      >{{ confirmDeleteMissing ? 'Close' : 'Cancel' }}</button>
      <button
        v-if="!confirmDeleteMissing"
        :disabled="confirmDeleteBusy"
        class="px-4 py-2 rounded-lg text-sm font-medium bg-badge-bg text-badge-text hover:opacity-90 cursor-pointer transition-colors disabled:cursor-wait disabled:opacity-60"
        @click="approveDelete"
      >Delete</button>
    </div>
  </div>

  <!-- ── Dialog view (Add / Edit Web Shortcut) ────────────────────────────── -->
  <div
    v-else-if="isDialogView || isEditDialogView"
    class="h-screen flex flex-col bg-surface p-5 gap-2 select-none"
  >
    <h2 class="text-text-primary text-base font-semibold leading-none">
      {{ isEditDialogView ? 'Edit Web Shortcut' : 'Add Web Shortcut' }}
    </h2>

    <div v-if="isDialogView" class="flex gap-2">
      <button
        v-for="p in QUICK_PRESETS"
        :key="p.name"
        type="button"
        class="flex flex-1 flex-col items-center gap-1 rounded-lg py-2 bg-surface-hover hover:bg-surface-active transition-colors cursor-pointer"
        @click="applyPreset(p)"
      >
        <span v-if="iconMap[p.icon]" class="flex h-5 w-5 [&>svg]:h-5 [&>svg]:w-5 text-text-muted" v-html="iconMap[p.icon]" />
        <span v-else class="text-[10px] font-bold text-text-muted leading-none">{{ p.name[0] }}</span>
        <span class="text-[10px] text-text-muted leading-none">{{ p.name }}</span>
      </button>
    </div>

    <div class="flex flex-col gap-1">
      <label class="text-text-muted text-xs">Name</label>
      <input
        v-model="newShortcutName"
        type="text"
        placeholder="Linear"
        class="rounded-lg border border-glass-border bg-surface-hover px-3 py-2 text-sm text-text-primary outline-none focus:border-accent transition-colors"
        style="color-scheme: dark;"
        autofocus
        @keydown.enter="submitShortcut(isEditDialogView ? 'edit' : 'add')"
        @keydown.esc="closeDialogWindow()"
      />
    </div>

    <div class="flex flex-col gap-1">
      <label class="text-text-muted text-xs">URL</label>
      <input
        v-model="newShortcutUrl"
        type="text"
        placeholder="https://linear.app"
        class="rounded-lg border border-glass-border bg-surface-hover px-3 py-2 text-sm text-text-primary outline-none focus:border-accent transition-colors"
        style="color-scheme: dark;"
        @keydown.enter="submitShortcut(isEditDialogView ? 'edit' : 'add')"
        @keydown.esc="closeDialogWindow()"
      />
    </div>

    <div class="flex flex-col gap-1">
      <label class="text-text-muted text-xs">Icon <span class="opacity-50">(optional)</span></label>
      <div class="grid grid-cols-9 gap-1">
        <button
          v-for="(svg, key) in iconMap"
          :key="key"
          type="button"
          :class="[
            'h-8 w-8 flex items-center justify-center rounded-lg transition-all duration-100',
            newShortcutIcon === key
              ? 'bg-accent text-surface'
              : 'bg-surface-hover text-text-muted hover:text-text-primary hover:bg-surface-active',
          ]"
          :title="key"
          @click="newShortcutIcon = newShortcutIcon === key ? null : key"
        >
          <span class="flex h-4 w-4 [&>svg]:h-4 [&>svg]:w-4" v-html="svg" />
        </button>
      </div>
    </div>

    <div class="flex flex-col gap-1">
      <label class="text-text-muted text-xs">Color <span class="opacity-50">(optional)</span></label>
      <div class="flex items-center gap-2">
        <!-- Live preview of the sidebar button -->
        <span
          class="flex h-8 w-8 shrink-0 items-center justify-center rounded-xl bg-glass-border text-sm font-semibold"
          :class="newShortcutColor ? '' : 'text-accent'"
          :style="newShortcutColor ? { color: newShortcutColor } : undefined"
        >
          <span
            v-if="newShortcutIcon && iconMap[newShortcutIcon]"
            class="flex h-4.5 w-4.5 [&>svg]:h-4.5 [&>svg]:w-4.5"
            v-html="iconMap[newShortcutIcon]"
          />
          <template v-else>{{ shortcutInitial(newShortcutName) }}</template>
        </span>

        <div class="flex flex-wrap items-center gap-1">
          <button
            type="button"
            :class="[
              'h-5 w-5 rounded-full bg-transparent cursor-pointer transition-all duration-100',
              newShortcutColor === null
                ? 'border-2 border-text-primary'
                : 'border border-dashed border-glass-border hover:border-text-muted',
            ]"
            title="Default (theme accent)"
            @click="newShortcutColor = null"
          />
          <button
            v-for="c in SHORTCUT_COLORS"
            :key="c.value"
            type="button"
            :class="[
              'h-5 w-5 rounded-full cursor-pointer transition-all duration-100',
              newShortcutColor === c.value
                ? 'border-2 border-text-primary'
                : 'border border-glass-border hover:scale-110',
            ]"
            :style="{ backgroundColor: c.value }"
            :title="c.name"
            @click="newShortcutColor = newShortcutColor === c.value ? null : c.value"
          />
        </div>
      </div>
    </div>

    <p v-if="addError" class="text-[11px] text-red-400 -mt-2">{{ addError }}</p>

    <div class="mt-auto flex gap-2 justify-end">
      <button
        class="px-4 py-2 rounded-lg text-sm text-text-muted hover:bg-surface-hover cursor-pointer transition-colors"
        @click="closeDialogWindow()"
      >Cancel</button>
      <button
        :class="isAdding ? 'bg-surface-active text-text-muted cursor-wait' : 'bg-accent text-surface hover:opacity-90'"
        class="px-4 py-2 rounded-lg text-sm font-medium cursor-pointer transition-colors"
        :disabled="isAdding"
        @click="submitShortcut(isEditDialogView ? 'edit' : 'add')"
      >
        {{ isAdding ? (isEditDialogView ? 'Saving…' : 'Adding…') : (isEditDialogView ? 'Save' : 'Add') }}
      </button>
    </div>
  </div>

  <!-- ── Sidebar view ─────────────────────────────────────────────────────── -->
  <template v-else>
    <nav
      class="glass fixed top-0 left-0 z-50 flex h-screen w-[64px] flex-col items-center gap-2 border-r border-glass-border select-none"
    >
      <!-- Zone 1: Logo -->
      <div class="flex w-full flex-col items-center pt-3 pb-2">
        <span class="flex h-10 w-10 items-center justify-center text-accent [&>svg]:h-10 [&>svg]:w-10" v-html="logoIcon" />
        <span v-if="appVersion" class="mt-1 text-[9px] leading-none text-text-muted select-none opacity-50">
          v{{ appVersion }}
        </span>
        <span v-if="isDev" class="mt-2 rounded px-1 py-[5px] text-[8px] font-bold leading-none tracking-wider text-white bg-red-500 select-none">
          DEV
        </span>
      </div>

      <div class="w-10 border-t border-glass-border" />

      <!-- Scrollable middle: zones 2, 2.5, 3 -->
      <div class="flex-1 overflow-y-auto scrollbar-hidden scroll-fade w-full flex flex-col items-center pb-2">

      <!-- Zone 2: Built-in messengers -->
      <div class="flex w-full flex-col items-center gap-1 px-2 pt-3">
        <div v-for="m in messengers" :key="m.label" class="relative">
          <button
            :class="[
              'group flex h-10 w-10 items-center justify-center rounded-xl border-none cursor-pointer transition-all duration-150',
              activeMessenger === m.label
                ? 'bg-glass-border text-accent'
                : 'text-text-muted hover:bg-surface-hover',
            ]"
            :title="m.displayName"
            @click="switchMessenger(m.label)"
          >
            <span class="flex h-5 w-5 items-center justify-center [&>svg]:h-5 [&>svg]:w-5" v-html="m.icon" />
          </button>

          <span
            v-if="unreadCounts[m.label] > 0"
            class="absolute -top-1 -right-1 flex h-4.5 min-w-4.5 items-center justify-center rounded-full bg-badge-bg px-1 text-sm font-semibold leading-none text-badge-text"
          >
            {{ unreadCounts[m.label] > 99 ? "99+" : unreadCounts[m.label] }}
          </span>
        </div>
      </div>

      <!-- Zone 2.5: User Messengers + Add Messenger button -->
      <div class="flex w-full flex-col items-center gap-1 px-2 pt-3">
        <div
          v-for="m in userMessengers"
          :key="m.id"
          class="relative group"
        >
          <button
            :class="[
              'flex h-10 w-10 items-center justify-center rounded-xl text-sm font-semibold cursor-pointer transition-all duration-150',
              activeMessenger === shortcutLabel(m.id)
                ? 'bg-glass-border text-accent'
                : 'text-text-muted hover:bg-surface-hover',
            ]"
            :title="m.name"
            @click="switchToUserMessenger(m)"
          >
            <span v-if="m.icon && iconMap[m.icon]" class="flex h-5 w-5 [&>svg]:h-5 [&>svg]:w-5" v-html="iconMap[m.icon]" />
            <template v-else>{{ shortcutInitial(m.name) }}</template>
          </button>
          <button
            class="absolute -top-1 -right-1 hidden group-hover:flex items-center justify-center h-5 w-5 p-0 rounded-full bg-surface text-text-muted hover:bg-badge-bg hover:text-white text-sm line-height cursor-pointer"
            title="Remove"
            @click.stop="askRemoveUserMessenger(m.id)"
          >×</button>
        </div>

        <button
          class="flex h-10 w-10 items-center justify-center rounded-xl border border-dashed border-glass-border cursor-pointer transition-all duration-150 text-text-muted hover:border-accent hover:text-accent bg-transparent"
          title="Add messenger"
          @click="invoke('open_add_messenger_window')"
        >
          <svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 5v14M5 12h14"/>
          </svg>
        </button>
      </div>

      <!-- Zone 3: Custom Shortcuts + Add button -->
      <div class="flex w-full flex-col items-center gap-1 px-2 pt-3">
        <div
          v-for="(sc, i) in customShortcuts"
          :key="sc.id"
          class="relative group"
          :class="[
            dragIndex === i ? 'z-10' : '',
            dragIndex !== null ? 'transition-transform duration-150' : '',
          ]"
          :style="shortcutDragStyle(i)"
        >
          <button
            :class="[
              'flex h-10 w-10 items-center justify-center rounded-xl text-sm font-semibold cursor-pointer touch-none transition-all duration-150',
              activeMessenger === shortcutLabel(sc.id)
                ? 'bg-glass-border text-accent'
                : 'text-text-muted hover:bg-surface-hover',
            ]"
            :style="sc.color ? { color: sc.color } : undefined"
            :title="sc.name"
            @pointerdown="onShortcutPointerDown($event, i)"
            @pointermove="onShortcutPointerMove"
            @pointerup="onShortcutPointerUp"
            @pointercancel="onShortcutPointerUp"
            @click="onShortcutClick(sc)"
            @contextmenu.prevent="invoke('open_edit_shortcut_window', { id: sc.id })"
          >
            <span v-if="sc.icon && iconMap[sc.icon]" class="flex h-5 w-5 [&>svg]:h-5 [&>svg]:w-5" v-html="iconMap[sc.icon]" />
            <template v-else>{{ shortcutInitial(sc.name) }}</template>
          </button>
          <button
            class="absolute -top-1 -right-1 hidden group-hover:flex items-center justify-center h-5 w-5 p-0 rounded-full bg-surface text-text-muted hover:bg-badge-bg hover:text-white text-sm line-height cursor-pointer"
            title="Remove"
            @click.stop="askRemoveShortcut(sc.id)"
          >×</button>
        </div>

        <button
          class="flex h-10 w-10 items-center justify-center rounded-xl border border-dashed border-glass-border cursor-pointer transition-all duration-150 text-text-muted hover:border-accent hover:text-accent bg-transparent"
          title="Add shortcut"
          @click="invoke('open_add_shortcut_window')"
        >
          <svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 5v14M5 12h14"/>
          </svg>
        </button>
      </div>

      </div><!-- /scrollable middle -->

      <!-- Zone 4: Settings -->
      <div class="w-full flex flex-col items-center">
        <div class="w-10 border-t border-glass-border mb-1" />

        <!-- Settings gear + accordion panel wrapper -->
        <div ref="settingsWrapper" class="relative w-full flex flex-col items-center mb-3">

          <!-- Accordion panel — floats above shortcuts -->
          <Transition
            enter-active-class="transition-all duration-200 ease-out"
            enter-from-class="opacity-0 translate-y-2"
            enter-to-class="opacity-100 translate-y-0"
            leave-active-class="transition-all duration-150 ease-in"
            leave-from-class="opacity-100 translate-y-0"
            leave-to-class="opacity-0 translate-y-2"
          >
            <div
              v-if="settingsOpen"
              class="absolute bottom-full left-0 w-full flex flex-col items-center gap-1 py-2 rounded-xl bg-surface border border-glass-border shadow-xl z-10"
            >
              <button
                class="flex flex-col h-12 w-12 items-center justify-center gap-0.5 rounded-xl border-none bg-transparent cursor-pointer transition-all duration-150"
                :class="autostartEnabled ? 'text-accent hover:bg-surface-hover' : 'text-text-muted hover:bg-surface-hover hover:text-text-primary'"
                :title="autostartEnabled ? 'Автозапуск увімкнено — натисніть, щоб вимкнути' : 'Автозапуск вимкнено — натисніть, щоб увімкнути'"
                @click="toggleAutostart"
              >
                <svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M12 2v4M12 18v4M4.93 4.93l2.83 2.83M16.24 16.24l2.83 2.83M2 12h4M18 12h4M4.93 19.07l2.83-2.83M16.24 7.76l2.83-2.83"/>
                </svg>
                <span class="text-[9px] leading-none opacity-70 select-none font-medium">
                  {{ autostartEnabled ? 'AUTO' : 'auto' }}
                </span>
              </button>

              <!-- Silence mode button -->
              <button
                class="flex flex-col h-12 w-12 items-center justify-center gap-0.5 rounded-xl border-none bg-transparent cursor-pointer transition-all duration-150"
                :class="silenceEnabled ? 'text-accent hover:bg-surface-hover' : 'text-text-muted hover:bg-surface-hover hover:text-text-primary'"
                :title="silenceEnabled ? 'Тиша увімкнена — сповіщення вимкнено. Натисніть, щоб увімкнути' : 'Сповіщення увімкнено — натисніть, щоб вимкнути'"
                @click="toggleSilence"
              >
                <svg v-if="silenceEnabled" xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V5a3 3 0 0 0-5.94-.6"/>
                  <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2a7 7 0 0 1-.11 1.23"/>
                  <line x1="12" y1="19" x2="12" y2="22"/>
                  <line x1="2" y1="2" x2="22" y2="22"/>
                </svg>
                <svg v-else xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/>
                  <path d="M19 10v2a7 7 0 0 1-14 0v-2"/>
                  <line x1="12" y1="19" x2="12" y2="22"/>
                </svg>
                <span class="text-[9px] leading-none opacity-70 select-none font-medium">
                  {{ silenceEnabled ? 'MUTE' : 'mute' }}
                </span>
              </button>

              <span v-if="hotkeyError" class="text-[8px] text-red-400 leading-none text-center px-1">{{ hotkeyError }}</span>

              <button
                class="flex flex-col h-12 w-12 items-center justify-center gap-0.5 rounded-xl border-none bg-transparent cursor-pointer transition-all duration-150"
                :class="isRecordingHotkey ? 'text-accent bg-surface-hover animate-pulse' : 'text-text-muted hover:bg-surface-hover hover:text-text-primary'"
                :title="isRecordingHotkey ? 'Press key combination (Esc to cancel)' : `Global hotkey: ${currentHotkey} — click to change`"
                @click="isRecordingHotkey ? null : startRecordingHotkey()"
              >
                <span class="text-base leading-none select-none">⌨</span>
                <span class="text-[9px] leading-none opacity-70 select-none font-medium">
                  {{ isRecordingHotkey ? '···' : formatHotkeyDisplay(currentHotkey) }}
                </span>
              </button>

              <!-- Theme mode button -->
              <button
                class="flex flex-col h-12 w-12 items-center justify-center gap-0.5 rounded-xl border-none bg-transparent cursor-pointer transition-all duration-150 text-text-muted hover:bg-surface-hover hover:text-text-primary"
                :title="`Тема: ${themeModeLabel} — натисніть, щоб змінити`"
                @click="cycleTheme"
              >
                <!-- system -->
                <svg v-if="themeMode === 'system'" xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/>
                </svg>
                <!-- light -->
                <svg v-else-if="themeMode === 'light'" xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <circle cx="12" cy="12" r="4"/><line x1="12" y1="2" x2="12" y2="6"/><line x1="12" y1="18" x2="12" y2="22"/><line x1="4.93" y1="4.93" x2="7.76" y2="7.76"/><line x1="16.24" y1="16.24" x2="19.07" y2="19.07"/><line x1="2" y1="12" x2="6" y2="12"/><line x1="18" y1="12" x2="22" y2="12"/><line x1="4.93" y1="19.07" x2="7.76" y2="16.24"/><line x1="16.24" y1="7.76" x2="19.07" y2="4.93"/>
                </svg>
                <!-- dark -->
                <svg v-else-if="themeMode === 'dark'" xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/>
                </svg>
                <!-- auto -->
                <svg v-else xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="m12 3-1.912 5.813a2 2 0 0 1-1.275 1.275L3 12l5.813 1.912a2 2 0 0 1 1.275 1.275L12 21l1.912-5.813a2 2 0 0 1 1.275-1.275L21 12l-5.813-1.912a2 2 0 0 1-1.275-1.275L12 3Z"/>
                  <path d="M5 3v4"/><path d="M19 17v4"/><path d="M3 5h4"/><path d="M17 19h4"/>
                </svg>
                <span class="text-[9px] leading-none opacity-70 select-none font-medium">{{ themeModeLabel }}</span>
              </button>

              <!-- Bug report button -->
              <button
                class="flex flex-col h-12 w-12 items-center justify-center gap-0.5 rounded-xl border-none bg-transparent cursor-pointer transition-all duration-150 text-text-muted hover:bg-surface-hover hover:text-text-primary"
                title="Report a bug"
                @click="invoke('open_bug_report_window')"
              >
                <svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <rect x="8" y="6" width="8" height="14" rx="2"/>
                  <path d="M8 10H5a1 1 0 0 0-1 1v2a1 1 0 0 0 1 1h3"/>
                  <path d="M16 10h3a1 1 0 0 1 1 1v2a1 1 0 0 1-1 1h-3"/>
                  <path d="M8 6l-2-3"/>
                  <path d="M16 6l2-3"/>
                  <path d="M12 6V3"/>
                </svg>
                <span class="text-[9px] leading-none opacity-70 select-none font-medium">BUG</span>
              </button>
            </div>
          </Transition>

          <!-- Gear button -->
          <button
            class="flex h-12 w-12 items-center justify-center rounded-xl border-none bg-transparent cursor-pointer transition-all duration-150"
            :class="settingsOpen ? 'text-accent bg-surface-hover' : 'text-text-muted hover:bg-surface-hover hover:text-text-primary'"
            title="Settings"
            @click="settingsOpen = !settingsOpen"
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              width="20" height="20"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
              class="transition-transform duration-300"
              :style="{ transform: settingsOpen ? 'rotate(90deg)' : 'rotate(0deg)' }"
            >
              <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/>
              <circle cx="12" cy="12" r="3"/>
            </svg>
          </button>

          <!-- Update button — always visible when update is available -->
          <Transition
            enter-active-class="transition-all duration-200 ease-out"
            enter-from-class="opacity-0 scale-90"
            enter-to-class="opacity-100 scale-100"
            leave-active-class="transition-all duration-150 ease-in"
            leave-from-class="opacity-100 scale-100"
            leave-to-class="opacity-0 scale-90"
          >
            <button
              v-if="updateAvailable"
              class="flex flex-col h-12 w-12 items-center justify-center gap-0.5 rounded-xl border-none bg-transparent cursor-pointer transition-all duration-150 text-accent hover:bg-surface-hover"
              :title="`Оновлення ${updateVersion} доступне — натисніть, щоб встановити`"
              :disabled="isInstalling"
              @click="installUpdate"
            >
              <svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M12 2v13M7 13l5 5 5-5"/><path d="M20 21H4"/>
              </svg>
              <span class="text-[9px] leading-none opacity-70 select-none font-medium">
                {{ isInstalling ? '···' : 'UPD' }}
              </span>
            </button>
          </Transition>
        </div>
      </div>
    </nav>

    <main class="ml-[64px] h-screen" />
  </template>
</template>
