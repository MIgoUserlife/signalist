<script setup lang="ts">
import { ref, reactive, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
import { MessageCircle, Send } from "lucide-vue-next";

const activeMessenger = ref("telegram");

interface MessengerConfig {
  label: string;
  displayName: string;
  icon: typeof Send;
}

const messengers: MessengerConfig[] = [
  { label: "telegram", displayName: "Telegram", icon: Send },
  { label: "whatsapp", displayName: "WhatsApp", icon: MessageCircle },
];

const unreadCounts = reactive<Record<string, number>>({
  telegram: 0,
  whatsapp: 0,
});

const currentHotkey = ref("Super+Shift+S");
const isRecordingHotkey = ref(false);
const hotkeyError = ref("");

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

async function saveHotkey(shortcut: string) {
  isRecordingHotkey.value = false;
  try {
    await invoke("set_global_shortcut", { shortcut });
    currentHotkey.value = shortcut;
  } catch (e) {
    hotkeyError.value = "Failed to register";
    setTimeout(() => { hotkeyError.value = ""; }, 2000);
  }
}

let unlisten: UnlistenFn | null = null;

onMounted(async () => {
  // Request notification permission if not already granted
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

  unlisten = await listen<{ messenger: string; count: number }>(
    "unread-update",
    (event) => {
      unreadCounts[event.payload.messenger] = event.payload.count;
    },
  );
});

onUnmounted(() => {
  unlisten?.();
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
  unreadCounts[label] = 0;
  try {
    await invoke("switch_messenger", { messenger: label });
    activeMessenger.value = label;
  } catch (e) {
    console.error(`Failed to switch to ${label}:`, e);
    await openMessenger(label);
  }
}

openMessenger("telegram");
</script>

<template>
  <nav
    class="glass fixed top-0 left-0 z-50 flex h-screen w-[72px] flex-col items-center border-r border-glass-border pt-3 select-none"
  >
    <div
      class="mb-5 flex h-10 w-10 items-center justify-center rounded-xl bg-gradient-to-br from-brand-purple to-brand-blue text-xl font-bold text-white"
    >
      S
    </div>

    <div class="flex w-full flex-col items-center gap-2 px-2">
      <div v-for="m in messengers" :key="m.label" class="relative">
        <button
          :class="[
            'group flex h-12 w-12 items-center justify-center rounded-xl border-none cursor-pointer transition-all duration-150',
            activeMessenger === m.label
              ? 'bg-text-primary text-surface-hover'
              : 'text-text-muted hover:bg-surface-hover hover:text-text-primary',
          ]"
          :title="m.displayName"
          @click="switchMessenger(m.label)"
        >
          <component :is="m.icon" :size="24" :stroke-width="2" />
        </button>

        <span
          v-if="unreadCounts[m.label] > 0"
          class="absolute -top-1 -right-1 flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-badge-bg px-1 text-[11px] font-semibold leading-none text-badge-text"
        >
          {{ unreadCounts[m.label] > 99 ? "99+" : unreadCounts[m.label] }}
        </span>
      </div>
    </div>

    <div class="mt-auto mb-4 flex flex-col items-center gap-1">
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
  </nav>

  <main class="ml-[72px] h-screen" />
</template>
