<script setup lang="ts">
import { ref, reactive, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import telegramIcon from './assets/icons/telegram.svg?raw';
import whatsappIcon from './assets/icons/whatsapp.svg?raw';

const _allIconsRaw = import.meta.glob('./assets/icons/*.svg', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;
const iconMap: Record<string, string> = Object.fromEntries(
  Object.entries(_allIconsRaw)
    .map(([path, svg]) => [path.replace('./assets/icons/', '').replace('.svg', ''), svg])
    .filter(([key]) => key !== 'telegram' && key !== 'whatsapp')
);

const isDialogView = new URLSearchParams(window.location.search).get("view") === "add-shortcut";
const isEditDialogView = new URLSearchParams(window.location.search).get("view") === "edit-shortcut";
const editShortcutId = new URLSearchParams(window.location.search).get("id") ?? "";

// ── Shared state (used by both views) ──────────────────────────────────────
interface CustomShortcut {
  id: string;
  name: string;
  url: string;
  icon?: string;
}

const newShortcutName = ref("");
const newShortcutUrl = ref("");
const addError = ref("");
const isAdding = ref(false);
const newShortcutIcon = ref<string | null>(null);

async function submitAddShortcut() {
  addError.value = "";
  const name = newShortcutName.value.trim();
  let url = newShortcutUrl.value.trim();
  if (!name) { addError.value = "Name is required"; return; }
  if (!url) { addError.value = "URL is required"; return; }
  if (!/^https?:\/\//i.test(url)) url = "https://" + url;
  isAdding.value = true;
  try {
    const sc = await invoke<CustomShortcut>("add_custom_shortcut", { name, url, icon: newShortcutIcon.value ?? null });
    await emit("shortcut-added", sc);
    await getCurrentWebviewWindow().close();
  } catch (e) {
    addError.value = String(e);
  } finally {
    isAdding.value = false;
  }
}

async function submitEditShortcut() {
  addError.value = "";
  const name = newShortcutName.value.trim();
  let url = newShortcutUrl.value.trim();
  if (!name) { addError.value = "Name is required"; return; }
  if (!url) { addError.value = "URL is required"; return; }
  if (!/^https?:\/\//i.test(url)) url = "https://" + url;
  isAdding.value = true;
  try {
    const sc = await invoke<CustomShortcut>("update_custom_shortcut", { id: editShortcutId, name, url, icon: newShortcutIcon.value ?? null });
    await emit("shortcut-updated", sc);
    await getCurrentWebviewWindow().close();
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

const unreadCounts = reactive<Record<string, number>>({
  telegram: 0,
  whatsapp: 0,
});

const currentHotkey = ref("Super+Shift+S");
const isRecordingHotkey = ref(false);
const hotkeyError = ref("");

const autostartEnabled = ref(false);

const appVersion = ref("");

const updateAvailable = ref(false);
const updateVersion = ref("");
const isInstalling = ref(false);

const customShortcuts = ref<CustomShortcut[]>([]);

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
    const update = await check();
    if (update?.available) {
      await update.downloadAndInstall();
      await relaunch();
    }
  } catch (e) {
    console.error("Update install failed:", e);
    isInstalling.value = false;
  }
}

async function toggleAutostart() {
  const next = !autostartEnabled.value;
  try {
    await invoke("set_autostart", { enable: next });
    autostartEnabled.value = next;
  } catch (e) {
    console.error("Failed to toggle autostart:", e);
  }
}

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

async function removeShortcut(id: string) {
  await invoke("remove_custom_shortcut", { id });
  customShortcuts.value = customShortcuts.value.filter(sc => sc.id !== id);
  if (activeMessenger.value === shortcutLabel(id)) {
    await openMessenger("telegram");
  }
}

let unlisten: UnlistenFn | null = null;
let unlistenActive: UnlistenFn | null = null;
let unlistenShortcutAdded: UnlistenFn | null = null;
let unlistenShortcutUpdated: UnlistenFn | null = null;

onMounted(async () => {
  if (isEditDialogView) {
    try {
      const shortcuts = await invoke<CustomShortcut[]>("list_custom_shortcuts");
      const sc = shortcuts.find(s => s.id === editShortcutId);
      if (sc) {
        newShortcutName.value = sc.name;
        newShortcutUrl.value = sc.url;
        newShortcutIcon.value = sc.icon ?? null;
      }
    } catch (e) {
      console.warn("Failed to load shortcut for editing:", e);
    }
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

  unlisten = await listen<{ messenger: string; count: number }>(
    "unread-update",
    (event) => {
      unreadCounts[event.payload.messenger] = event.payload.count;
    },
  );

  unlistenActive = await listen<string>("active-messenger-changed", (event) => {
    activeMessenger.value = event.payload;
  });

  unlistenShortcutAdded = await listen<CustomShortcut>("shortcut-added", async (event) => {
    customShortcuts.value.push(event.payload);
    await openCustomShortcut(event.payload);
  });

  unlistenShortcutUpdated = await listen<CustomShortcut>("shortcut-updated", (event) => {
    const idx = customShortcuts.value.findIndex(s => s.id === event.payload.id);
    if (idx !== -1) customShortcuts.value[idx] = event.payload;
  });

  try {
    appVersion.value = await getVersion();
  } catch (e) {
    console.warn("Failed to get app version:", e);
  }

  try {
    const update = await check();
    if (update?.available) {
      updateAvailable.value = true;
      updateVersion.value = update.version;
    }
  } catch (e) {
    console.warn("Update check failed:", e);
  }

  await loadCustomShortcuts();
});

onUnmounted(() => {
  unlisten?.();
  unlistenActive?.();
  unlistenShortcutAdded?.();
  unlistenShortcutUpdated?.();
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

if (!isDialogView && !isEditDialogView) {
  openMessenger("telegram");
}
</script>

<template>
  <!-- ── Dialog view (Edit Web Shortcut window) ──────────────────────────── -->
  <div
    v-if="isEditDialogView"
    class="h-screen flex flex-col bg-surface p-5 gap-2 select-none"
  >
    <h2 class="text-text-primary text-base font-semibold leading-none">Edit Web Shortcut</h2>

    <div class="flex flex-col gap-1">
      <label class="text-text-muted text-xs">Name</label>
      <input
        v-model="newShortcutName"
        type="text"
        placeholder="Linear"
        class="rounded-lg border border-glass-border bg-surface-hover px-3 py-2 text-sm text-text-primary outline-none focus:border-accent transition-colors"
        style="color-scheme: dark;"
        autofocus
        @keydown.enter="submitEditShortcut"
        @keydown.esc="getCurrentWebviewWindow().close()"
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
        @keydown.enter="submitEditShortcut"
        @keydown.esc="getCurrentWebviewWindow().close()"
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

    <p v-if="addError" class="text-[11px] text-red-400 -mt-2">{{ addError }}</p>

    <div class="mt-auto flex gap-2 justify-end">
      <button
        class="px-4 py-2 rounded-lg text-sm text-text-muted hover:bg-surface-hover cursor-pointer transition-colors"
        @click="getCurrentWebviewWindow().close()"
      >Cancel</button>
      <button
        :class="isAdding ? 'bg-surface-active text-text-muted cursor-wait' : 'bg-accent text-surface hover:opacity-90'"
        class="px-4 py-2 rounded-lg text-sm font-medium cursor-pointer transition-colors"
        :disabled="isAdding"
        @click="submitEditShortcut"
      >{{ isAdding ? 'Saving…' : 'Save' }}</button>
    </div>
  </div>

  <!-- ── Dialog view (Add Web Shortcut window) ────────────────────────────── -->
  <div
    v-else-if="isDialogView"
    class="h-screen flex flex-col bg-surface p-5 gap-2 select-none"
  >
    <h2 class="text-text-primary text-base font-semibold leading-none">Add Web Shortcut</h2>

    <div class="flex flex-col gap-1">
      <label class="text-text-muted text-xs">Name</label>
      <input
        v-model="newShortcutName"
        type="text"
        placeholder="Linear"
        class="rounded-lg border border-glass-border bg-surface-hover px-3 py-2 text-sm text-text-primary outline-none focus:border-accent transition-colors"
        style="color-scheme: dark;"
        autofocus
        @keydown.enter="submitAddShortcut"
        @keydown.esc="getCurrentWebviewWindow().close()"
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
        @keydown.enter="submitAddShortcut"
        @keydown.esc="getCurrentWebviewWindow().close()"
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

    <p v-if="addError" class="text-[11px] text-red-400 -mt-2">{{ addError }}</p>

    <div class="mt-auto flex gap-2 justify-end">
      <button
        class="px-4 py-2 rounded-lg text-sm text-text-muted hover:bg-surface-hover cursor-pointer transition-colors"
        @click="getCurrentWebviewWindow().close()"
      >Cancel</button>
      <button
        :class="isAdding ? 'bg-surface-active text-text-muted cursor-wait' : 'bg-accent text-surface hover:opacity-90'"
        class="px-4 py-2 rounded-lg text-sm font-medium cursor-pointer transition-colors"
        :disabled="isAdding"
        @click="submitAddShortcut"
      >{{ isAdding ? 'Adding…' : 'Add' }}</button>
    </div>
  </div>

  <!-- ── Sidebar view ─────────────────────────────────────────────────────── -->
  <template v-else>
    <nav
      class="glass fixed top-0 left-0 z-50 flex h-screen w-[72px] flex-col items-center gap-2 border-r border-glass-border select-none"
    >
      <!-- Zone 1: Logo -->
      <div class="flex w-full flex-col items-center pt-3 pb-2">
        <img src="./assets/logo.png" alt="Signalist" class="h-10 w-10 rounded-xl" draggable="false" />
        <span v-if="appVersion" class="mt-1 text-[9px] leading-none text-text-muted select-none opacity-50">
          v{{ appVersion }}
        </span>
      </div>

      <div class="w-10 border-t border-glass-border" />

      <!-- Zone 2: Built-in messengers -->
      <div class="flex w-full flex-col items-center gap-2 px-2 pt-3">
        <div v-for="m in messengers" :key="m.label" class="relative">
          <button
            :class="[
              'group flex h-12 w-12 items-center justify-center rounded-xl border-none cursor-pointer transition-all duration-150',
              activeMessenger === m.label
                ? 'bg-text-primary text-surface-hover'
                : 'bg-surface-hover text-text-muted hover:bg-surface-active hover:text-text-primary',
            ]"
            :title="m.displayName"
            @click="switchMessenger(m.label)"
          >
            <span class="flex h-6 w-6 items-center justify-center [&>svg]:h-6 [&>svg]:w-6" v-html="m.icon" />
          </button>

          <span
            v-if="unreadCounts[m.label] > 0"
            class="absolute -top-1 -right-1 flex h-4.5 min-w-4.5 items-center justify-center rounded-full bg-badge-bg px-1 text-sm font-semibold leading-none text-badge-text"
          >
            {{ unreadCounts[m.label] > 99 ? "99+" : unreadCounts[m.label] }}
          </span>
        </div>
      </div>

      <!-- Zone 2.5: Custom Shortcuts + Add button -->
      <div class="flex w-full flex-col items-center gap-2 px-2 pt-3">
        <div
          v-for="sc in customShortcuts"
          :key="sc.id"
          class="relative group"
        >
          <button
            :class="[
              'flex h-12 w-12 items-center justify-center rounded-xl text-sm font-semibold cursor-pointer transition-all duration-150',
              activeMessenger === shortcutLabel(sc.id)
                ? 'bg-text-primary text-surface'
                : 'bg-surface-hover text-text-muted hover:bg-surface-active hover:text-text-primary',
            ]"
            :title="sc.name"
            @click="switchToCustom(sc)"
            @contextmenu.prevent="invoke('open_edit_shortcut_window', { id: sc.id })"
          >
            <span v-if="sc.icon && iconMap[sc.icon]" class="flex h-6 w-6 [&>svg]:h-6 [&>svg]:w-6" v-html="iconMap[sc.icon]" />
            <template v-else>{{ shortcutInitial(sc.name) }}</template>
          </button>
          <button
            class="absolute -top-1 -right-1 hidden group-hover:flex items-center justify-center h-5 w-5 p-0 rounded-full bg-surface text-text-muted hover:bg-red-500 hover:text-white text-sm line-height cursor-pointer"
            title="Remove"
            @click.stop="removeShortcut(sc.id)"
          >×</button>
        </div>

        <button
          class="flex h-12 w-12 items-center justify-center rounded-xl border border-dashed border-glass-border cursor-pointer transition-all duration-150 text-text-muted hover:border-accent hover:text-accent bg-transparent"
          title="Add shortcut"
          @click="invoke('open_add_shortcut_window')"
        >
          <svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 5v14M5 12h14"/>
          </svg>
        </button>
      </div>

      <!-- Zone 3: Settings -->
      <div class="mt-auto w-full flex flex-col items-center">
        <div class="w-10 border-t border-glass-border mb-1" />
        <div class="mb-3 flex flex-col items-center gap-1">
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
          <button
            class="flex flex-col h-12 w-12 items-center justify-center gap-0.5 rounded-xl border-none bg-transparent cursor-pointer transition-all duration-150"
            :class="autostartEnabled
              ? 'text-accent hover:bg-surface-hover'
              : 'text-text-muted hover:bg-surface-hover hover:text-text-primary'"
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
          <span v-if="hotkeyError" class="text-[8px] text-red-400 leading-none text-center px-1">{{ hotkeyError }}</span>
          <button
            class="flex flex-col h-12 w-12 items-center justify-center gap-0.5 rounded-xl border-none bg-transparent cursor-pointer transition-all duration-150"
            :class="isRecordingHotkey
              ? 'text-accent bg-surface-hover animate-pulse'
              : 'text-text-muted hover:bg-surface-hover hover:text-text-primary'"
            :title="isRecordingHotkey ? 'Press key combination (Esc to cancel)' : `Global hotkey: ${currentHotkey} — click to change`"
            @click="isRecordingHotkey ? null : startRecordingHotkey()"
          >
            <span class="text-base leading-none select-none">⌨</span>
            <span class="text-[9px] leading-none opacity-70 select-none font-medium">
              {{ isRecordingHotkey ? '···' : formatHotkeyDisplay(currentHotkey) }}
            </span>
          </button>
        </div>
      </div>
    </nav>

    <main class="ml-[72px] h-screen" />
  </template>
</template>
