<script>
  /**
   * A small busy ring for work that reports no progress of its own: a local
   * measurement, a denoise run that answers only at the end. Decorative; the
   * words beside it say what is happening. Under reduced motion it holds
   * still and pulses instead of turning.
   *
   * @type {{ size?: number }}
   */
  let { size = 14 } = $props()
</script>

<span class="spinner" style:width="{size}px" style:height="{size}px" aria-hidden="true"></span>

<style>
  .spinner {
    display: inline-block;
    flex: none;
    border-radius: var(--r-pill);
    border: 2px solid var(--line2);
    border-top-color: var(--accent);
    animation: denoise-turn .8s linear infinite;
  }
  @keyframes denoise-turn { to { transform: rotate(360deg) } }
  @media (prefers-reduced-motion: reduce) {
    .spinner { animation: mcBlink 1.6s ease-in-out infinite }
  }
</style>
