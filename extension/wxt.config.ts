import { defineConfig } from 'wxt';

export default defineConfig({
  modules: ['@wxt-dev/module-svelte'],
  manifest: {
    name: 'Local Live Subtitles',
    description: 'Real-time local Vulkan speech transcription subtitles for active browser tabs',
    permissions: ['activeTab', 'tabCapture', 'offscreen', 'storage', 'scripting'],
  },
});
