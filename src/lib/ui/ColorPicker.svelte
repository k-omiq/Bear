<script>
  import { anchoredOverlay } from './overlay.svelte.js'
  import { focusable } from './focus.js'
  import {
    clamp,
    hexInvalid,
    hexOnCommit,
    hexOnInput,
    hexToRgb,
    hsbToHex,
    hsbToRgb,
    rgbToHex,
    rgbToHsb,
  } from './color.js'
  import Icon from '../icons/Icon.svelte'
  import { t } from '../i18n/index.js'
  import { screenColorPicker } from '../api/screencolor.js'

  /**
   * Custom Svelte Color Picker.
   *
   * Composition:
   * - Trigger: compact 26x26 toolbar swatch button that opens an anchored popover.
   * - Popover panel:
   *   - Large saturation/brightness square with draggable target and keyboard support.
   *   - Narrow vertical rainbow hue rail with draggable thumb and keyboard support.
   *   - Preview swatch and, where the platform can sample the screen, an Eyedropper button.
   *   - Numeric H / S / B inputs.
   *   - Numeric R / G / B inputs.
   *   - Editable HEX input with validation and normalization to lowercase #rrggbb.
   *
   * @type {{
   *   value?: string,
   *   label?: string,
   *   align?: 'start' | 'end',
   *   unclipped?: boolean,
   *   onchange: (hex: string) => void,
   * }}
   */
  let {
    value = '#ffffff',
    label = t('tools.param.color'),
    align = 'start',
    unclipped = false,
    onchange,
  } = $props()

  /** @type {HTMLElement | undefined} */
  let root = $state()
  /** @type {HTMLElement | undefined} */
  let panel = $state()
  let flippedY = $state(false)
  let floating = $state({ x: 0, y: 0 })

  const panelId = $props.id()
  const overlay = anchoredOverlay(() => root, () => unclipped ? panel : undefined)
  const open = $derived(overlay.open)

  /** @param {HTMLElement} node @param {boolean} floating */
  function portal(node, floating) {
    const home = document.createComment('picker home')
    node.before(home)
    /** @param {boolean} detached */
    const move = (detached) => {
      if (detached) document.body.appendChild(node)
      else home.after(node)
    }
    move(floating)
    return {
      update: move,
      destroy: () => { node.remove(); home.remove() },
    }
  }

  // Internal state for HSB and RGB
  let h = $state(0)
  let s = $state(0)
  let b = $state(100)
  let hexText = $state('#ffffff')
  let isEditingHex = $state(false)

  // Sync state from incoming prop value when not actively typing hex
  $effect(() => {
    const committed = hexOnCommit(value) ?? '#ffffff'
    if (!isEditingHex) {
      hexText = committed
    }
    const rgb = hexToRgb(committed)
    if (rgb) {
      const hsb = rgbToHsb(rgb.r, rgb.g, rgb.b)
      // Only overwrite hue if saturation and brightness are non-zero,
      // so gray/white/black don't discard current hue while dragging.
      if (hsb.s > 0 && hsb.b > 0) {
        h = hsb.h
      }
      s = hsb.s
      b = hsb.b
    }
  })

  $effect(() => {
    if (!open || !unclipped || !root || !panel) return
    const position = () => {
      const trigger = root.getBoundingClientRect()
      const width = panel.offsetWidth || 260
      const height = panel.offsetHeight || 360
      const vw = globalThis.innerWidth || width + 16
      const vh = globalThis.innerHeight || height + 16
      const below = trigger.bottom + 8
      const above = trigger.top - height - 8
      floating = {
        x: Math.max(8, Math.min(vw - width - 8, align === 'end' ? trigger.right - width : trigger.left)),
        y: below + height <= vh - 8 ? below : above >= 8 ? above : Math.max(8, vh - height - 8),
      }
    }
    position()
    globalThis.addEventListener?.('resize', position)
    globalThis.addEventListener?.('scroll', position, true)
    return () => {
      globalThis.removeEventListener?.('resize', position)
      globalThis.removeEventListener?.('scroll', position, true)
    }
  })

  const currentRgb = $derived(hsbToRgb(h, s, b))
  const currentHex = $derived(rgbToHex(currentRgb.r, currentRgb.g, currentRgb.b))

  $effect(() => {
    if (!open) {
      flippedY = false
      isEditingHex = false
      return
    }
    const node = panel
    if (!node) return
    focusable(node)[0]?.focus()

    const box = node.getBoundingClientRect()
    const vh = globalThis.innerHeight ?? 0
    if (vh && box.bottom > vh - 8 && box.top - box.height > 0) {
      flippedY = true
    } else {
      flippedY = false
    }
  })

  /** @type {HTMLDivElement | undefined} */
  let sbArea = $state()
  /** @type {HTMLDivElement | undefined} */
  let hueRail = $state()
  let isDraggingSb = $state(false)
  let isDraggingHue = $state(false)

  function emitChange(newHex) {
    hexText = newHex
    onchange(newHex)
  }

  // --- Saturation / Brightness Drag Handlers ---
  function updateSbFromPointer(event) {
    if (!sbArea) return
    const rect = sbArea.getBoundingClientRect()
    const x = clamp(event.clientX - rect.left, 0, rect.width)
    const y = clamp(event.clientY - rect.top, 0, rect.height)
    s = Math.round((x / rect.width) * 100)
    b = Math.round((1 - y / rect.height) * 100)
    emitChange(hsbToHex(h, s, b))
  }

  function onSbPointerDown(event) {
    if (event.button !== 0) return
    isDraggingSb = true
    sbArea?.setPointerCapture(event.pointerId)
    updateSbFromPointer(event)
  }

  function onSbPointerMove(event) {
    if (!isDraggingSb) return
    updateSbFromPointer(event)
  }

  function onSbPointerUp(event) {
    if (isDraggingSb) {
      isDraggingSb = false
      try {
        sbArea?.releasePointerCapture(event.pointerId)
      } catch {
        /* pointer already released */
      }
    }
  }

  function onSbKeyDown(event) {
    const step = event.shiftKey ? 10 : 1
    let handled = true
    if (event.key === 'ArrowLeft') {
      s = clamp(s - step, 0, 100)
    } else if (event.key === 'ArrowRight') {
      s = clamp(s + step, 0, 100)
    } else if (event.key === 'ArrowDown') {
      b = clamp(b - step, 0, 100)
    } else if (event.key === 'ArrowUp') {
      b = clamp(b + step, 0, 100)
    } else {
      handled = false
    }
    if (handled) {
      event.preventDefault()
      emitChange(hsbToHex(h, s, b))
    }
  }

  // --- Hue Rail Drag Handlers ---
  function updateHueFromPointer(event) {
    if (!hueRail) return
    const rect = hueRail.getBoundingClientRect()
    const y = clamp(event.clientY - rect.top, 0, rect.height)
    h = Math.round((y / rect.height) * 360) % 360
    emitChange(hsbToHex(h, s, b))
  }

  function onHuePointerDown(event) {
    if (event.button !== 0) return
    isDraggingHue = true
    hueRail?.setPointerCapture(event.pointerId)
    updateHueFromPointer(event)
  }

  function onHuePointerMove(event) {
    if (!isDraggingHue) return
    updateHueFromPointer(event)
  }

  function onHuePointerUp(event) {
    if (isDraggingHue) {
      isDraggingHue = false
      try {
        hueRail?.releasePointerCapture(event.pointerId)
      } catch {
        /* pointer already released */
      }
    }
  }

  function onHueKeyDown(event) {
    const step = event.shiftKey ? 10 : 1
    let handled = true
    if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') {
      h = (((h - step) % 360) + 360) % 360
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowRight') {
      h = (((h + step) % 360) + 360) % 360
    } else {
      handled = false
    }
    if (handled) {
      event.preventDefault()
      emitChange(hsbToHex(h, s, b))
    }
  }

  // --- Numeric HSB Inputs ---
  function onHInput(e) {
    const val = Number.parseInt(e.currentTarget.value.replace(/[^0-9]/g, ''), 10)
    if (!Number.isNaN(val)) {
      h = clamp(val, 0, 360) % 360
      emitChange(hsbToHex(h, s, b))
    }
  }

  function onSInput(e) {
    const val = Number.parseInt(e.currentTarget.value.replace(/[^0-9]/g, ''), 10)
    if (!Number.isNaN(val)) {
      s = clamp(val, 0, 100)
      emitChange(hsbToHex(h, s, b))
    }
  }

  function onBInput(e) {
    const val = Number.parseInt(e.currentTarget.value.replace(/[^0-9]/g, ''), 10)
    if (!Number.isNaN(val)) {
      b = clamp(val, 0, 100)
      emitChange(hsbToHex(h, s, b))
    }
  }

  // --- Numeric RGB Inputs ---
  function onRInput(e) {
    const val = Number.parseInt(e.currentTarget.value.replace(/[^0-9]/g, ''), 10)
    if (!Number.isNaN(val)) {
      const r = clamp(val, 0, 255)
      const hsb = rgbToHsb(r, currentRgb.g, currentRgb.b)
      if (hsb.s > 0 && hsb.b > 0) h = hsb.h
      s = hsb.s
      b = hsb.b
      emitChange(rgbToHex(r, currentRgb.g, currentRgb.b))
    }
  }

  function onGInput(e) {
    const val = Number.parseInt(e.currentTarget.value.replace(/[^0-9]/g, ''), 10)
    if (!Number.isNaN(val)) {
      const g = clamp(val, 0, 255)
      const hsb = rgbToHsb(currentRgb.r, g, currentRgb.b)
      if (hsb.s > 0 && hsb.b > 0) h = hsb.h
      s = hsb.s
      b = hsb.b
      emitChange(rgbToHex(currentRgb.r, g, currentRgb.b))
    }
  }

  function onBValInput(e) {
    const val = Number.parseInt(e.currentTarget.value.replace(/[^0-9]/g, ''), 10)
    if (!Number.isNaN(val)) {
      const bComp = clamp(val, 0, 255)
      const hsb = rgbToHsb(currentRgb.r, currentRgb.g, bComp)
      if (hsb.s > 0 && hsb.b > 0) h = hsb.h
      s = hsb.s
      b = hsb.b
      emitChange(rgbToHex(currentRgb.r, currentRgb.g, bComp))
    }
  }

  // --- Hex Field Handlers ---
  function onHexChange(e) {
    isEditingHex = true
    hexText = e.currentTarget.value
    const parsed = hexOnInput(hexText)
    if (parsed) {
      const rgb = hexToRgb(parsed)
      if (rgb) {
        const hsb = rgbToHsb(rgb.r, rgb.g, rgb.b)
        if (hsb.s > 0 && hsb.b > 0) h = hsb.h
        s = hsb.s
        b = hsb.b
        onchange(parsed)
      }
    }
  }

  function commitHexValue(rawText) {
    isEditingHex = false
    const committed = hexOnCommit(rawText)
    if (committed) {
      const rgb = hexToRgb(committed)
      if (rgb) {
        const hsb = rgbToHsb(rgb.r, rgb.g, rgb.b)
        if (hsb.s > 0 && hsb.b > 0) h = hsb.h
        s = hsb.s
        b = hsb.b
        emitChange(committed)
      }
    } else {
      hexText = currentHex
    }
  }

  const pickScreenColor = screenColorPicker()

  async function pickFromScreen() {
    try {
      const hex = await pickScreenColor?.()
      if (hex) {
        commitHexValue(hex)
      }
    } catch {
      /* the sampler failed to open; the value is unchanged */
    }
  }
</script>

{#snippet pickerPanel()}
    <div
      bind:this={panel}
      use:portal={unclipped}
      id={panelId}
      class="picker-panel"
      class:end={align === 'end'}
      class:flipped-y={flippedY}
      class:unclipped
      role="dialog"
      tabindex="-1"
      aria-label={label}
      style:left={unclipped ? `${floating.x}px` : undefined}
      style:top={unclipped ? `${floating.y}px` : undefined}
      onkeydown={(event) => { if (unclipped) overlay.dismissKey(event) }}
      onfocusout={(event) => { if (unclipped) overlay.focusOut(event) }}
    >
      <!-- Visual Picker Area: SB Square + Hue Rail -->
      <div class="visual-row">
        <!-- Saturation & Brightness Area -->
        <div
          bind:this={sbArea}
          class="sb-box"
          style:background-color="hsl({h}, 100%, 50%)"
          role="slider"
          tabindex="0"
          aria-label={t('tools.color.area')}
          aria-valuetext={t('tools.color.areaValue', { saturation: s, brightness: b })}
          aria-valuenow={b}
          onpointerdown={onSbPointerDown}
          onpointermove={onSbPointerMove}
          onpointerup={onSbPointerUp}
          onpointercancel={onSbPointerUp}
          onkeydown={onSbKeyDown}
        >
          <div class="sb-white-overlay"></div>
          <div class="sb-black-overlay"></div>
          <div
            class="sb-handle"
            style:left="{s}%"
            style:top="{100 - b}%"
            style:background-color={currentHex}
          ></div>
        </div>

        <!-- Hue Vertical Rainbow Rail -->
        <div
          bind:this={hueRail}
          class="hue-rail"
          role="slider"
          tabindex="0"
          aria-label={t('tools.color.hue')}
          aria-valuemin="0"
          aria-valuemax="360"
          aria-valuenow={h}
          onpointerdown={onHuePointerDown}
          onpointermove={onHuePointerMove}
          onpointerup={onHuePointerUp}
          onpointercancel={onHuePointerUp}
          onkeydown={onHueKeyDown}
        >
          <div class="hue-thumb" style:top="{(h / 360) * 100}%"></div>
        </div>
      </div>

      <!-- Preview + Eyedropper + HEX Row -->
      <div class="preview-hex-row">
        <div class="preview-swatch" style:background-color={currentHex}></div>
        {#if pickScreenColor}
          <button
            type="button"
            class="eyedropper-btn"
            title={t('tools.action.eyedropper')}
            aria-label={t('tools.action.eyedropper')}
            onclick={pickFromScreen}
          >
            <Icon name="eyedropper" size={14} />
          </button>
        {/if}
        <div class="hex-input-group">
          <label class="channel-label" for="{panelId}-hex">{t('tools.color.hex')}</label>
          <input
            id="{panelId}-hex"
            type="text"
            class="channel-input hex-input"
            class:invalid={hexInvalid(hexText)}
            value={hexText}
            spellcheck="false"
            autocapitalize="off"
            autocomplete="off"
            aria-label={t('tools.color.hex')}
            aria-invalid={hexInvalid(hexText) ? 'true' : undefined}
            oninput={onHexChange}
            onblur={(e) => commitHexValue(e.currentTarget.value)}
            onkeydown={(e) => {
              if (e.key === 'Enter') commitHexValue(e.currentTarget.value)
            }}
          />
        </div>
      </div>

      <!-- Numeric HSB and RGB Fields -->
      <div class="numeric-grid">
        <!-- HSB Group -->
        <div class="channel-col">
          <label class="channel-label" for="{panelId}-h">{t('tools.color.hue')}</label>
          <input
            id="{panelId}-h"
            type="text"
            inputmode="numeric"
            class="channel-input"
            value={h}
            aria-label={t('tools.color.hue')}
            oninput={onHInput}
          />
        </div>
        <div class="channel-col">
          <label class="channel-label" for="{panelId}-s">{t('tools.color.saturation')}</label>
          <input
            id="{panelId}-s"
            type="text"
            inputmode="numeric"
            class="channel-input"
            value={s}
            aria-label={t('tools.color.saturation')}
            oninput={onSInput}
          />
        </div>
        <div class="channel-col">
          <label class="channel-label" for="{panelId}-b">{t('tools.color.brightness')}</label>
          <input
            id="{panelId}-b"
            type="text"
            inputmode="numeric"
            class="channel-input"
            value={b}
            aria-label={t('tools.color.brightness')}
            oninput={onBInput}
          />
        </div>

        <!-- RGB Group -->
        <div class="channel-col">
          <label class="channel-label" for="{panelId}-r">{t('tools.color.red')}</label>
          <input
            id="{panelId}-r"
            type="text"
            inputmode="numeric"
            class="channel-input"
            value={currentRgb.r}
            aria-label={t('tools.color.red')}
            oninput={onRInput}
          />
        </div>
        <div class="channel-col">
          <label class="channel-label" for="{panelId}-g">{t('tools.color.green')}</label>
          <input
            id="{panelId}-g"
            type="text"
            inputmode="numeric"
            class="channel-input"
            value={currentRgb.g}
            aria-label={t('tools.color.green')}
            oninput={onGInput}
          />
        </div>
        <div class="channel-col">
          <label class="channel-label" for="{panelId}-blue">{t('tools.color.blue')}</label>
          <input
            id="{panelId}-blue"
            type="text"
            inputmode="numeric"
            class="channel-input"
            value={currentRgb.b}
            aria-label={t('tools.color.blue')}
            oninput={onBValInput}
          />
        </div>
      </div>
    </div>
{/snippet}

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="anchor"
  bind:this={root}
  onkeydown={(e) => overlay.dismissKey(e)}
  onfocusout={(e) => overlay.focusOut(e)}
>
  <button
    type="button"
    class="swatch-trigger"
    style:background-color={currentHex}
    aria-label={label}
    aria-haspopup="dialog"
    aria-expanded={open}
    aria-controls={open ? panelId : undefined}
    onclick={overlay.toggle}
  ></button>

  {#if open}{@render pickerPanel()}{/if}
</div>

<style>
  .anchor {
    position: relative;
    display: inline-flex;
    align-items: center;
  }

  .swatch-trigger {
    -webkit-appearance: none;
    appearance: none;
    flex: none;
    width: 26px;
    height: 26px;
    padding: 0;
    border: 1px solid var(--line2);
    border-radius: var(--r-chip);
    cursor: pointer;
    box-sizing: border-box;
    transition:
      border-color var(--dur-fast) var(--ease),
      box-shadow var(--dur-fast) var(--ease);
  }
  .swatch-trigger:hover {
    border-color: var(--tintline);
  }
  .swatch-trigger:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .picker-panel {
    position: absolute;
    top: calc(100% + var(--s-2));
    left: 0;
    z-index: 60;
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    width: 236px;
    padding: var(--s-3);
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--edge);
    animation: mcIn var(--dur) var(--ease);
    cursor: default;
    user-select: none;
  }
  .picker-panel.end {
    left: auto;
    right: 0;
  }
  .picker-panel.flipped-y {
    top: auto;
    bottom: calc(100% + var(--s-2));
  }
  .picker-panel.unclipped {
    position: fixed;
    right: auto;
    bottom: auto;
    max-height: calc(100vh - 16px);
    overflow: auto;
  }

  .visual-row {
    display: flex;
    gap: var(--s-2);
    height: 160px;
  }

  .sb-box {
    position: relative;
    flex: 1;
    height: 100%;
    border-radius: var(--r-sm);
    overflow: hidden;
    cursor: crosshair;
    touch-action: none;
  }
  .sb-box:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .sb-white-overlay {
    position: absolute;
    inset: 0;
    background: linear-gradient(to right, #ffffff, rgba(255, 255, 255, 0));
    pointer-events: none;
  }

  .sb-black-overlay {
    position: absolute;
    inset: 0;
    background: linear-gradient(to top, #000000, rgba(0, 0, 0, 0));
    pointer-events: none;
  }

  .sb-handle {
    position: absolute;
    width: 12px;
    height: 12px;
    border: 2px solid #ffffff;
    border-radius: var(--r-pill);
    box-shadow: 0 0 2px rgba(0, 0, 0, 0.8), inset 0 0 1px rgba(0, 0, 0, 0.4);
    transform: translate(-50%, -50%);
    pointer-events: none;
  }

  .hue-rail {
    position: relative;
    width: 16px;
    height: 100%;
    border-radius: var(--r-sm);
    background: linear-gradient(
      to bottom,
      #ff0000 0%,
      #ffff00 17%,
      #00ff00 33%,
      #00ffff 50%,
      #0000ff 67%,
      #ff00ff 83%,
      #ff0000 100%
    );
    cursor: pointer;
    touch-action: none;
  }
  .hue-rail:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .hue-thumb {
    position: absolute;
    left: -2px;
    right: -2px;
    height: 6px;
    border: 2px solid #ffffff;
    border-radius: var(--r-xs);
    box-shadow: 0 0 2px rgba(0, 0, 0, 0.8);
    transform: translateY(-50%);
    pointer-events: none;
  }

  .preview-hex-row {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }

  .preview-swatch {
    flex: none;
    width: 28px;
    height: 28px;
    border: 1px solid var(--line2);
    border-radius: var(--r-sm);
    box-sizing: border-box;
  }

  .eyedropper-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: none;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    background: var(--panel2);
    color: var(--text);
    cursor: pointer;
    transition:
      border-color var(--dur-fast) var(--ease),
      background var(--dur-fast) var(--ease);
  }
  .eyedropper-btn:hover {
    border-color: var(--line2);
  }
  .eyedropper-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .hex-input-group {
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
    gap: var(--s-1);
  }

  .numeric-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--s-2);
  }

  .channel-col {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .channel-label {
    font-size: 10px;
    font-weight: 500;
    color: var(--t3);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .channel-input {
    width: 100%;
    height: 24px;
    padding: 0 var(--s-1);
    border: 1px solid var(--line);
    border-radius: var(--r-xs);
    background: var(--panel2);
    color: var(--text);
    font-size: 11px;
    font-family: inherit;
    text-align: center;
    box-sizing: border-box;
    outline: none;
    transition: border-color var(--dur-fast) var(--ease);
  }
  .channel-input:hover {
    border-color: var(--line2);
  }
  .channel-input:focus {
    border-color: var(--accent);
  }
  .channel-input.invalid {
    border-color: var(--warn);
  }

  .hex-input {
    text-align: left;
    font-family: monospace;
    font-size: 11px;
  }
</style>
