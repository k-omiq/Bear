<script>
  import Icon from '../icons/Icon.svelte'

  /**
   * A score out of five, drawn as stars. `label` is the whole reading for
   * assistive tech - "Efficiency: 4 of 5" - because five glyphs read aloud
   * one by one say nothing.
   *
   * @type {{ value: number, label: string }}
   */
  let { value, label } = $props()

  const filled = $derived(Math.max(0, Math.min(5, Math.round(value))))
</script>

<span class="stars" role="img" aria-label={label} title={label}>
  {#each [1, 2, 3, 4, 5] as n (n)}
    <span class:on={n <= filled}><Icon name={n <= filled ? 'star-filled' : 'star'} size={12} strokeWidth={1.3} /></span>
  {/each}
</span>

<style>
  .stars { display: inline-flex; gap: 1px; color: var(--line2) }
  .on { color: var(--accent) }
</style>
