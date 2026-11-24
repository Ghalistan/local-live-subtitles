<script lang="ts">
  import { onMount } from 'svelte';
  import {
    DEFAULT_SUBTITLE_STYLE,
    getSpeakerColor,
    hexToRgba,
    type SubtitleView,
  } from '../lib/messages';

  let { subscribe }: { subscribe: (cb: (view: SubtitleView) => void) => () => void } = $props();

  let view = $state<SubtitleView>({
    lines: [],
    showFurigana: true,
    style: DEFAULT_SUBTITLE_STYLE,
    visible: false,
  });

  const boxStyle = $derived.by(() => {
    const s = view.style ?? DEFAULT_SUBTITLE_STYLE;
    const rtSize = Math.max(10, Math.round(s.fontSize * 0.5));
    return `--sub-font-size:${s.fontSize}px;--sub-rt-size:${rtSize}px;--sub-color:${s.textColor};--sub-bg:${hexToRgba(s.bgColor, s.bgOpacity)};`;
  });

  onMount(() =>
    subscribe((next) => {
      view = next;
    }),
  );
</script>

{#if view.visible && view.lines.length > 0}
  <div class="subtitle-wrap" role="status" aria-live="polite">
    {#each view.lines as line, idx (line.id)}
      {@const lineHasFurigana = Boolean(
        view.showFurigana && line.tokens && line.tokens.some((t) => Boolean(t.reading)),
      )}
      <span
        class="subtitle-box"
        data-interim={!line.isFinal}
        data-older={idx < view.lines.length - 1}
        data-furigana={lineHasFurigana}
        style={boxStyle}
      >
        {#each line.speakers ?? [] as spk}
          <span class="speaker-tag" style={`color:${getSpeakerColor(spk)}`}>S{spk}</span>
        {/each}
        {#if lineHasFurigana && line.tokens}
          {#each line.tokens as tok}
            {#if tok.reading}
              <ruby>{tok.text}<rt>{tok.reading}</rt></ruby>
            {:else}
              {tok.text}
            {/if}
          {/each}
        {:else}
          {line.text}
        {/if}
      </span>
    {/each}
  </div>
{/if}
