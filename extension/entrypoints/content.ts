import { mount, unmount } from 'svelte';
import { createShadowRootUi } from 'wxt/utils/content-script-ui/shadow-root';
import type { BackgroundToContentMessage, SubtitleView } from '../lib/messages';
import SubtitleOverlay from './SubtitleOverlay.svelte';

const SUBTITLE_CSS = `
  .subtitle-wrap {
    position: fixed;
    left: 50%;
    bottom: 48px;
    transform: translateX(-50%);
    max-width: 82vw;
    z-index: 2147483647;
    pointer-events: none;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
  }

  .subtitle-box {
    display: inline-block;
    padding: 8px 16px;
    border-radius: 8px;
    background: var(--sub-bg, rgba(0, 0, 0, 0.78));
    color: var(--sub-color, #ffffff);
    font-family: system-ui, -apple-system, 'Hiragino Kaku Gothic ProN', 'Yu Gothic UI', sans-serif;
    font-size: var(--sub-font-size, 24px);
    font-weight: 600;
    line-height: 1.4;
    text-align: center;
    text-shadow: 0 2px 4px rgba(0, 0, 0, 0.9);
    word-break: break-word;
  }

  .subtitle-box[data-interim='true'] {
    opacity: 0.82;
  }

  .subtitle-box[data-older='true'] {
    opacity: 0.88;
  }

  .subtitle-box[data-furigana='true'] {
    padding-top: 12px;
    line-height: 1.65;
  }

  .subtitle-box ruby {
    ruby-position: over;
    ruby-align: center;
  }

  .subtitle-box rt {
    font-size: var(--sub-rt-size, 12px);
    font-weight: 500;
    color: var(--sub-color, #e2e8f0);
    opacity: 0.88;
    line-height: 1.1;
    user-select: none;
  }

  .speaker-tag {
    display: inline-block;
    margin-right: 0.35em;
    padding: 0.05em 0.38em;
    border-radius: 5px;
    font-size: 0.78em;
    font-weight: 700;
    line-height: 1.25;
    vertical-align: baseline;
    background: rgba(255, 255, 255, 0.14);
    border: 1px solid currentColor;
  }
`;

export default defineContentScript({
  matches: ['<all_urls>'],
  async main(ctx) {
    let current: SubtitleView = { lines: [], visible: false };
    const listeners = new Set<(view: SubtitleView) => void>();
    let hideTimer: ReturnType<typeof setTimeout> | null = null;

    const subscribe = (cb: (view: SubtitleView) => void) => {
      listeners.add(cb);
      cb(current);
      return () => listeners.delete(cb);
    };

    const publish = (next: SubtitleView) => {
      current = next;
      for (const listener of listeners) {
        listener(next);
      }
    };

    const ui = await createShadowRootUi(ctx, {
      name: 'local-live-subtitles',
      position: 'overlay',
      zIndex: 2147483647,
      css: SUBTITLE_CSS,
      anchor: () => document.fullscreenElement ?? document.body,
      onMount(container) {
        const appContainer = document.createElement('div');
        container.append(appContainer);
        return mount(SubtitleOverlay, {
          target: appContainer,
          props: { subscribe },
        });
      },
      onRemove(app) {
        if (app) unmount(app);
      },
    });

    ui.mount();

    const onFullscreenChange = () => {
      const target = document.fullscreenElement ?? document.body;
      if (target && ui.shadowHost.parentElement !== target) {
        target.append(ui.shadowHost);
      }
    };
    document.addEventListener('fullscreenchange', onFullscreenChange);

    const onMessage = (msg: BackgroundToContentMessage) => {
      if (msg.type !== 'SUBTITLE_UPDATE') return;
      if (hideTimer) {
        clearTimeout(hideTimer);
        hideTimer = null;
      }

      publish({
        lines: msg.lines,
        showFurigana: msg.showFurigana,
        style: msg.style,
        visible: msg.visible,
      });

      if (msg.visible && msg.lines.length > 0) {
        hideTimer = setTimeout(() => {
          publish({ lines: [], visible: false });
        }, 7000);
      }
    };
    chrome.runtime.onMessage.addListener(onMessage);

    ctx.onInvalidated(() => {
      document.removeEventListener('fullscreenchange', onFullscreenChange);
      chrome.runtime?.onMessage?.removeListener(onMessage);
      if (hideTimer) clearTimeout(hideTimer);
    });
  },
});
