<script>
  // The app's name as bubble letters, drawn here rather than set in a font,
  // the same way Manga Typesetter draws its own.
  //
  // Each letter is a few centre lines stroked thick with round caps and joins,
  // so every end and corner comes out puffy. Letters overlap a little and tilt
  // a few degrees each way, like stickers. Drawn per letter in three layers: a
  // ring in the page colour, which keeps a letter apart from the one it sits
  // on; the body in the theme's accent; a small shine at the top left. One
  // soft shadow under the whole word sits on the ground and stays put while
  // the letters hop on hover.
  //
  // Geometry is in viewBox units: x-height from y 18 to 42, bowls of radius 12
  // round y 30, strokes 10 wide.
  let { label } = $props()

  const W = 10
  const THIN = 4 // the e's bar, so its eye stays open
  const RING = 5

  const bowl = (cx) => `M${cx - 12} 30a12 12 0 1 1 24 0a12 12 0 1 1 -24 0`
  // A short arc just outside a bowl's top-left, from 235 to 265 degrees.
  const shine = (cx) => `M${cx - 7.75} 18.94A13.5 13.5 0 0 1 ${cx - 1.18} 16.55`
  // A short line on a stem's left edge, for letters with no bowl.
  const stemShine = (x) => `M${x - 1.5} 36V31`

  /** @type {Record<string, (cx: number) => {strokes: {d: string, thin?: boolean}[], shine: string}>} */
  const GLYPHS = {
    a: (cx) => ({ strokes: [{ d: bowl(cx) }, { d: `M${cx + 12} 18V42` }], shine: shine(cx) }),
    c: (cx) => ({
      // Open to the right, 45 degrees either side of the middle.
      strokes: [{ d: `M${cx + 8.5} 21.5A12 12 0 1 0 ${cx + 8.5} 38.5` }],
      shine: shine(cx),
    }),
    e: (cx) => ({
      // The bowl runs from the bar's right end, up and round, and stops 65
      // degrees below it so the mouth stays open.
      strokes: [{ d: `M${cx + 12} 30A12 12 0 1 0 ${cx + 5.07} 40.88` }, { d: `M${cx - 11} 32H${cx + 11}`, thin: true }],
      shine: shine(cx),
    }),
    g: (cx) => ({
      strokes: [{ d: bowl(cx) }, { d: `M${cx + 12} 18V44C${cx + 12} 55 ${cx + 2} 58 ${cx - 8} 54` }],
      shine: shine(cx),
    }),
    l: (cx) => ({ strokes: [{ d: `M${cx} 5V42` }], shine: `M${cx - 1.5} 16V9` }),
    m: (cx) => ({
      strokes: [
        { d: `M${cx - 18} 18V42` },
        { d: `M${cx - 18} 27A9 9 0 0 1 ${cx} 27V42` },
        { d: `M${cx} 27A9 9 0 0 1 ${cx + 18} 27V42` },
      ],
      shine: stemShine(cx - 18),
    }),
    n: (cx) => ({
      strokes: [{ d: `M${cx - 10} 18V42` }, { d: `M${cx - 10} 28A10 10 0 0 1 ${cx + 10} 28V42` }],
      shine: stemShine(cx - 10),
    }),
    r: (cx) => ({ strokes: [{ d: `M${cx - 5} 42V31C${cx - 5} 23 ${cx} 18 ${cx + 8} 18` }], shine: stemShine(cx - 5) }),
  }

  // Centres are spaced so neighbours overlap by about five units, with a
  // word gap after "manga".
  const LETTERS = [
    ['m', 23, -5], ['a', 58, 4], ['n', 85, -3], ['g', 112, 5], ['a', 141, -4],
    ['c', 191, 4], ['l', 209, -3], ['e', 227, 5], ['a', 256, -4], ['n', 283, 3], ['e', 310, -5], ['r', 332, 6],
  ].map(([glyph, cx, tilt]) => ({ cx, tilt, ...GLYPHS[glyph](cx) }))

  const width = (s) => (s.thin ? THIN : W)
  const tilt = (l) => `rotate(${l.tilt} ${l.cx} 30)`
</script>

<svg class="wordmark" viewBox="-4 -4 354 70" role="img" aria-label={label}>
  <g class="shadow" transform="translate(0 2.6)">
    {#each LETTERS as l}
      <g transform={tilt(l)}>
        {#each l.strokes as s}<path d={s.d} stroke-width={width(s)} />{/each}
      </g>
    {/each}
  </g>
  {#each LETTERS as l, i}
    <g transform={tilt(l)}>
      <g class="letter" style="animation-delay:{i * 40}ms">
        {#each l.strokes as s}<path class="ring" d={s.d} stroke-width={width(s) + RING} />{/each}
        {#each l.strokes as s}<path class="body" d={s.d} stroke-width={width(s)} />{/each}
        <path class="shine" d={l.shine} />
      </g>
    </g>
  {/each}
</svg>

<style>
  .wordmark {
    display: block;
    height: 40px;
    width: auto;
    overflow: visible;
  }
  path {
    fill: none;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .shadow { stroke: var(--accent); opacity: .2; }
  .ring { stroke: var(--bg); }
  .body { stroke: var(--accent); }
  /* Always white: on a pale accent it fades out rather than reading as a dent. */
  .shine { stroke: #fff; stroke-width: 2.6; opacity: .6; }

  .wordmark:hover .letter { animation: hop .5s cubic-bezier(.3, .7, .4, 1.5) both; }
  @keyframes hop {
    0%, 100% { transform: translateY(0); }
    40% { transform: translateY(-3.5px); }
  }
  @media (prefers-reduced-motion: reduce) {
    .wordmark:hover .letter { animation: none; }
  }
</style>
