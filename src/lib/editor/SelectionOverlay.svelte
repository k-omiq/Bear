<script>
  import { colorAt as gradientColor, inPixels, paintGradient, wireGradient } from './gradient.js'

  /**
   * A held Patch or Gradient selection, drawn over its page
   * (`retouch.svelte.js`). Nothing here takes the pointer: the drawing
   * surface under it reads presses against the same geometry
   * (`DrawLayer.svelte#adjustment`), so what is drawn and what is grabbed
   * cannot disagree.
   *
   * - **The selection** as a dashed outline, a brush selection as the path it
   *   swept.
   * - **Patch, Normal mode:** the ghost, the same outline where the source is
   *   (or, for Destination, where the selection goes), dashed in the mark
   *   colour and moved by the drag. While the proposal is on its way there is
   *   no ghost yet. The other modes read no source, so they draw none.
   * - **Gradient:** the line and its two handles, and the preview: the
   *   gradient painted at a fraction of the page's size into a canvas over the
   *   selection's box, cut to the selection's shape and shown at the layer's
   *   opacity. The native render is the commit's; this follows the handles.
   *
   * Geometry is page percent like every draft, drawn in page pixels inside a
   * `scale` so a brush path's width is one number (`DraftPreview.svelte`).
   */

  /**
   * @type {{
   *   held: import('./draft.svelte.js').HeldSelection,
   *   params: Record<string, any>,
   *   pageWidth: number,
   *   pageHeight: number,
   * }}
   */
  let { held, params, pageWidth, pageHeight } = $props()

  /** Longest side of the preview canvas, in its own pixels. */
  const PREVIEW = 256

  const px = (/** @type {{x: number, y: number}} */ point) => ({ x: (point.x / 100) * pageWidth, y: (point.y / 100) * pageHeight })

  /** The selection's outline in page pixels, moved by `offset` (page percent). */
  function outline(/** @type {{x: number, y: number}} */ offset) {
    const shift = (/** @type {{x: number, y: number}} */ point) => px({ x: point.x + offset.x, y: point.y + offset.y })
    if (held.stroke) {
      return {
        kind: 'stroke',
        points: held.stroke.points.map(shift).map((p) => `${p.x},${p.y}`).join(' '),
        width: held.stroke.radius * 2,
      }
    }
    const painted = held.painted
    if (!painted) return null
    const points = painted.points.map(shift)
    if (painted.kind === 'polygon') return { kind: 'polygon', points: points.map((p) => `${p.x},${p.y}`).join(' ') }
    const xs = points.map((p) => p.x)
    const ys = points.map((p) => p.y)
    const box = { x: Math.min(...xs), y: Math.min(...ys), w: Math.max(...xs) - Math.min(...xs), h: Math.max(...ys) - Math.min(...ys) }
    return { kind: painted.kind === 'ellipse' ? 'ellipse' : 'rect', box }
  }

  const own = $derived(outline({ x: 0, y: 0 }))
  const ghost = $derived(held.tool === 'patch' && params.mode === 'normal' && held.offset ? outline(held.offset) : null)
  const line = $derived(held.tool === 'gradient' && held.start && held.end ? { a: px(held.start), b: px(held.end) } : null)

  /** The selection's box in page pixels, whole pixels, for the preview canvas. */
  const box = $derived({
    x: Math.floor((held.bbox.x / 100) * pageWidth),
    y: Math.floor((held.bbox.y / 100) * pageHeight),
    w: Math.max(1, Math.ceil((held.bbox.w / 100) * pageWidth)),
    h: Math.max(1, Math.ceil((held.bbox.h / 100) * pageHeight)),
  })

  /** @type {HTMLCanvasElement|undefined} */
  let canvas = $state()

  $effect(() => {
    if (!canvas || held.tool !== 'gradient' || !held.start || !held.end) return
    const g = inPixels(wireGradient(params, held.start, held.end), pageWidth, pageHeight)
    const scale = Math.min(1, PREVIEW / Math.max(box.w, box.h))
    const width = Math.max(1, Math.round(box.w * scale))
    const height = Math.max(1, Math.round(box.h * scale))
    if (canvas.width !== width) canvas.width = width
    if (canvas.height !== height) canvas.height = height
    const ctx = canvas.getContext('2d')
    if (!ctx) return
    const image = ctx.createImageData(width, height)
    paintGradient(image, g, box, scale)
    ctx.putImageData(image, 0, 0)
    // Cut to the selection: everything outside its shape goes.
    ctx.save()
    ctx.globalCompositeOperation = 'destination-in'
    ctx.setTransform(scale, 0, 0, scale, -box.x * scale, -box.y * scale)
    ctx.fillStyle = '#000'
    ctx.strokeStyle = '#000'
    const shape = own
    ctx.beginPath()
    if (shape?.kind === 'stroke' && held.stroke) {
      ctx.lineWidth = held.stroke.radius * 2
      ctx.lineCap = 'round'
      ctx.lineJoin = 'round'
      held.stroke.points.map(px).forEach((p, i) => (i ? ctx.lineTo(p.x, p.y) : ctx.moveTo(p.x, p.y)))
      ctx.stroke()
    } else if (shape?.kind === 'polygon' && held.painted) {
      held.painted.points.map(px).forEach((p, i) => (i ? ctx.lineTo(p.x, p.y) : ctx.moveTo(p.x, p.y)))
      ctx.closePath()
      ctx.fill('evenodd')
    } else if (shape?.kind === 'ellipse' && shape.box) {
      ctx.ellipse(shape.box.x + shape.box.w / 2, shape.box.y + shape.box.h / 2, shape.box.w / 2, shape.box.h / 2, 0, 0, Math.PI * 2)
      ctx.fill()
    } else if (shape?.box) {
      ctx.rect(shape.box.x, shape.box.y, shape.box.w, shape.box.h)
      ctx.fill()
    }
    ctx.restore()
  })

  /** The two ends' colours, so a handle says which stop it starts from. */
  const ends = $derived.by(() => {
    if (held.tool !== 'gradient' || !held.start || !held.end) return null
    const g = wireGradient(params, held.start, held.end)
    const swatch = (/** @type {number} */ t) => {
      const { color } = gradientColor(g, params.reverse ? 1 - t : t)
      return `rgb(${color.map((v) => Math.round(v)).join(' ')})`
    }
    return { start: swatch(0), end: swatch(1) }
  })
</script>

{#snippet drawn(shape, cls)}
  {#if shape?.kind === 'stroke'}
    <polyline class={cls} points={shape.points} style="stroke-width: {shape.width}" stroke-linecap="round" stroke-linejoin="round" />
  {:else if shape?.kind === 'polygon'}
    <polygon class={cls} points={shape.points} vector-effect="non-scaling-stroke" />
  {:else if shape?.kind === 'ellipse'}
    <ellipse class={cls} cx={shape.box.x + shape.box.w / 2} cy={shape.box.y + shape.box.h / 2} rx={shape.box.w / 2} ry={shape.box.h / 2} vector-effect="non-scaling-stroke" />
  {:else if shape}
    <rect class={cls} x={shape.box.x} y={shape.box.y} width={shape.box.w} height={shape.box.h} vector-effect="non-scaling-stroke" />
  {/if}
{/snippet}

{#if held.tool === 'gradient' && held.start && held.end}
  <canvas
    bind:this={canvas}
    class="preview"
    style:left="{(box.x / pageWidth) * 100}%"
    style:top="{(box.y / pageHeight) * 100}%"
    style:width="{(box.w / pageWidth) * 100}%"
    style:height="{(box.h / pageHeight) * 100}%"
    style:opacity={Math.max(0, Math.min(100, Number(params.opacity ?? 100))) / 100}
    aria-hidden="true"
  ></canvas>
{/if}

<svg class="overlay" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
  <g transform="scale({100 / pageWidth} {100 / pageHeight})">
    {#if own?.kind !== 'stroke'}
      {@render drawn(own, 'edge under')}
    {/if}
    {@render drawn(own, 'edge')}
    {#if ghost}
      {@render drawn(ghost, 'ghost')}
    {/if}
    {#if line}
      <line class="line under" x1={line.a.x} y1={line.a.y} x2={line.b.x} y2={line.b.y} vector-effect="non-scaling-stroke" />
      <line class="line" x1={line.a.x} y1={line.a.y} x2={line.b.x} y2={line.b.y} vector-effect="non-scaling-stroke" />
    {/if}
  </g>
</svg>

{#if held.tool === 'gradient' && held.start && held.end && ends}
  <span class="handle" style:left="{held.start.x}%" style:top="{held.start.y}%" style:background={ends.start} aria-hidden="true"></span>
  <span class="handle" style:left="{held.end.x}%" style:top="{held.end.y}%" style:background={ends.end} aria-hidden="true"></span>
{/if}

<style>
  .overlay,
  .preview {
    position: absolute;
    z-index: var(--z-draw);
    pointer-events: none;
  }

  .overlay {
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
  }

  .edge,
  .ghost,
  .line {
    fill: none;
    stroke-width: 1.5;
  }

  /* Marching ants without the march: a light line under a dashed mark-colour
     one, so the outline reads over black ink and over white paper alike. */
  .under {
    stroke: var(--paper);
    stroke-width: 3;
    opacity: .85;
  }

  polyline.edge,
  polyline.ghost {
    stroke-width: unset;
    opacity: .25;
    stroke: var(--page-mark);
  }

  .edge:not(.under) {
    stroke: var(--page-mark);
    stroke-dasharray: 5 4;
  }

  .ghost {
    stroke: var(--page-mark);
    stroke-dasharray: 2 3;
    opacity: .9;
  }

  .line:not(.under) {
    stroke: var(--page-mark);
  }

  .handle {
    position: absolute;
    z-index: var(--z-draw);
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid var(--page-mark);
    box-shadow: 0 0 0 1px var(--paper);
    transform: translate(-50%, -50%);
    pointer-events: none;
  }
</style>
