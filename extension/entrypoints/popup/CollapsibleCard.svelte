<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    title: string;
    meta?: string;
    open?: boolean;
    children: Snippet;
  }

  let { title, meta = '', open = $bindable(true), children }: Props = $props();
</script>

<details class="card" bind:open>
  <summary class="card-summary">
    <span class="card-summary-left">
      <span class="card-chevron" aria-hidden="true">▸</span>
      <span class="card-title">{title}</span>
    </span>
    {#if meta}
      <span class="card-meta">{meta}</span>
    {/if}
  </summary>
  <div class="card-body">
    {@render children()}
  </div>
</details>

<style>
  .card {
    border-radius: 7px;
    border: 1px solid #252b3b;
    background: #151821;
    overflow: hidden;
  }

  .card-summary {
    list-style: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 9px 10px;
    cursor: pointer;
    user-select: none;
  }

  .card-summary::-webkit-details-marker {
    display: none;
  }

  .card-summary:hover {
    background: #191e2a;
  }

  .card-summary-left {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }

  .card-chevron {
    display: inline-block;
    font-size: 10px;
    color: #8c95a6;
    transition: transform 0.15s ease;
  }

  .card[open] > .card-summary .card-chevron {
    transform: rotate(90deg);
  }

  .card-title {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: #93c5fd;
  }

  .card-meta {
    font-size: 10px;
    color: #8c95a6;
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }

  .card-body {
    padding: 0 10px 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
</style>
