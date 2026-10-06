# Holdout measurement

Create a rights-cleared, human-labeled set outside the repository. All masks
use source-page coordinates and nonzero pixels mean membership. Run:

```sh
python3 spikes/holdout-metrics/measure.py --labels /path/labels.json --predictions /path/predictions --corrections /path/corrections.csv --output /path/metrics.json
```

`labels.json` has `{"pages": [...]}`. Each page has a unique `id`,
`lettering_mask` and `protected_art_mask` (one-channel PNG paths relative to
the label manifest), `instances`, and optional `tags`. Each instance has a
unique `id`, either `box_xywh: [x,y,w,h]` or `polygon: [[x,y],...]`, and
optional `tags`. Instance pixels are the intersection of its shape with the
page lettering mask. Tags are `japanese`, `korean`, `bubble`, `sfx`, `tiny`,
`outlined`, `art-contact`, and `longstrip`. A page enters a slice if its page
or any instance bears that tag; every metric in that slice uses the whole
page. Mark no-text pages with page tags. This avoids assigning a predicted
non-text pixel to a particular instance without a human decision.

Prediction files are named `<id>.json` by default; `prediction` on a label
page can override that name. A file may also be a one-channel applied write
mask PNG. JSON may be a SAM page JSON with `mask_png`, or a fusion page JSON
from `spikes/chapter-combo/results` and its associated run cache, with
`component_labels.file`, `candidates`, and `mask_label_id`. The fusion label
PNG and page JSON must remain together. Detector-only candidate boxes count
as candidates but do not grant predicted write pixels. Applied-mask connected
components are 8-connected candidates. An instance is found when at least
50% of its labeled pixels are predicted, configurable with
`--instance-threshold`. A false candidate overlaps no labeled instance pixel.
Complete-page recall includes no-text pages. Non-text exposure and protected
art damage are pixel counts, and protected-art damage is included in exposure
when masks do not overlap lettering. Runtime and peak memory are reported only
when the prediction JSON carries them. Missing measurements are not zero.

The chapter reference run is indexed by
`spikes/chapter-combo/results/live-chapter-109-run.json`. Its per-page fusion
JSON and numbered component PNGs are under
`spikes/chapter-combo/.cache/live-chapter-109/fusion/`; SAM page JSON and
binary masks are under `.../sam/` and `.../sam/masks/`. That cache is an
unlabeled research run, not holdout ground truth.

Optional correction CSV columns are `page_id,seconds`, one row per corrected
page. Mask and reconstruction judgments stay in separate JSON records using
`mask-review.schema.json` and `reconstruction-review.schema.json`. These
human judgments are not inferred from pixel overlap.
