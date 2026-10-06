# App icon exploration (2026-09-25)

The chosen icon is `design/icon.png` (1024 master, macOS grid) and `design/icon.svg`
(vector). `src-tauri/icons` and `infra/site/public/icon.png` come from the master via
`npx tauri icon design/icon.png`. The exploration PNGs listed below were deleted on
2026-09-26; `run.sh`, `run2.sh` and the concept files regenerate them (new renders
will differ).

Generated with the image-gen skill on Azure `spendlimit5k`, GPT-image-2.5.

- Round 1: 25 concepts in `concepts.json`, each rendered once with `gpt-image-2.5-flare` and once with `gpt-image-2.5-sunburst` (`out/NN-*-{flare,sunburst}.png`, sheets `sheet-01-13.png`, `sheet-14-25.png`).
- Round 2: the 4 strongest story concepts (peel mask, halftone, eraser) refined with Sunburst, 2 renders each (`concepts2.json`, `sheet-round2.png`).
- Final: peel reveal, Sunburst at quality `max`, 2048 px (`concepts3.json`, `out/F-peel-final-f2.png`).
- `finalize.py` fits that render to the macOS grid (824/1024 squircle, opaque tile, soft shadow) and writes `final/icon-{1024..16}.png`.

Why the peel reveal: a rose layer carrying Japanese text lifts off to leave a clean balloon, which is exactly what the app does. Plain empty balloons read as a chat app. It stays legible at 32 px and uses the brand ink, paper, and rose.

## SVG

- `design/icon.svg` (58 KB, written by `svg/vectorize.py`): the final icon rebuilt as clean layered vectors. Balloon, rose film, curl, roll and lifted text are smoothed outlines taken from `design/icon.png`; the tile, halftone, gradients, shadows and blur are native SVG. Rebuild with `python3 svg/vectorize.py` (needs numpy, opencv-python-headless, pillow).
- `svg/icon-traced.svg` (108 KB): a plain auto-trace (vtracer) for comparison. Blotchy, not recommended.

`design/icon.svg` uses SVG filters (drop shadows, blur). Browsers and macOS render them; some design tools drop filters on import.
