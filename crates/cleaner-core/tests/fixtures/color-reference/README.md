# Independent color reference fixtures

All images, profiles, sample arrays and generators here are synthetic CC0-1.0
material generated for this project. No scanner or vendor ICC profile is copied.
`manifest.json` records SHA-256 checksums, generator versions and tolerances.

Run `generate.c` (compile command in its header) and then `generate.py` to
regenerate. The C generator uses libjpeg-turbo 3.2.0, independently of the
production Rust zune-jpeg decoder. It writes CMYK JPEGs with and without Adobe
APP14, Adobe YCCK, and their normalized non-inverted CMYK reference samples
(16×8×4 bytes, zero means no ink). The independent reference uses integer slow
IDCT and no fancy upsampling. The original fixtures use 1:1 component sampling, with a maximum component
difference of 2/255. `ycck-subsampled` adds 2:1 horizontal/vertical chroma
sampling, compared with libjpeg fancy upsampling (maximum 5/255: IDCT/chroma
rounding amplified by the YCCK matrix). `ycck-markerless` deliberately lacks
APP14; libjpeg and production both interpret its four components as CMYK.
This negative ambiguity fixture does **not** prove recovery of intended YCCK:
there is no reliable discriminator, so processing reports the CMYK assumption.

`*-icc.jpg` have exactly the same image codestream and embed synthetic-cmyk.icc.
The ICC is a valid bidirectional CMYK output profile with Lab PCS, D50 white,
and an explicit ideal ink assumption: encoded sRGB = (1−CMY) × (1−K). Its CLUT
uses nine samples per axis. This profile tests correct source-space handling;
it does not represent a particular press or scanner. `.srgb` files contain
Pillow/LCMS relative-colorimetric transforms of independently decoded CMYK
samples, tolerance 3/255 (decoder and CLUT rounding). A timestamp normalization
makes the profile reproducible. Native CMYK fixture `cmyk8` uses this profile;
`fixtures::mismatched_cmyk()` retains the old RGB-profile/CMYK negative case.

The PNG generator writes chunks directly with Python struct/zlib, independently
of the production PNG encoder. `gamma-chrm.png` declares linear RGB with sRGB
primaries; the expected preview is the standard sRGB transfer function applied
to each sample / 255. `gray16-key.png` and `rgb16-key.png` deliberately contain
adjacent low-byte values with identical high bytes; only the exact color key
is transparent. `indexed-alpha.png` has indices 0,1,2,3 with alpha 0,64,128,255.
`modern-color.png` exercises cICP/sBIT/mDCV/cLLI. `conflicting-profile.png`
and `malformed-profile.png` must never reach an encoder as valid RGB profiles.

Orientation JPEGs share a 4×3 asymmetric picture and EXIF values 1–8. Animation
fixtures contain complete red and blue 4×3 frames; first-frame conversion is red.
Original-byte hashes and decoded-sample checks are separate assertions.

`selected-srgb.cmyk` is the independent Pillow/LCMS relative-colorimetric
reverse transform of RGB (255,0,0), (128,128,128), and (40,100,180) through
the synthetic CMYK profile. Selected-fill native channels allow at most one
8-bit value versus this reference.

The workflow matrix also exercises native 8/16-bit PNG/TIFF rasters, packed
logical samples (ignoring row padding), and converted animation archives. The
appearance tests use a one-code-value bound for controlled SDR transforms;
combined JPEG decode plus profile transform has the separately documented
three-code bound. These tolerances do not apply to native outside-mask samples,
which must match exactly.

`generate_tiff.py` independently writes 8/16-bit straight/associated-alpha TIFFs
using tifffile; `.native` buffers are tifffile-decoded native samples (16-bit
big endian), and `.rgba` buffers are Pillow's independent rendered 8-bit RGBA.
Preview tolerance is one 8-bit code. Association tags and native samples must
remain exact through buffered and streaming TIFF outputs. Native associated
samples cannot be relabeled as PNG/PSD straight-alpha values; export uses TIFF.
Legacy TIFF tags 301/318/319 without ICC test explicit unsupported-interpretation
refusal while exact original import/passthrough remain possible. Generator
versions and every associated fixture/reference hash are in `tiff-manifest.json`.
