<script>
  /**
   * A bare range input, for the places a slider sits inside a control cluster
   * rather than in a labelled row - the editor's wipe slider in the view pill.
   * `Slider` is the row form: label left, range, readout right.
   *
   * There is no visible label, so `label` is the accessible name and the
   * tooltip. `valueText` is what assistive tech reads instead
   * of the bare number, and is normally the same string the cluster prints
   * beside the control.
   *
   * @type {{
   *   label: string,
   *   value: number,
   *   min: number,
   *   max: number,
   *   step?: number,
   *   width?: number,
   *   valueText?: string,
   *   disabled?: boolean,
   *   onchange: (value: number) => void,
   * }}
   */
  let {
    label,
    value,
    min,
    max,
    step = 1,
    width = 92,
    valueText,
    disabled = false,
    onchange,
  } = $props()

</script>

<input
  type="range"
  class="range"
  {min}
  {max}
  {step}
  {value}
  {disabled}
  style:width="{width}px"
  title={label}
  aria-label={label}
  aria-valuetext={valueText}
  oninput={(e) => onchange(Number(e.currentTarget.value))}
/>

<style>
  .range {
    height: 16px;
    flex: none;
    margin: 0;
    cursor: pointer;
  }
  .range:disabled { opacity: .45; cursor: default }
</style>
