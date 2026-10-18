# Chinese and Korean automatic cleaning

This note explains how Chinese and Korean text reaches automatic cleaning without a separate OCR model, and what has been checked.

The shipped `comictextdetector.onnx` supplies both boxes and a pixel mask, but
its upstream project describes training on synthetic manga/comic data. The
already-shipped `ogkalu/comic-text-and-bubble-detector` is the complementary
detection source: its model card says its data includes Manga, Webtoon, Manhua
and Western comics. Comic Translate also pairs this detector with algorithmic
segmentation in its Korean and Chinese pipeline. This project therefore reuses
the model already needed for balloon classification instead of adding another
runtime or large set of weights.

The implementation adopts confident `text_bubble` and `text_free` boxes that
the primary detector did not cover. Because that model has no segmentation
head, an adopted box receives a conservative box-local Otsu mask when the
primary segmentation contains no ink there. Flat and near-flat boxes remain
empty. The generated mask feeds both line extraction/script identification and
the existing fitting pipeline. The script gate accepts `Hangul`, `HanS`, and
`HanT` (including vertical and `-dn` variants), while Latin and other trusted
non-CJK scripts remain protected. Outside-balloon text keeps the existing
review/explicit-opt-in policy.

An actual-model smoke test on 2026-09-14 used synthetic speech balloons rather
than a claim about production-page accuracy. The Korean sample reproduced the
important detection failure shape: zero primary text boxes and one confident
adopted `TextInBubble` box. The primary segmentation still contained ink in
that box, so this sample did not need the Otsu fallback; the focused constructed
regression separately forces an empty primary segmentation and verifies the
fallback through mask fitting and rendered ink removal. Korean reached
`Clean(Hangul)`. Simplified
and traditional Chinese samples produced `Clean(HanS)` and `Clean(HanT)`.
The English control was also recovered through the adopted-box path and remained
untouched as `NotJapanese { script: "Latin" }`.

The same four samples were then run through the full adapter pipeline. Korean,
simplified Chinese, and traditional Chinese each produced one accepted rung-0
fill patch, changing 8,710, 7,665, and 8,028 pixels respectively, with zero
changed pixels outside the permitted mask margin. The English control produced
one untouched review region and changed zero pixels. These remain synthetic
smoke checks rather than an accuracy measurement on production pages.

`manga-ocr` remains an optional Japanese-only rescue reader. It is not used as
evidence that Korean or Chinese OCR is supported; those languages reach auto
cleaning through multilingual detection, segmentation, and script ID.

Primary references:

- <https://github.com/dmMaze/comic-text-detector>
- <https://huggingface.co/ogkalu/comic-text-and-bubble-detector>
- <https://github.com/ogkalu2/comic-translate>
