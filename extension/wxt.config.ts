import { defineConfig } from 'wxt';

export default defineConfig({
  modules: ['@wxt-dev/module-svelte'],
  svelte: {
    vite: {
      // The overlay runs in tabs whose CSP can disallow Svelte's Trusted Types policy.
      dynamicCompileOptions: ({ filename }) =>
        filename.endsWith('SubtitleOverlay.svelte') ? { fragments: 'tree' } : undefined,
    },
  },
  manifest: {
    name: 'Local Live Subtitles',
    description: 'Real-time local Vulkan speech transcription subtitles for active browser tabs',
    permissions: ['activeTab', 'tabCapture', 'offscreen', 'storage', 'scripting'],
  },
});
