/**
 * Manga Cleaner icon set.
 *
 * Every glyph is drawn from scratch on a 16x16 grid for a 1.5 stroke with
 * round caps and round joins. Conventions, applied to all of them:
 *
 *  - Live area is 2.5 .. 13.5 for rectilinear forms (11 units). Round and
 *    diagonal forms run slightly larger (circles ~11.2 across, diagonals to
 *    ~11.8 bbox) so that their ink reads at the same optical size.
 *  - Straight strokes and every terminal point sit on half-pixel coordinates
 *    (x.5 / y.5) so the 1.5 stroke lands crisply at 16px.
 *  - One corner radius: rectangular forms are rounded with r = 1 arcs
 *    (r = 1.2 - 1.4 on the largest boxes so the visual radius stays constant).
 *  - `h.01` segments are deliberate single dots, drawn by the round line cap.
 *
 * Format: `name: [d, ...]` for stroke-only glyphs, or
 * `name: { paths: [d, ...], filled: [d, ...] }` when a glyph needs solid
 * sub-paths. `filled` sub-paths render with fill:currentColor and no stroke.
 *
 * @typedef {string[] | { paths?: string[], filled?: string[] }} IconGlyph
 * @type {Record<string, IconGlyph>}
 */
export const icons = {
  /* ---- navigation / shell ---------------------------------------------- */

  // House: roof, walls, door. Silhouette 11 wide, 10.7 tall.
  'home': [
    'M2.5 7.05 8 2.65l5.5 4.4V12.35a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1Z',
    'M6.3 13.35V9.65h3.4v3.7',
  ],

  // Two offset sheets - the page list, plural by construction.
  'pages': [
    'M2.5 6.5a1 1 0 0 1 1-1H9.5a1 1 0 0 1 1 1V12.5a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1Z',
    'M5.5 5.5V3.5a1 1 0 0 1 1-1H12.5a1 1 0 0 1 1 1V9.5a1 1 0 0 1-1 1H10.5',
  ],

  // Stacked planes seen edge-on.
  'layers': [
    'M8 2.5 13.4 5.4 8 8.3 2.6 5.4Z',
    'M2.6 8.2 8 11.1l5.4-2.9',
    'M2.6 10.6 8 13.5l5.4-2.9',
  ],

  // Open-end wrench along the 45 degree diagonal.
  'tools': [
    'M9.16 5.23a.54 .54 0 0 0 0 .75l.86 .86a.54 .54 0 0 0 .75 0l2.02-2.02a3.22 3.22 0 0 1-4.26 4.26l-3.7 3.7a1.14 1.14 0 0 1-1.61-1.61l3.7-3.7a3.22 3.22 0 0 1 4.26-4.26l-2.02 2.02Z',
  ],

  /* ---- view state ------------------------------------------------------- */

  // Eye with a solid pupil. The solid pupil is the "showing the original" read.
  'eye': {
    paths: [
      'M2.5 8C4.1 5.2 5.9 3.8 8 3.8s3.9 1.4 5.5 4.2c-1.6 2.8-3.4 4.2-5.5 4.2S4.1 10.8 2.5 8Z',
    ],
    filled: [
      'M9.9 8a1.9 1.9 0 1 1-3.8 0 1.9 1.9 0 0 1 3.8 0Z',
    ],
  },

  // Same eye, struck through. The pupil stays: without it the outline alone
  // reads as a null sign rather than an eye.
  'eye-off': {
    paths: [
      'M2.5 8C4.1 5.2 5.9 3.8 8 3.8s3.9 1.4 5.5 4.2c-1.6 2.8-3.4 4.2-5.5 4.2S4.1 10.8 2.5 8Z',
      'M2.9 13.1 13.1 2.9',
    ],
    filled: [
      'M9.9 8a1.9 1.9 0 1 1-3.8 0 1.9 1.9 0 0 1 3.8 0Z',
    ],
  },

  // Thumbtack, front on.
  'pin': [
    'M4.6 2.5h6.8',
    'M6.3 2.5 5.8 8 3.7 9.6v1.1h8.6V9.6L10.2 8l-.5-5.5',
    'M8 10.7v2.8',
  ],

  // Page with two solid patches - the mask tint laid over regions of the art.
  'mask-overlay': {
    paths: [
      'M2.6 4a1.4 1.4 0 0 1 1.4-1.4h8a1.4 1.4 0 0 1 1.4 1.4v8a1.4 1.4 0 0 1-1.4 1.4h-8a1.4 1.4 0 0 1-1.4-1.4Z',
    ],
    filled: [
      'M5.2 5.9h5.6v1.8H5.2Z',
      'M5.2 9.1h3.7v1.8H5.2Z',
    ],
  },

  // Two sliders. A gear turns to mush at 16px; sliders do not.
  'settings': [
    'M2.5 5.2h2.3M8.2 5.2h5.3',
    'M2.5 10.8h6.4M12.3 10.8h1.2',
    'M8.2 5.2a1.7 1.7 0 1 1-3.4 0 1.7 1.7 0 0 1 3.4 0Z',
    'M12.3 10.8a1.7 1.7 0 1 1-3.4 0 1.7 1.7 0 0 1 3.4 0Z',
  ],

  // Arrow leaving a tray.
  'export': [
    'M8 10.2V2.6',
    'M5.2 5.4 8 2.6l2.8 2.8',
    'M2.9 10.2v2.4a.9 .9 0 0 0 .9.9h8.4a.9 .9 0 0 0 .9-.9v-2.4',
  ],

  /* ---- tools (must separate at a glance in a vertical stack) ------------ */

  // Auto clean: one centred four-point star with concave arms.
  'sparkle': [
    'M8 2.4q.4 5.2 5.6 5.6-5.2 .4-5.6 5.6-.4-5.2-5.6-5.6 5.2-.4 5.6-5.6Z',
  ],

  // Brush: barrel on the diagonal, ferrule band, paint smear at the lower
  // left. Without the ferrule the glyph reads as a pen.
  'brush': [
    'M6.05 8.93 11.06 3.92a1.25 1.25 0 0 1 1.76 1.76L7.81 10.69Z',
    'M7 7.98 8.75 9.73',
    'M6.05 8.93c-1.58 .37-2.32 1.76-3.15 3.43 1.85-.19 3.43-.56 4.17-1.48 .74-.93 .37-1.67-1.02-1.95Z',
  ],

  // Shapes: a square and a disc, overlapping.
  'shapes': [
    'M2.6 3.5a.9 .9 0 0 1 .9-.9h5a.9 .9 0 0 1 .9.9v5a.9 .9 0 0 1-.9.9h-5a.9 .9 0 0 1-.9-.9Z',
    'M13.4 10.3a3.1 3.1 0 1 1-6.2 0 3.1 3.1 0 0 1 6.2 0Z',
  ],

  // AI mask brush: a bare stick with a spark at the tip. No barrel, so it
  // never reads as `brush`; the star is off-centre, so it never reads as
  // `sparkle`.
  'wand': [
    'M2.7 13.2 8.7 7.2',
    'M11 2.8q.34 1.96 2.3 2.3-1.96 .34-2.3 2.3-.34-1.96-2.3-2.3 1.96-.34 2.3-2.3Z',
  ],

  // Content-aware fill: a droplet.
  'droplet': [
    'M8 2.9c1.4 2.6 4.3 4.6 4.3 6.2a4.3 4.3 0 0 1-8.6 0c0-1.6 2.9-3.6 4.3-6.2Z',
  ],

  // Clone / heal: a rubber stamp on its pad.
  'stamp': [
    'M6.2 2.7h3.6a1 1 0 0 1 1 1v1.5a1 1 0 0 1-1 1H6.2a1 1 0 0 1-1-1V3.7a1 1 0 0 1 1-1Z',
    'M6.6 6.2 5.4 9.4M9.4 6.2l1.2 3.2',
    'M3.6 9.4h8.8a.8 .8 0 0 1 .8 .8v.4a.8 .8 0 0 1-.8 .8H3.6a.8 .8 0 0 1-.8-.8v-.4a.8 .8 0 0 1 .8-.8Z',
    'M3.4 13.3h9.2',
  ],

  /* ---- zoom / history --------------------------------------------------- */

  'zoom-fit': [
    'M2.6 6.1V3.5a.9 .9 0 0 1 .9-.9h2.6M10 2.6h2.6a.9 .9 0 0 1 .9.9v2.6M13.5 10v2.6a.9 .9 0 0 1-.9.9H10M6.1 13.5H3.5a.9 .9 0 0 1-.9-.9V10',
  ],

  'zoom-in': [
    'M11.3 7a4.3 4.3 0 1 1-8.6 0 4.3 4.3 0 0 1 8.6 0Z',
    'M10.2 10.2 13.4 13.4',
    'M4.9 7h4.2M7 4.9v4.2',
  ],

  'zoom-out': [
    'M11.3 7a4.3 4.3 0 1 1-8.6 0 4.3 4.3 0 0 1 8.6 0Z',
    'M10.2 10.2 13.4 13.4',
    'M4.9 7h4.2',
  ],

  'undo': [
    'M2.7 6.15h7.2a3.2 3.2 0 0 1 0 6.4H7.4',
    'M5.5 3.45 2.7 6.15l2.8 2.7',
  ],

  'redo': [
    'M13.3 6.15H6.1a3.2 3.2 0 0 0 0 6.4h2.5',
    'M10.5 3.45 13.3 6.15l-2.8 2.7',
  ],

  /* ---- chevrons / arrows ------------------------------------------------ */

  'chevron-left':  ['M10.2 3.6 5.8 8l4.4 4.4'],
  'chevron-right': ['M5.8 3.6 10.2 8l-4.4 4.4'],
  'chevron-up':    ['M3.6 10.2 8 5.8l4.4 4.4'],
  'chevron-down':  ['M3.6 5.8 8 10.2l4.4-4.4'],

  'chevrons-left':  ['M7.6 3.9 3.5 8l4.1 4.1', 'M12.5 3.9 8.4 8l4.1 4.1'],
  'chevrons-right': ['M8.4 3.9 12.5 8l-4.1 4.1', 'M3.5 3.9 7.6 8l-4.1 4.1'],

  'arrow-up':   ['M8 13.3V2.7', 'M3.9 6.8 8 2.7l4.1 4.1'],
  'arrow-down': ['M8 2.7v10.6', 'M3.9 9.2 8 13.3l4.1-4.1'],

  /* ---- status / feedback ------------------------------------------------ */

  'close': ['M4.1 4.1 11.9 11.9', 'M11.9 4.1 4.1 11.9'],

  'warning-triangle': [
    'M7 3.05a1.2 1.2 0 0 1 2 0l4.6 8.6a1.2 1.2 0 0 1-1 1.8H3.4a1.2 1.2 0 0 1-1-1.8Z',
    'M8 6.45v2.5M8 11.2h.01',
  ],

  'check': ['M3.3 8.1 6.5 11.3l6.2-6.6'],

  'dot': {
    filled: ['M10.6 8a2.6 2.6 0 1 1-5.2 0 2.6 2.6 0 0 1 5.2 0Z'],
  },

  'info': [
    'M13.6 8a5.6 5.6 0 1 1-11.2 0 5.6 5.6 0 0 1 11.2 0Z',
    'M8 7.4v3.5M8 5.2h.01',
  ],

  'refresh': [
    'M13.3 8a5.3 5.3 0 1 1-5.3-5.3c1.48 0 2.9 .58 3.95 1.62l1.35 1.33',
    'M13.3 3.3v2.65h-2.65',
  ],

  'trash': [
    'M2.9 4.4h10.2',
    'M6.3 4.4V3.3a.8 .8 0 0 1 .8-.8h1.8a.8 .8 0 0 1 .8.8v1.1',
    'M4.2 4.4 4.7 12.7a.9 .9 0 0 0 .9.8h4.8a.9 .9 0 0 0 .9-.8l.5-8.3',
  ],

  /* ---- files ------------------------------------------------------------ */

  'folder': [
    'M2.5 12.15V3.85a1 1 0 0 1 1-1h2.9a1 1 0 0 1 .8.4l.9 1.2a1 1 0 0 0 .8.4h3.6a1 1 0 0 1 1 1v6.3a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1Z',
  ],

  'file': [
    'M9.2 2.5H4.6a1 1 0 0 0-1 1v9a1 1 0 0 0 1 1h6.8a1 1 0 0 0 1-1V5.3Z',
    'M9.2 2.5v1.8a1 1 0 0 0 1 1h2.2',
  ],

  /* ---- misc controls ---------------------------------------------------- */

  'plus': ['M8 2.9v10.2M2.9 8h10.2'],

  'search': [
    'M11.3 7a4.3 4.3 0 1 1-8.6 0 4.3 4.3 0 0 1 8.6 0Z',
    'M10.2 10.2 13.4 13.4',
  ],

  // Pipette on the same 45 degree diagonal as `brush`, and told apart from it
  // at the two ends: the bulb is wider than the barrel where the brush's head
  // is not, and the barrel closes to a point where the brush's opens into a
  // smear. Drawn rather than borrowed because the hex row used `droplet`,
  // which is the Content-aware fill tool's glyph, and one glyph meaning two
  // things in one screen is worse than a second glyph.
  'eyedropper': [
    // Barrel: 2.4 across, down the diagonal from the shoulder, drawn to a
    // point at 2.9 13.1 and back. Closed, so the shoulder is the ferrule.
    'M8.7 5.6 3.75 10.55 2.9 13.1 5.45 12.25 10.4 7.3Z',
    // Bulb: the same diagonal, 4 across, capped with a half circle.
    'M8.13 5.04 9.97 3.2a2 2 0 0 1 2.83 2.83L10.96 7.87Z',
  ],

  'keyboard': [
    'M2.4 5.3a1.2 1.2 0 0 1 1.2-1.2h8.8a1.2 1.2 0 0 1 1.2 1.2v5.4a1.2 1.2 0 0 1-1.2 1.2H3.6a1.2 1.2 0 0 1-1.2-1.2Z',
    'M5.2 6.9h.01M8 6.9h.01M10.8 6.9h.01',
    'M5.2 9.9h5.6',
  ],

  // Six-dot grip. Two lines would read as a menu; the grid reads as a handle.
  'drag-handle': [
    'M5 4h.01M11 4h.01M5 8h.01M11 8h.01M5 12h.01M11 12h.01',
  ],

  'resize-corner': [
    'M3.6 12.4 12.4 3.6',
    'M8.4 3.6h4v4',
    'M7.6 12.4H3.6V8.4',
  ],

  'more-horizontal': {
    filled: [
      'M4.8 8a1.2 1.2 0 1 1-2.4 0 1.2 1.2 0 0 1 2.4 0Z',
      'M9.2 8a1.2 1.2 0 1 1-2.4 0 1.2 1.2 0 0 1 2.4 0Z',
      'M13.6 8a1.2 1.2 0 1 1-2.4 0 1.2 1.2 0 0 1 2.4 0Z',
    ],
  },

  'external-link': [
    'M12.2 9.1v3.2a1.2 1.2 0 0 1-1.2 1.2H3.7a1.2 1.2 0 0 1-1.2-1.2V5a1.2 1.2 0 0 1 1.2-1.2h3.2',
    'M9.6 2.5h3.9v3.9',
    'M13.5 2.5 7.9 8.1',
  ],

  'lock': [
    'M4.3 7.2h7.4a1.3 1.3 0 0 1 1.3 1.3v3.7a1.3 1.3 0 0 1-1.3 1.3H4.3a1.3 1.3 0 0 1-1.3-1.3V8.5a1.3 1.3 0 0 1 1.3-1.3Z',
    'M5.3 7.2V5.4a2.7 2.7 0 0 1 5.4 0v1.8',
  ],

  'cloud': [
    'M11.05 11.9H6.39a3.85 3.85 0 1 1 3.66-4.99h1a2.5 2.5 0 1 1 0 4.99Z',
  ],

  // The language picker's mark, on the Welcome screen and in Settings.
  'globe': [
    'M13.6 8A5.6 5.6 0 1 1 2.4 8a5.6 5.6 0 0 1 11.2 0Z',
    'M2.4 8h11.2',
    'M8 2.4c-1.6 1.5-2.4 3.4-2.4 5.6s.8 4.1 2.4 5.6c1.6-1.5 2.4-3.4 2.4-5.6S9.6 3.9 8 2.4Z',
  ],

  'cpu': [
    'M4.4 5.6a1.2 1.2 0 0 1 1.2-1.2h4.8a1.2 1.2 0 0 1 1.2 1.2v4.8a1.2 1.2 0 0 1-1.2 1.2H5.6a1.2 1.2 0 0 1-1.2-1.2Z',
    'M6.6 6.6h2.8v2.8H6.6Z',
    'M6.4 2.6v1.8M9.6 2.6v1.8M6.4 11.6v1.8M9.6 11.6v1.8',
    'M2.6 6.4h1.8M2.6 9.6h1.8M11.6 6.4h1.8M11.6 9.6h1.8',
  ],


  /* ---- tool bar -------------------------------------------------------- */

  // Rounded rectangle, landscape.
  'shape-rect': [
    'M2.5 5.5a1 1 0 0 1 1-1H12.5a1 1 0 0 1 1 1V10.5a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1Z',
  ],

  // Ellipse, landscape.
  'shape-ellipse': [
    'M8 4A5.5 4 0 0 1 13.5 8 5.5 4 0 0 1 8 12 5.5 4 0 0 1 2.5 8 5.5 4 0 0 1 8 4Z',
  ],

  // Freehand lasso loop with a short tail.
  'shape-lasso': [
    'M9.6 10.9C7.2 12.1 3.6 11.3 2.8 8.8 2 6.3 4.2 3.2 7.6 2.7c3.4-.5 6 1.8 5.7 4.3-.2 1.6-1.4 2.8-2.7 3.4',
    'M10.6 10.4c-.6 1.1-.3 2.2.6 3',
  ],

  // Irregular pentagon with crisp corners and vertex dots.
  'shape-polygon': [
    'M8 2.5 13.5 5.5 11.5 13.5 3.5 12.5 2.5 7.5Z',
    'M8 2.5h.01M13.5 5.5h.01M11.5 13.5h.01M3.5 12.5h.01M2.5 7.5h.01',
  ],

  'shape-line': ['M2.5 12.5 13.5 3.5'],

  // Selection: a marquee, the square drawn as rounded corners and dashes, the
  // mark every editor uses for "the area that is picked".
  'selection': [
    'M2.5 4.5v-1a1 1 0 0 1 1-1h1',
    'M7 2.5h2',
    'M11.5 2.5h1a1 1 0 0 1 1 1v1',
    'M13.5 7v2',
    'M13.5 11.5v1a1 1 0 0 1-1 1h-1',
    'M9 13.5H7',
    'M4.5 13.5h-1a1 1 0 0 1-1-1v-1',
    'M2.5 9V7',
  ],

  // Add to the selection: the marquee's corners with a plus inside.
  'mask-add': [
    'M2.5 4.5v-1a1 1 0 0 1 1-1h1',
    'M11.5 2.5h1a1 1 0 0 1 1 1v1',
    'M13.5 11.5v1a1 1 0 0 1-1 1h-1',
    'M4.5 13.5h-1a1 1 0 0 1-1-1v-1',
    'M8 5.3v5.4M5.3 8h5.4',
  ],

  // Remove from the selection: the same corners with a minus.
  'mask-remove': [
    'M2.5 4.5v-1a1 1 0 0 1 1-1h1',
    'M11.5 2.5h1a1 1 0 0 1 1 1v1',
    'M13.5 11.5v1a1 1 0 0 1-1 1h-1',
    'M4.5 13.5h-1a1 1 0 0 1-1-1v-1',
    'M5.3 8h5.4',
  ],

  // Open book seen from the front.
  'book': [
    'M2.5 4.5c2-1 3.5-.8 5.5.3v7.7c-2-1-3.5-.8-5.5.3V4.5Z',
    'M13.5 4.5c-2-1-3.5-.8-5.5.3v7.7c2-1 3.5-.8 5.5.3V4.5Z',
  ],

  // Three horizontal slider tracks with staggered adjustment knobs.
  'sliders': [
    'M2.5 4.5H13.5M2.5 8H13.5M2.5 11.5H13.5',
    'M5.5 3V6M10.5 6.5V9.5M7.5 10V13',
  ],

  // Play button: right-pointing triangle.
  'play': [
    'M4 4a1 1 0 0 1 1.5-.9l7.5 4.1a1 1 0 0 1 0 1.7L5.5 13A1 1 0 0 1 4 12.1Z',
  ],

  // Stop button: rounded square.
  'stop': [
    'M3.5 4.5a1 1 0 0 1 1-1H11.5a1 1 0 0 1 1 1V11.5a1 1 0 0 1-1 1H4.5a1 1 0 0 1-1-1Z',
  ],

  // Two interlocking chain links along the diagonal.
  'link': [
    'M6.7 8.7a3.3 3.3 0 0 0 5 .4l2-2a3.3 3.3 0 0 0-4.7-4.7l-1.1 1.1',
    'M9.3 7.3a3.3 3.3 0 0 0-5-.4l-2 2a3.3 3.3 0 0 0 4.7 4.7l1.1-1.1',
  ],

  // Interlocking chain links with a diagonal break slash.
  'link-off': [
    'M6.7 8.7a3.3 3.3 0 0 0 5 .4l2-2a3.3 3.3 0 0 0-4.7-4.7l-1.1 1.1',
    'M9.3 7.3a3.3 3.3 0 0 0-5-.4l-2 2a3.3 3.3 0 0 0 4.7 4.7l1.1-1.1',
    'M2.9 2.9 13.1 13.1',
  ],

  // Adhesive bandage at 45 degrees with center pad perforations.
  'bandage': [
    'M3 10.2L10.2 3a2 2 0 0 1 2.8 2.8L5.8 13a2 2 0 0 1-2.8-2.8Z',
    'M7 7h.01M9 7h.01M7 9h.01M9 9h.01',
  ],

  // Question mark.
  'help': [
    'M5.8 5.6a2.2 2.2 0 0 1 4.4 0c0 1.5-2.2 2-2.2 3.4M8 12.5h.01',
  ],

  // Five-point star, outline. A rating's empty mark.
  'star': [
    'M8 2.55 9.5 6.39l4.11.24-3.18 2.61 1.04 3.98L8 11l-3.47 2.22 1.04-3.98-3.18-2.61 4.11-.24Z',
  ],

  // The same star, solid. A rating's full mark.
  'star-filled': {
    paths: ['M8 2.55 9.5 6.39l4.11.24-3.18 2.61 1.04 3.98L8 11l-3.47 2.22 1.04-3.98-3.18-2.61 4.11-.24Z'],
    filled: ['M8 2.55 9.5 6.39l4.11.24-3.18 2.61 1.04 3.98L8 11l-3.47 2.22 1.04-3.98-3.18-2.61 4.11-.24Z'],
  },

  // Pause: two upright bars.
  'pause': [
    'M5.5 3.5v9M10.5 3.5v9',
  ],

  // Download: an arrow down onto a tray line.
  'download': [
    'M8 2.5v7.5M4.8 6.8 8 10l3.2-3.2M3 13.5h10',
  ],
}

/** Every icon name in the set, in declaration order. */
export const iconNames = /** @type {string[]} */ (Object.keys(icons))

/**
 * Normalise a glyph entry into its stroked and filled sub-path lists.
 * @param {string} name
 * @returns {{ paths: string[], filled: string[] }}
 */
export function glyph(name) {
  const raw = icons[name]
  if (!raw) return { paths: [], filled: [] }
  if (Array.isArray(raw)) return { paths: raw, filled: [] }
  return { paths: raw.paths ?? [], filled: raw.filled ?? [] }
}
