# M0 legacy region ledger

Run `cargo run -p spike-m0-ledger -- --input-dir /path/to/pages --output-dir /path/to/scratch --max-pages 3`.
The harness opens the same `Pipeline` and `PageContext` as `spike-clean-page`.
It requires installed CTD, RT-DETR, script gate, gate labels, and LaMa model
files. It does not fetch them. The input images and all crops remain in the
chosen output directory, so use a scratch location for scans without rights.
Use `--cpu-only` where no GPU adapter is available; this keeps the same legacy
pipeline and records CPU provider timing.

Each page has a final export. Untouched regions have `raw-crop.png` using the
pipeline's returned box. Cleaned regions have `raw-crop-proxy.png` using the
applied mask bounds, plus `ink-proxy.png`, `applied-mask.png`, and `patch.png`.
The pipeline does not expose a cleaned region's detector candidate box or its
base mask. Their ledger columns are empty, and no `base-mask.png` is written.
`candidate_box_proxy` and `ink_proxy_bounds` identify the retained proxies.
`candidate.json` uses null for unavailable boxes and mask bounds. Untouched
regions have no mask files. A page with no regions has one row for page time
and process high-water RSS, with its region and outcome fields empty.
`ledger.json` and `ledger.csv` record automatic outcomes and reason keys.
Human failure categories and correction time remain blank for review. Missed
discovery cannot be inferred without labels. The current pipeline silently
skips an empty detector seed before returning a region outcome, so the ledger
cannot count those weak/empty masks until that event is exposed by the app API.
Peak RSS is a cumulative process
high-water mark, not isolated per-page model allocation.
