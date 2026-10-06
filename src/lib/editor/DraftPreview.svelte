<script>
  import { draft } from './draft.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { draftMaskColor } from '../model/masks.js'
  import { AI_STROKE_PX, paintedStroke, polylinePoints } from './gesture.js'
  import { t } from '../i18n/index.js'

  /**
   * What a gesture looks like while it is still a gesture: the mask it is
   * about to make, the path that described it, and - for Clone / heal - where
   * the stamp is reading from.
   *
   * **A stroke previews as the stroke.** It used to preview as its bounding
   * box, on the reasoning that a region *is* a bbox and showing an outline the
   * commit would not produce would be the lie. That reasoning was sound and its
   * premise was the bug: the commit produced a rectangle because the seam only
   * carried one. Now that the painted shape
   * crosses the seam, the honest preview of a brush stroke is the swept disc.
   * A Shapes gesture previews as the shape it actually sends: the rectangle
   * for a rectangle drag, the ellipse for an ellipse, and the closed outline
   * for a lasso or a polygon - which used to be drawn as the box around them
   * and no longer commits as one (`drawing.svelte.js#paintedShape`).
   *
   * The SVG's user units *are* page percent, which is the coordinate space
   * `gesture.js` works in, so nothing here converts anything - except the
   * stroke, whose group is scaled into **page pixels** so that a round cap is
   * round: a brush is circular in the page's own pixels, and the sheet's
   * non-square aspect turns a percent-space circle into an ellipse on screen.
   * Hairlines carry `vector-effect="non-scaling-stroke"` so they stay hairlines
   * at every zoom; the swept stroke deliberately does not, because its width is
   * the thing being shown.
   */

  /** @type {{pageId: string, pageWidth?: number, pageHeight?: number, regions?: ReadonlyArray<import('../model/types.js').Region>}} */
  let { pageId, pageWidth = 1600, pageHeight = 2400, regions = [] } = $props()

  const uid = $props.id()

  const active = $derived(draft.active?.pageId === pageId ? draft.active : null)
  const bbox = $derived(active?.bbox ?? null)
  const source = $derived(
    editor.tool === 'cloneHeal' && draft.cloneSource?.pageId === pageId ? draft.cloneSource : null,
  )
  /**
   * Paint and clone / heal preview as **pixels** now (`PaintLayer.svelte`), so
   * the tinted capsule that stood in for them would only sit on top of the real
   * thing and mute it. The dashed cursor ring is `DrawLayer`'s and stays: it
   * says where the next stamp lands, which the painted pixels behind it cannot.
   */
  const livePixels = $derived(active?.tool === 'cloneHeal' || active?.tool === 'brush')

  /**
   * The **path the pointer took**, drawn as a hairline over the shape it
   * describes. It belongs to the two shapes that are being *built* out of
   * clicks - a lasso and a polygon - where the vertices so far are the only
   * sign of progress.
   *
   * It is deliberately not drawn over a `stroke`. The swept capsule already
   * says exactly what the stroke covers, and a 1px `--page-mark` line down the
   * middle of it read as a nib mark trailing the pointer - a second, sharper
   * shape at the same place as the real one, describing nothing the capsule
   * did not already describe.
   */
  const path = $derived(
    active &&
    active.points.length > 1 &&
    (active.kind === 'lasso' || active.kind === 'polygon') &&
    !livePixels
      ? polylinePoints(active.points)
      : '',
  )

  /**
   * The swept stroke, in page pixels: the same path and the same radius
   * `drawing.svelte.js#strokeOf` sends across the seam, read from the same
   * parameters, so what is drawn here is what will be cleaned.
   *
   * **Only the AI mask brush reaches this now.** It is the one stroke tool
   * whose commit is a *region* rather than pixels, so a tinted capsule is the
   * honest preview of it. The Brush and Clone / heal are `livePixels` and draw
   * their real colours in `PaintLayer.svelte`; the colour and opacity this
   * block used to paint the capsule with were left over from the Brush's mask
   * modes and could not be reached once the Brush became paint-only.
   */
  const swept = $derived.by(() => {
    if (active?.kind !== 'stroke' || livePixels) return null
    const params = editor.toolParams[active.tool] ?? {}
    const size = Number(params.size ?? (active.tool === 'aiMaskBrush' ? AI_STROKE_PX : 0))
    const stroke = paintedStroke(active.points, size)
    if (!stroke) return null
    return {
      width: stroke.radius * 2,
      points: stroke.points
        .map((point) => `${(point.x / 100) * pageWidth},${(point.y / 100) * pageHeight}`)
        .join(' '),
      // A one-point stroke has no polyline to draw, so it is a dot.
      dot: stroke.points.length < 2 ? stroke.points[0] : null,
    }
  })

  /**
   * A closed outline, for the two shapes that are one: a lasso is a freehand
   * polygon and a clicked polygon is the same thing with fewer samples. Both
   * used to preview as the **rectangle around them** with the path drawn on
   * top, which is exactly what the commit no longer does
   * (`drawing.svelte.js#paintedShape`) - so the preview was showing the old
   * defect while the seam had stopped producing it.
   */
  const closed = $derived(
    (active?.kind === 'lasso' || active?.kind === 'polygon') && (active?.points.length ?? 0) >= 3
      ? polylinePoints(active.points)
      : '',
  )

  /**
   * A Shapes gesture set to a solid colour previews **in that colour**: it is
   * paint rather than a clean, and the blue mark tint would say "an engine will
   * decide what goes here", which is the one thing it will not do.
   */
  const solidFill = $derived(
    active?.tool === 'shapes' && editor.toolParams.shapes?.mode === 'solid'
      ? {
          color: String(editor.toolParams.shapes?.color ?? '#000000'),
          opacity:
            Math.max(0, Math.min(100, Number(editor.toolParams.shapes?.opacity ?? 100))) / 100,
        }
      : null,
  )
  /**
   * The selection tool previews in the colour its masks are drawn in
   * (`DetectionMasks.svelte`), so what is being added looks like what it will
   * join. Remove is the opposite act and must not look like more mask: an
   * area is a dashed outline with no fill, and a brush stroke is hatched.
   */
  const maskDraft = $derived(active?.tool === 'maskSelect')
  const cutting = $derived(maskDraft && active?.mode === 'erase')
  const maskFill = $derived(Math.max(0, Math.min(100, Number(session.maskOpacity ?? 35))) / 100)
  /**
   * The colour of the detection the gesture would join, speech bubble or
   * outside; over none, the speech bubble colour, which every region of
   * unknown place takes (`model/masks.js#draftMaskColor`).
   */
  const maskColor = $derived(draftMaskColor(regions, bbox, session))
  const maskStyle = $derived(
    maskDraft
      ? cutting
        ? `fill: none; stroke: ${maskColor};`
        : `fill: ${maskColor}; fill-opacity: ${maskFill}; stroke: ${maskColor};`
      : undefined,
  )
  /** The hatch a removing stroke is drawn with, in page pixels like the stroke. */
  const hatchGap = $derived(Math.max(6, (swept?.width ?? 0) / 3))

  const shapeStyle = $derived(maskStyle)

  /**
   * A solid shape, in **page pixels**, drawn the way `region.rs#shape_coverage`
   * rasterises it: the outline is an inner stroke `outlineWidth` page pixels
   * wide (a stroke twice that, clipped to the shape), a feather is a Gaussian
   * of sigma `feather / 2`, and a line is a round-capped stroke as wide as the
   * outline row says. None of it is `non-scaling-stroke`: every width is a
   * distance on the page, so it grows and shrinks with the zoom exactly as
   * the committed pixels will.
   */
  const solidShape = $derived.by(() => {
    if (!solidFill || !bbox || livePixels || !active) return null
    const params = editor.toolParams.shapes ?? {}
    const outline = Math.max(0, Math.min(30, Number(params.outlineWidth ?? 0) || 0))
    const feather = Math.max(0, Math.min(20, Number(params.feather ?? 0) || 0))
    const px = (/** @type {{x: number, y: number}} */ point) => ({
      x: (point.x / 100) * pageWidth,
      y: (point.y / 100) * pageHeight,
    })
    if (active.kind === 'line') {
      if (active.points.length < 2) return null
      const [a, b] = [px(active.points[0]), px(active.points.at(-1))]
      return { kind: 'line', a, b, width: Math.max(1, outline), outline: 0, blur: 0, box: null }
    }
    const box = {
      x: (bbox.x / 100) * pageWidth,
      y: (bbox.y / 100) * pageHeight,
      w: (bbox.w / 100) * pageWidth,
      h: (bbox.h / 100) * pageHeight,
    }
    const blur = feather / 2
    if (active.kind === 'lasso' || active.kind === 'polygon') {
      if (active.points.length < 3) return null
      const points = active.points.map(px).map((point) => `${point.x},${point.y}`).join(' ')
      return { kind: 'polygon', points, box, outline, blur }
    }
    return { kind: active.kind === 'ellipse' ? 'ellipse' : 'rect', box, outline, blur }
  })
  /** The swept stroke's own paint, where the selection tool sets it. */
  const sweptStyle = $derived(
    !maskDraft
      ? ''
      : cutting
        ? `stroke: url(#${uid}-hatch);`
        : `stroke: ${maskColor}; stroke-opacity: ${maskFill};`,
  )
</script>

<!-- One solid shape's geometry in page pixels: its fill, its outline and the
     clip that keeps the outline inside it are all this. A lasso fills
     even-odd, the rule the backend's `contains_point` uses. -->
{#snippet geometry(shape, style, clip = undefined)}
  {#if shape.kind === 'ellipse'}
    <ellipse
      cx={shape.box.x + shape.box.w / 2}
      cy={shape.box.y + shape.box.h / 2}
      rx={shape.box.w / 2}
      ry={shape.box.h / 2}
      clip-path={clip}
      {style}
    />
  {:else if shape.kind === 'polygon'}
    <polygon points={shape.points} fill-rule="evenodd" clip-rule="evenodd" clip-path={clip} {style} />
  {:else}
    <rect x={shape.box.x} y={shape.box.y} width={shape.box.w} height={shape.box.h} clip-path={clip} {style} />
  {/if}
{/snippet}

<svg class="preview" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true" style:--mask-color={maskColor}>
  {#if source}
    <g class="source">
      <circle cx={source.x} cy={source.y} r="1.2" vector-effect="non-scaling-stroke" />
      <line x1={source.x - 2.4} y1={source.y} x2={source.x + 2.4} y2={source.y}
            vector-effect="non-scaling-stroke" />
      <line x1={source.x} y1={source.y - 2.4} x2={source.x} y2={source.y + 2.4}
            vector-effect="non-scaling-stroke" />
    </g>
  {/if}

  {#if swept}
    <g transform="scale({100 / pageWidth} {100 / pageHeight})">
      {#if cutting}
        <defs>
          <pattern
            id="{uid}-hatch"
            patternUnits="userSpaceOnUse"
            width={hatchGap}
            height={hatchGap}
            patternTransform="rotate(45)"
          >
            <rect width={hatchGap / 2} height={hatchGap} style="fill: {maskColor}" />
          </pattern>
        </defs>
      {/if}
      {#if swept.dot}
        <circle
          class="shape"
          class:cut={cutting}
          cx={(swept.dot.x / 100) * pageWidth}
          cy={(swept.dot.y / 100) * pageHeight}
          r={swept.width / 2}
          style={maskDraft ? (cutting ? `fill: url(#${uid}-hatch); stroke: none` : `fill: ${maskColor}; fill-opacity: ${maskFill}; stroke: none`) : undefined}
        />
      {:else}
        <!-- The width is a `style` and not an attribute on purpose: the
             stylesheet's `.shape { stroke-width }` would win over a
             presentation attribute and draw every stroke as a hairline. -->
        <polyline
          class="shape swept"
          class:cut={cutting}
          points={swept.points}
          style="stroke-width: {swept.width}; {sweptStyle}"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      {/if}
    </g>
  {:else if solidShape}
    {@const shape = solidShape}
    <g transform="scale({100 / pageWidth} {100 / pageHeight})">
      <defs>
        {#if shape.blur > 0 && shape.box}
          <!-- Room for three sigma past the box; the default filter region is
               a tenth of it and would crop a wide feather. -->
          <filter
            id="{uid}-feather"
            filterUnits="userSpaceOnUse"
            x={shape.box.x - shape.blur * 4}
            y={shape.box.y - shape.blur * 4}
            width={shape.box.w + shape.blur * 8}
            height={shape.box.h + shape.blur * 8}
          >
            <feGaussianBlur stdDeviation={shape.blur} />
          </filter>
        {/if}
        {#if shape.outline > 0}
          <clipPath id="{uid}-inside">{@render geometry(shape, '')}</clipPath>
        {/if}
      </defs>
      <g
        style="opacity: {solidFill?.opacity ?? 1}"
        filter={shape.blur > 0 ? `url(#${uid}-feather)` : undefined}
      >
        {#if shape.kind === 'line'}
          <line
            x1={shape.a.x}
            y1={shape.a.y}
            x2={shape.b.x}
            y2={shape.b.y}
            stroke-linecap="round"
            style="stroke: {solidFill?.color}; stroke-width: {shape.width}"
          />
        {:else}
          {@render geometry(shape, `fill: ${solidFill?.color}; stroke: none`)}
          {#if shape.outline > 0}
            {@render geometry(
              shape,
              `fill: none; stroke: ${editor.toolParams.shapes?.outlineColor ?? '#000000'}; stroke-width: ${shape.outline * 2}`,
              `url(#${uid}-inside)`,
            )}
          {/if}
        {/if}
      </g>
    </g>
  {:else if bbox && !livePixels}
    {#if active?.kind === 'line' && (active?.points.length ?? 0) >= 2}
      <line class="shape" x1={active.points[0].x} y1={active.points[0].y}
        x2={active.points.at(-1).x} y2={active.points.at(-1).y}
        vector-effect="non-scaling-stroke" style="stroke: var(--page-mark-line)" />
    {:else if active?.kind === 'ellipse'}
      {#if cutting}
        <ellipse class="cut-under" cx={bbox.x + bbox.w / 2} cy={bbox.y + bbox.h / 2} rx={bbox.w / 2} ry={bbox.h / 2}
          vector-effect="non-scaling-stroke" />
      {/if}
      <ellipse
        class="shape"
        class:cut={cutting}
        cx={bbox.x + bbox.w / 2}
        cy={bbox.y + bbox.h / 2}
        rx={bbox.w / 2}
        ry={bbox.h / 2}
        vector-effect="non-scaling-stroke"
        style={shapeStyle}
      />
    {:else if closed}
      {#if cutting}
        <polygon class="cut-under" points={closed} vector-effect="non-scaling-stroke" />
      {/if}
      <polygon class="shape" class:cut={cutting} points={closed} vector-effect="non-scaling-stroke" style={shapeStyle} />
    {:else if active?.kind !== 'lasso' && active?.kind !== 'polygon' && active?.kind !== 'line'}
      {#if cutting}
        <rect class="cut-under" x={bbox.x} y={bbox.y} width={bbox.w} height={bbox.h} vector-effect="non-scaling-stroke" />
      {/if}
      <rect
        class="shape"
        class:cut={cutting}
        x={bbox.x}
        y={bbox.y}
        width={bbox.w}
        height={bbox.h}
        vector-effect="non-scaling-stroke"
        style={shapeStyle}
      />
    {/if}
  {/if}

  {#if path}
    {#if cutting}
      <polyline class="cut-under" points={path} vector-effect="non-scaling-stroke" />
    {/if}
    <polyline
      class="path"
      class:closing={active?.kind === 'polygon'}
      class:cut={cutting}
      points={path}
      vector-effect="non-scaling-stroke"
    />
  {/if}
</svg>

<!-- The gesture's size in words as well as in pixels, and the one number on
     the sheet that changes while a gesture runs. Polite rather than assertive:
     it should not interrupt, and a drag produces a great many of them. -->
{#if bbox}
  <p class="readout" aria-live="polite">
    {t('canvas.draft.size', { w: Math.round(bbox.w), h: Math.round(bbox.h) })}
  </p>
{/if}

<style>
  .preview {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
    overflow: visible;
  }

  /* Blue, not ink: the artwork under the preview is black ink, and a dark
     draft over a dark panel is a draft the user cannot see. */
  .shape {
    fill: var(--page-mark-tint);
    stroke: var(--page-mark-line);
    stroke-width: 1.5;
  }

  /* A swept stroke has no outline to draw: the stroke *is* the area, so the
     tint moves from the fill to the stroke and the width comes from the
     brush rather than from here. */
  .shape.swept {
    fill: none;
    stroke: var(--page-mark-tint);
  }

  .path {
    fill: none;
    stroke: var(--page-mark);
    stroke-width: 1;
    stroke-linejoin: round;
    stroke-linecap: round;
    opacity: .7;
  }

  /* A polygon that has not been closed yet says so by being dashed. */
  .path.closing { stroke-dasharray: 3 2 }

  /* The selection tool's remove: an outline that is only dashes, over nothing,
     reads as the area being taken away rather than added. */
  .shape.cut:not(.swept) { stroke-dasharray: 4 3 }

  /* Under those dashes, a dark line of its own, so the gaps read as dark
     over a mask drawn in the same colour and the dashes read over dark art:
     the marching ants of every selection tool. */
  .cut-under {
    fill: none;
    stroke: var(--page-line);
    stroke-width: 3;
    stroke-linejoin: round;
  }
  .path.cut {
    stroke: var(--mask-color, var(--page-mark));
    stroke-width: 1.5;
    stroke-dasharray: 4 3;
    opacity: 1;
  }

  .source {
    fill: none;
    stroke: var(--page-mark);
    stroke-width: 1.25;
  }

  .readout {
    position: absolute;
    right: 6px;
    bottom: 5px;
    margin: 0;
    padding: 2px 6px;
    border-radius: var(--r-xs);
    background: var(--paper);
    border: 1px solid var(--page-line);
    color: var(--page-ink);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: max(8px, 2.15cqw);
    line-height: 1.5;
    pointer-events: none;
    animation: mcFade var(--dur-fast) var(--ease);
  }
</style>
