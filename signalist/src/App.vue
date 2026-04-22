<script setup lang="ts">
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

const activeMessenger = ref("telegram");

interface MessengerConfig {
  label: string;
  displayName: string;
  icon: string;
}

const messengers: MessengerConfig[] = [
  { label: "telegram", displayName: "Telegram", icon: "✈️" },
  { label: "whatsapp", displayName: "WhatsApp", icon: "💬" },
];

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

openMessenger("telegram");
</script>

<template>
  <nav class="sidebar">
    <div class="sidebar-brand">S</div>
    <div class="sidebar-messengers">
      <button
        v-for="m in messengers"
        :key="m.label"
        :class="['sidebar-btn', { active: activeMessenger === m.label }]"
        :title="m.displayName"
        @click="switchMessenger(m.label)"
      >
        <span class="sidebar-icon">{{ m.icon }}</span>
      </button>
    </div>
  </nav>
</template>

<style>
:root {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  font-size: 14px;
  line-height: 1.5;
  color: #e0e0e0;
  background-color: #1e1e2e;
  margin: 0;
  padding: 0;
  height: 100vh;
  overflow: hidden;
}

html, body, #app {
  height: 100vh;
  margin: 0;
  padding: 0;
  overflow: hidden;
}

.sidebar {
  position: fixed;
  top: 0;
  left: 0;
  width: 72px;
  height: 100vh;
  background-color: #181825;
  display: flex;
  flex-direction: column;
  align-items: center;
  padding-top: 12px;
  z-index: 9999;
  -webkit-user-select: none;
  user-select: none;
  border-right: 1px solid #313244;
}

.sidebar-brand {
  width: 40px;
  height: 40px;
  border-radius: 12px;
  background: linear-gradient(135deg, #7c3aed, #2563eb);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 20px;
  font-weight: 700;
  color: #fff;
  margin-bottom: 20px;
}

.sidebar-messengers {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
  padding: 0 8px;
  box-sizing: border-box;
}

.sidebar-btn {
  width: 56px;
  height: 56px;
  border-radius: 16px;
  border: none;
  background: transparent;
  color: #a6adc8;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.15s, color 0.15s;
  font-size: 24px;
}

.sidebar-btn:hover {
  background: #313244;
  color: #cdd6f4;
}

.sidebar-btn.active {
  background: #45475a;
  color: #89b4fa;
  box-shadow: inset 3px 0 0 #89b4fa;
}

.sidebar-icon {
  line-height: 1;
}
</style>
