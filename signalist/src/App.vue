<script setup lang="ts">
import { ref, reactive, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
import { MessageCircle, Send, Settings } from "lucide-vue-next";

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

    <div class="mt-auto mb-4">
      <button
        class="flex h-10 w-10 items-center justify-center rounded-xl border-none bg-transparent text-text-muted transition-colors duration-150 hover:bg-surface-hover hover:text-text-primary cursor-pointer"
        title="Settings"
      >
        <Settings :size="20" :stroke-width="1.8" />
      </button>
    </div>
  </nav>

  <main class="ml-[72px] h-screen" />
</template>
