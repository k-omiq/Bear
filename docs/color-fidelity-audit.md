# Color fidelity audit

Baseline reviewed and implementation updated 2026-10-03. The original six gaps
below drove `color-fidelity-fix-plan.md`; implementation and verification evidence
is recorded there. Native bytes, native samples, metadata and appearance are
separate guarantees.

## Imported assets

| Source | Chapter-owned working file | Original preservation |
| --- | --- | --- |
| PNG, TIFF, JPEG | Same format in `pages/` | Exact bytes, including metadata and JPEG compressed data |
| WebP, GIF, BMP | Lossless PNG in `pages/` | Exact source in sibling `originals/` |
| AVIF, HEIC, JPEG XL | Unsupported | Reported as skipped |

Conversions record chapter-relative original/working paths and hashes, detected
format, chosen frame, frame count, conversion version and retained/lost fields.
Both assets use atomic writes; a failed working write removes its new archive.
Duplicate checks precede publication and collision suffixes are deterministic.
Deleting the external scan folder does not remove chapter-owned assets.

Animated GIF/WebP use the first visible canvas, with animation reported and all
frames retained in the archive. Static full-canvas GIF keeps indices, palette and
transparency. WebP EXIF is carried to the initial working PNG when available;
re-encoded edited output does not blindly retain stale EXIF geometry/thumbnails.
Older converted chapters remain readable and report a missing original archive.
No original JPEG is reconstructed from an old PNG.

## Six baseline gaps and their resolution

1. **PNG description loss:** shared `ColorDescription` and buffered/streaming
   writer policy preserve integer gAMA/cHRM/sBIT/cICP/mDCV/cLLI and compatible safe
   ancillary chunks. cICP takes precedence over ICC, then sRGB, then gamma/chrm.
   Conflicting legacy descriptions are normalized with declarations. Edits remove
   stale sBIT/cLLI. TIFF/PSD receive an equivalent supported ICC or refuse the
   target explicitly. Stitches compare full interpretation and transparency.
2. **CMYK/YCCK JPEG mismatch:** native component decoding normalizes Adobe
   inversion and YCCK to CMYK. Header and raster agree; CMYK ICC never labels RGB.
   Edited CMYK output uses TIFF. Untouched same-format JPEG/CBZ entries retain
   exact bytes. ICC signatures and validity are checked before re-encoding.
3. **Unmanaged previews/transparency:** supported ICC and SDR declarations drive
   one tagged-sRGB derivative pipeline. Gray/RGB colour keys compare full native
   samples before quantization; indexed partial alpha survives. Linear-light,
   premultiplied-alpha downsampling avoids coloured transparent fringes. These
   derivatives never replace native source or export buffers.
4. **Unmanaged paint:** UI sRGB colours transform into the resolved source space;
   fractional coverage blends in linear light. Selected fill supports managed
   16-bit/CMYK/indexed cases, with unrepresentable-colour approximation reported.
   Native clone values remain exact when fully covered. Models cross explicit
   managed RGB boundaries. Live stroke previews use the backend commit renderer
   and discard superseded results.
5. **Lost conversion structure/orientation:** original archives retain all source
   structure; static GIF preserves indices where possible. Shared EXIF geometry
   covers orientations 1–8. Existing native strip patches are intersected before
   rotation; new oriented seam edits store native per-page parts under one undo
   identity. Re-encoded exports orient once; untouched passthrough preserves tags.
6. **Weak evidence and hidden output changes:** synthetic valid CMYK ICC replaces
   the old RGB-profile negative fixture. Checked-in libjpeg and Pillow/LCMS
   reference arrays establish component/appearance correctness independently of
   production decoding. Export preflight carries page-specific actual formats,
   metadata declarations and revisions through per-page/CBZ/stitched/PSD seams.

## Limits and precise guarantees

- Native PNG/TIFF/JPEG originals are exact. Supported lossless output preserves
  decoded native samples outside masks, with orientation treated as a permutation.
  This cannot reconstruct samples from before the source JPEG was compressed.
- Untagged RGB/gray display assumes sRGB transfer. Untagged CMYK uses a named
  multiplicative-ink preview assumption; selected-colour operations require an
  invertible profile. No guessed profile is written into original assets.
- Invalid or wrong-space ICC and unsupported HDR display/paint transforms fail
  explicitly. Native byte passthrough remains available for retained sources.
- Brush modes remain 8-bit Gray/GrayAlpha/RGB/RGBA. Models still refuse indexed
  and CMYK inputs. Output fallback preserves native space rather than silently
  converting those pages to RGB.
- Associated-alpha TIFF preserves native premultiplied samples and its explicit
  alpha tag; previews unassociate display derivatives. PNG requests fall back to
  TIFF and PSD refuses; editing currently refuses because measured/fitted fills
  are not association-aware. Straight-alpha TIFF writes an explicit alpha tag.
- Legacy non-ICC TIFF transfer/white-point/primary declarations are detected but
  not yet transformed: preview/editing/re-encoding refuses them explicitly;
  original import and unchanged TIFF passthrough remain available.
- General ancillary metadata is not universally retained. Stale EXIF thumbnails,
  unsafe unknown chunks and invalidated image summaries may be omitted on
  re-encode; exact originals remain available. Unsupported equivalent-profile or
  transparency/target combinations are refused before publication.
- Streaming stitches retain one source page plus row buffers, but the existing
  global patch list remains resident. Export preflight encodes to a discard sink
  to validate the exact writer and therefore adds work before execution.

## Verification

The plan records commands, counts and independent-reference tolerances per batch.
Regression coverage includes native import hashes, archived animation, static GIF
indices, positioned first-frame composition, WebP EXIF conversion, mixed JPEG/PNG
CBZ, stale export plans, validation refusal before publication, partial-publication
reporting, and asymmetric orientation/history cases. Navigation and RTL/LTR work
are retained. Installed-webview verification and any unavailable model/platform
checks are reported separately at the release gate; a passing DOM test alone is
not evidence of an installed-app result.
