# Findings

What was learned by measuring, testing and building this application. Every number here was
taken rather than estimated, and each carries the conditions it was taken under.

**The machine.** Unless another is named, every figure below is an Apple M5, 10 cores, 32 GB
unified memory, macOS 26.5.1, ONNX Runtime 1.28.0. Nothing here has ever been executed on
Windows or Linux, and a one-machine number is never written as though it were general.

---

## Hardware and acceleration

- **One operator decides where the inpainter runs.** `DFT` is implemented by exactly three ONNX
  Runtime providers: CPU, DirectML and WebGPU. A provider without the kernel does not fail; the
  runtime partitions the graph and copies tensors at every boundary. LaMa's Fourier convolutions
  sit mid-network, so the partition is a dozen round trips rather than a tail.
- **Provider medians, per model.** manga-LaMa 512²: 2540 ms CPU, 3899 ms CoreML, 1311 ms
  WebGPU. Detector 1024²: 703 / 383 / 548 ms. MI-GAN 512²: 415 / 569 / 256 ms. Balloon detector
  640² int8: 129 ms CPU, 222 ms WebGPU, and CoreML cannot build a session at all, taking 8 s to
  say so. Script identification 48×200: 2 ms CPU, 8 ms WebGPU.
- **No single provider wins,** so the provider is chosen per model from one table with a
  measurement beside every entry. The detector belongs on CoreML, both inpainters on WebGPU, the
  small models on the CPU where transfer costs more than compute.
- **LaMa wants exactly two intra-op threads:** 3531 ms at 1 (42% worse), 2486 ms at 2, 2552 at
  4, 2804 at 8, 3848 at 10 (55% worse), 2719 at the runtime's default (9% off). Determinism
  needs one thread, which puts LaMa at 3.5 s, so the golden-test and shipping configurations no
  longer share a latency.
- **Session build is a real cost and is not in the latency table.** CoreML takes 3.6 s for the
  detector's session and 25.7 s for LaMa's, against 0.1 s and 3.3 s on the CPU. A one-page run
  went from 1.6 s to 4.8 s when the detector moved to CoreML.
- **Hold the session.** LaMa's build costs 4.0 s on WebGPU and 3.2 s on the CPU against
  steady-state runs of 1470 ms and 2650 ms. Latency drifts +10.5% on WebGPU and +2.4% on the CPU
  over 40 runs, which is not a leak: a leak would compound rather than settle.
- **Do not batch regions.** The spatial input is fixed at 512², so batch is the only dynamic
  axis: per-region cost is flat from batch 1 to 4 on the GPU, 29% worse at 8, and 2.1× worse at
  batch 4 on the CPU. There is no batching path to build.
- **A forced provider is honoured on speed and gated on memory.** LaMa peaks at 740 MB on WebGPU
  (986 ms) against 8.19 GB on CoreML (1856 ms); MI-GAN at 212 MB (264 ms) against 5.62 GB
  (1311 ms). A forced provider whose measured peak will not fit the room the platform grants is
  declined to the CPU. It gates on a measurement or not at all.
- **Windows gets DirectML for every vendor:** Direct3D 12, so NVIDIA, AMD and Intel come through
  one download, and it has the `DFT` kernel. CUDA is offered and never default: that archive
  carries no DirectML, the process loads exactly one runtime library, and CUDA has no `DFT`, and
  it needs cuDNN 9 installed by hand.
- **The WebGPU plugin gives every desktop platform but one a GPU path.** Because the plugin
  registers into the runtime beside it and carries the `DFT` kernel, Windows has it next to
  DirectML, Linux x64 has it over Vulkan for every vendor, and a CUDA build no longer puts the
  inpainter back on the CPU: the detector goes to CUDA and the inpainter to the plugin. On Linux
  it needs the system Vulkan loader, and a machine without one has the provider registered,
  finding no adapter, and declined for that reason rather than silently absent. Linux aarch64 has
  neither a plugin nor a CUDA archive published and stays on the CPU. AMD's and Intel's own
  providers are not an alternative there: nobody publishes a C archive carrying them, and neither
  ROCm nor OpenVINO has `DFT`, so even downloadable they would take the detector and not the
  inpainter.
- **The Japanese text reader stays on the CPU, and that is a measurement.** One 224 px crop, ten
  runs: encoder 270 ms on the CPU against 218 ms on CoreML, decoder at eight tokens 4 ms against
  15 ms, and the encoder's 52 ms of gain costs a 6.6 s session build. The decoder runs once per
  token, so CoreML loses the part that runs most, and a rescue is a handful of regions on a page,
  so the build never amortises. Unlike the balloon detector, CoreML builds both graphs here and
  is the wrong choice anyway.

## Engines and quality

- **Re-measured through the application's own engines, every level moved.** LaMa at 512², median
  of eleven: 629 ms WebGPU, 1486 ms CPU provider, 1856 ms warm CoreML, session built in 2.1 s,
  against the probe harness's 1311 and 2540 ms on the same machine. The ordering is identical,
  so every decision resting on it stands and none of the numbers in it do.
- **MI-GAN was removed, and the rung it held is left empty.** It was reachable only by escalation
  from manga-LaMa, so every region that arrived there had already been declined by the stronger
  model: a rung whose entire population is what a better engine refused buys nothing, and it cost
  a download and a second held session to buy it. The number 3 stays vacant rather than being
  closed up, because a stored engine ceiling is a name and every comparison is on the rung's
  integer, so sliding the rungs above it down would silently reorder a ceiling somebody had
  already stored. manga-LaMa is the top of the automatic ladder now, and a region it declines is
  left alone and listed for review, which is the ladder's own answer for its end. A patch whose
  stored provenance says `migan` reads as LaMa rather than as an engine this build does not have.
  The measurements in the three bullets below are kept as the record of what was weighed.
- **MI-GAN was fast because it is a 28 MB model.** Warm: 116 to 120 ms on WebGPU (304 ms first
  call), 776 ms CPU provider, 1311 ms CoreML, session built in **51 ms**, forty times under
  LaMa's 2.1 s, which is what lets a live brush open it on demand. A held session is given back
  after five minutes idle rather than at the end of a run, returning 510 MB.
- **The published MI-GAN pipeline was the one that shipped, while it shipped.** It crops, resizes and blends inside
  the model, so the blend was measured: exactly **1 px** outside the mask the graph is handed, against an edit
  margin of 6, over 800 configurations of page, input size, mask size, shape and position. The
  planned re-export would have solved a problem that does not exist.
- **The two model rungs cut deliberately different holes, while there were two.** LaMa gets a stroke-tight hole
  with nothing added; MI-GAN gets the mask grown by a margin, because that graph blends a band of its
  own input back in. Measured at 3 px at 512² and flat across thirty configurations there, 9 px
  at 256², 300 px at 1024²: a property of input size, not of the graph.
- **The stroke-tight hole is a measurement.** Seven geometries were run on each of the eleven
  crowded-fixture regions that reach LaMa: the fitted mask, grown by 8, grown by 16, the convex
  hull, the bounding box, two edge-constrained floods. The stroke-tight hole left the least ink
  on ten of eleven, and the fatter the hole the more ink came back.
- **On a real scan the stroke-tight hole is the wrong hole, which is the opposite of what the
  fixtures said.** Ten regions of one page were rendered three ways, the page before, the first
  pass's hole, and the retry's, which is that set grown by the isolation radius because a re-run
  takes the stored applied mask as its hole. The first pass left stroke-shaped residue in every
  balloon where the retry came back clean: on a scanned page the model reads a stroke-tight hole
  as a glyph's silhouette and draws a glyph into it. The fit grows the ink by that same 5 native
  px past the first step now, walking downward so a balloon whose lettering nearly touches its
  outline gets as much margin as clears the edge map and never a hole across the outline. A retry
  still grows by 5 more. The synthetic table that chose the tight hole stands as what it measured,
  and as the reason a fixture is not a scan.
- **The leftover marks are invented by the model,** which two controls established: a crop
  stretched eight times about the paper level shows no trace of the zeroed text, and a scribble
  control over a crop with every mark painted out moved the written pixels 0 levels on ten of
  the eleven.
- **The decline metric does not fire on the failure this corpus contains.** Where LaMa invents
  text into a balloon, the interior-to-surround edge-energy ratio is 0.15 to 0.37 against a
  threshold of 2×, and all seven scored outputs pass: a balloon's annulus is flat paper crossed
  by its own outline, so its median is 0 and its mean is the outline.
- **The optional local redraw rung, measured.** FLUX.2 Klein 4B, Apache-2.0, MLX 4-bit,
  4,619,705,407 bytes on disk. One 512² render at 4 steps peaks at **6.58 GB** of physical
  footprint and settles at **1.37 GB** after release; 1024² peaks at **11.46 GB**. Opening costs
  0.6 s, rendering about **21 s** at 512² and **161 s** at 1024². Unguarded, the same 512²
  render costs **15.36 GB**, so its memory guards are worth **2.3×**.
- **Two of the four guards were withdrawn once the rung met real fixture text.** Evicting the
  text encoder made the second render through one open throw, and tiled decoding seams the
  resolution small crops are upscaled into. Keeping the encoder costs 8.92 GB across two renders
  (19.5 s and 26.6 s for a 288×608 and a 192×288 crop) and 9.77 GB at the largest untiled shape
  of 768², so the declared working set rose to 10 GiB.
- **A second backend, measured on Metal only.** With 4-bit SDNQ under `torch` and `diffusers`:
  5.48 GB of weights, open 4.5 s, renders of 47.8 s and 38.4 s, 7.00 GB peak across both.
  Roughly twice the wall clock and 1.9 GB cheaper at the peak, so the backend is a setting
  rather than a platform fact.
- **A third backend was measured and not adopted.** `stable-diffusion.cpp` at 768² and 4 steps
  with 5.49 GB of GGUF weights: 78.9, 83.5 and 94.8 s at 6.63 to 6.79 GB peak, so 3.1× to 4.9×
  slower than MLX. Offloading to the host was worse on unified memory (107.5 s, 8.71 GB), and
  its video-memory budget was a no-op, merging 29 planner segments into 1 with the footprint
  unmoved. Output was clean but lifted background tone from 246 to 253.
- **Sizing the redraw crop to the region was that rung's largest quality fix.** Grown by 64 px
  of context and then floored at 512², a 40×140 balloon region came back as halftone smudged
  over the text; the same region in a crop sized to itself came back clean. Context is now half
  the long side clamped to 24 to 80 px, snapped up to a multiple of 16, with no floor.
- **Sixty-six renders across eight axes, one parameter changed at a time.** 768 is the working
  resolution: 512 reproduces the smudge, and 1024 and a one-megapixel area target both paint
  invented texture over a flat white balloon for 2× to 6× the clock. Guidance stays 1.0 and
  steps stay 4. The prompt is now one colourspace-agnostic instruction, replacing a pair whose
  screentone arm was clean at seed 1 and no other seed tried.
- **The hardware gate's arithmetic is unified-memory arithmetic, not total RAM.** macOS caps
  Metal residency at roughly 75% of physical memory, 21 to 24 GB of a 32 GB machine. Worked:
  physical 32,768 MB, room 21,665 MB, resident cleaner 2,510 MB, admitted with 20,085,822,804
  bytes. An 8 GB Mac is left 2.77 GB, below the weights alone, and is refused.
- **The cloud rung's costs and terms.** Paid tier only, enforced, because the unpaid tier trains
  on submissions with human review. No mask parameter exists, so a mask sent as a reference
  image is advisory, and no per-ratio pixel table is published, so sizes are probed once per
  model, ratio and tier, about 40 calls under $3. Per image: $0.045 at 512px, $0.067 at 1K,
  $0.101 at 2K, $0.151 at 4K on the flash model, $0.134 at 1K and 2K and $0.24 at 4K on the pro
  model, halved in batch. Its watermark is mandatory and robust to crop, therefore spatially
  distributed, so every returned pixel is potentially altered.
- **The drift gate's primary test is how much correction the model needed.** A uniform tone
  shift is exactly what an affine fit absorbs and it reproduces on an adjacent held-out half, so
  a fit needing a bias beyond 8 levels or a gain outside 0.9 to 1.1 is rejected and clamp
  saturation is a rejection cause. The ring must be at least 32 px wide: at about 10 px the
  outer half gives one block row, so a p99 over roughly 50 samples is the maximum, the statistic
  it claimed to replace.

## Detection and the script gate

- **The detector's output contract, pinned.** 94,669,756 bytes; `images float32[1,3,1024,1024]`
  in; `blk [1,64512,7]`, `seg [1,1,1024,1024]`, `det [1,2,1024,1024]` out. Bind by channel
  count, never by index: upstream carries a defensive swap for exports that reverse two outputs,
  and only the channel count tells them apart.
- **The box classes are languages, not bubble kinds.** The model emits English and Japanese,
  over which upstream itself comments that the class may be wrong. The bubble, text-bubble and
  free-text taxonomy the script gate was built on belongs to a different detector, discussed
  four documents away as the permissive alternative. The scoping survived; the mechanism did not.
- **Detection resolution is a fixed 1024² letterbox,** so the scale factor is the long edge over
  1024 and a 1600×2400 page is scaled by about 2.34. A height-band pre-scale described as
  leaving that page untouched belongs to another tool and sits in front of a detector that
  resizes again: not merely inert, lossy twice. That is also why long pages are cut into
  segments first: an 800×12000 page letterboxed whole is a scale factor of about 11.7.
- **The balloon detector is cheap and accurate:** 11.1 MB int8, Apache-2.0, RT-DETR-v2 rather
  than a YOLOv8 derivative, **0.25 s a page**, emitting exactly the three classes the gate
  wanted, with boxes within about 10 px of the fixture's drawn ellipses.
- **Its size input is width-first,** against the documented height-first order. With the
  documented order every box returns with x multiplied by H/W and y by W/H, 1.5 and 0.667 on a
  1600×2400 page: confident detections in the wrong places, which reads as a bad model rather
  than as a swapped pair.
- **The balloon-geometry second opinion was decided by ring shape.** A closed rectangular ring
  inside a rounded balloon leaves it at the corners long before the sides run out, reads the art
  beyond the rim, and calls picture on a region the detector had right. Sampling the middle half
  of each of four sides turned **43 of 80** swept geometries from textured or unreadable into
  solid, with open screentone still textured.
- **The script model's input convention is undocumented and cost real time.** Input is
  `float32[1,1,48,W]` and is one text line, not a block. A vertical line is rotated
  counter-clockwise, and a mirrored rotation returns plausible labels with no error and no low
  score. A crop whose median is under 64 is inverted first, and values use the source
  recogniser's black and white normalisation. Its output is CTC and the class labelled `Broken`
  is the blank, so averaging the score field returns `Broken` for every input ever tried.
- **Two conventions on top of that.** A recognition margin of 15% of the line's cross-axis size,
  because a hiragana column reads as vertical Japanese at 60 px wide and as Latin at the 43 px
  its mask occupies. And `Common`, `Joined`, `NULL` and `Broken` are abstentions rather than
  votes, since short columns return `Common` and counting it against Japanese would fail the
  punctuation-only boxes that must be classified Japanese.
- **Every CJK label is treated as one class.** The documented confusion mass is Chinese against
  Japanese and it showed up here, a hiragana column returning simplified Han. The question
  actually asked is whether the text is Latin typesetting a localiser added, and against that
  every CJK label is the same answer. Hangul is excluded.
- **Sound effects are conceded, not classified.** The best published detectors reach 61.2%
  H-mean on onomatopoeia boxes and 67.8% on polygons, against 0.889 to 0.918 average precision
  for bubble text in the same corpus, a gap of about 30 points. Their named failure modes break
  any classifier that could be built here.
- **Size thresholds scale by text size, not page size.** Absolute cutoffs are meaningless across
  tankoubon and webtoon, and the fraction-of-page-area replacement was worse: 8e-3 of page area
  is 7,680 px² on an 800×1200 webtoon, 31× smaller than the 240,000 px² dialogue box used to
  condemn the absolute version. Thresholds are relative to the page's median box area.
- **The small-box drop was throwing away dialogue the detector was sure of.** The rule is aimed at
  speckle, and it read the box's size and nothing else. On twelve pages of a real scan, seven
  boxes the detector was 0.84 to 0.94 sure of fell under 0.15 of the page's median box area and
  left the run entirely, not into review but off the page, each of them a short single column of
  dialogue inside a balloon the balloon detector scored 0.74 to 0.90. The one small box on those
  pages that really was noise scored 0.41. So the drop no longer applies at or above a confidence
  of 0.7, the middle of that gap, and the exemption is the drop's alone: an oversized box is still
  flagged whatever the score, because flagging costs nothing and dropping could not be undone.
- **The paper tolerance has to come from the paper, not from a constant or from the page.** On six
  pages of a real scan 32 of 56 regions read as out of a balloon, and every one of the nine the
  balloon detector had placed inside a bubble at 0.88 to 0.94 is a white speech bubble in the
  crop. The scan's paper has a spread of 7 to 10 levels against a fixed tolerance of six, so a
  fifth of every ring was off the fill. The tolerance is now the innermost rings' own spread, the
  92nd percentile of deviation times 1.5, floored at 6 and capped at 20 levels; the page's
  flattest-decile noise could not stand in for it, because it measures 0 on three of the six pages
  whose flattest tiles are saturated white. After the change, 20 of 56, all of them regions the
  detector itself labels free text.
- **A balloon drawn as two lobes was reported as text outside a speech bubble.** The detector emits
  one bubble box per lobe and no text-in-bubble box at all; the lettering nearly fills the shape,
  so the ring walk meets the outline on its first ring and reads picture, and ordinary dialogue
  went to review. Six regions across eighteen probe pages. Sure bubble boxes now overrule the
  paper reading when they cover 75 per cent of the region and one of them holds its centre, and
  coverage is taken over the **union**, which is the load-bearing part: no single lobe covers even
  a third of such a region. The separation is wide, 0.79, 0.87, 0.93 and 0.97 against 0.00 and
  0.34 for the two regions that must stay outside, which fail the centre test as well. The union
  is one balloon's lobes and not every sure shape on the page: a box joins it only by touching or
  overlapping a box that holds the centre, which stops a neighbouring balloon lending the missing
  quarter. A free-text box still wins outright wherever the model drew one.
- **Text the text detector never boxed had no region at all, and the balloon detector had already
  seen it.** Inside a balloon the text detector is complete: over 28 real scans there is a
  text-detector box under every text-in-bubble box. Outside one it is not, and the miss is silent,
  because a region that is never built is not cleaned, not flagged and not listed. Nine boxes
  scored 0.53 to 0.88 had no region over them at all, narration boxes, a caption line running the
  page's width, a stylised sound effect, a chapter title strip and a credits line, and two more
  came out with them for eleven adopted regions. The balloon detector's text boxes are a second
  source of regions now: a sure box that nothing already covers becomes a region and is grown
  through the same four tiers as a detector box. Covered is the page's existing pair of tests, the
  box's centre in some region's masking rectangle or an overlap above the share the extended tier
  already merges on, because centre alone adopts a wide caption whose middle falls between two
  boxes of that same caption and overlap alone adopts a small box in a large region's corner. The
  size rules apply by half: the large-box flag applies, the small-box drop does not, since such a
  box exists precisely because the text detector emitted nothing there to measure it against. The
  joined list is sorted by position before anything counts it, because a region's id is its index.
  The seed was measured before any of it was built: 1,204 px of segmentation under the smallest
  box up to 21,040 under the title strip, a quarter to a third of the box on the large ones. The
  two heads of the detector disagree, and the segmentation head marks the ink the box head never
  emitted a box around, which is what makes the adoption cheap.
- **A narration box has no room around it, so the ring walk read it as picture.** The walk answers
  the paper question only where there is room for an answer; a rectangular narration box is drawn
  with its frame tight against its lettering, so the first ring is already on the frame and beyond
  the frame is the art the plate sits on. Four of them went to review as text outside a balloon
  while the same pages' balloons cleaned. The walk is the first opinion now rather than the only
  one: where it finds no band, the paper **between** the strokes is read instead, every on-page
  pixel the grown text mask does not claim, measured by the same machinery the rings are. The
  order is one way on purpose, since a walk that found a real band has measured a real balloon and
  the inside can add nothing to it. Two thresholds separate the cases and both were measured.
  One-sidedness refuses art, which strays both lighter and darker than its own median, while ink
  strays one way only; that alone is not enough, because a screentone is one-sided too, so the
  off-fill allowance is twice a ring's, a ring being a line of clearance where anything off the
  fill is a defect and the inside of a text box containing ink by construction. Over the 28 scans
  the four narration boxes sit at 6, 10, 10 and 14 per cent of their samples against 19 for the
  nearest thing that must not read as paper, a gap running from 15 to 17 with twice the ring's
  eighth landing in it. The paper floor was measured the same way: the narration boxes keep 50 to
  62 per cent of their pixels after the halo and the least any of the 254 regions keeps is 15, so
  a fifth is a floor against a box that is all stroke rather than a threshold anything real sits
  near. **Twenty of the 254 regions changed verdict, every one of them from out-of-balloon to
  clean or uncertain and none the other way.** What did not move is what the reading is bounded to
  exclude, each checked from its crop: a sound effect over art at 70 per cent off the fill and
  two-sided, a caption over tone at 79, and a credits line reversed out of dark artwork at 74,
  which was expected to flip and does not, because that line is not on paper. The reading was
  removed on 2026-09-29, when it was found to pass sound effects with a white outline as paper
  (see "One in/out answer for text" under What has not been measured).
- **A Japanese-only reader cannot be the gate and is the right instrument for the gate's
  failures.** The identifier returns uncertain on ordinary dialogue in ordinary balloons, and
  occasionally names something exotic, Tibetan on a tall kana column, Syriac on another, because
  those scripts are also tall and thin and a 48-pixel strip of vertical kana is inside their basin.
  Over the 28 reference scans, 254 regions, **12 were offered to the reader and 12 rescued, 4.7
  per cent, with none offered and declined**, and every one is a speech balloon a person would
  clean without hesitating. The rescue runs under three conditions, each a refusal to widen: the
  balloon detector placed the region confidently inside a bubble, since a reader whose whole
  vocabulary is Japanese will read something off a sound effect too, so the permission cannot come
  from the reader; the identifier failed, either uncertain or naming a script outside a short
  trusted allow-list, so a confident Latin is never second-guessed; and the reading is at least
  two characters and at least 60 per cent written in a Japanese block, on the same letters-only
  rule the gate's own decision rules use. A rescued region carries a script of its own, so
  provenance and the probe tell a rescue from an identification without re-running anything. The
  negative control is the page with the title strip and the credits line: nothing on it was offered
  to the reader at all, because neither of those is inside a confident balloon and the balloons
  were already clean from the identifier. The reading is evidence about script and not a
  transcription anyone consumes: one rescue is very likely wrong about the characters and is still
  right that the balloon holds Japanese, which is the only question asked. It costs 164 ms for a
  ten-character balloon on the CPU, 232 at 26 characters, 275 at 28 and 808 at 36, decoding
  greedily and stopping at 64 tokens.
- **Fixtures are generated and are not a corpus.** The detector emits no box for either large
  sound effect on one of them while the segmentation mask has clear signal inside both, the
  conceded weakness appearing where predicted. And three of the four have a noise floor of 0.0,
  so every region routes to flat fill and the denoise rung never executes: the page added for it
  carries normal grain at sigma 2.6 levels, because a uniform draw over a handful of levels is
  bimodal by the ring's own test and would be refused as multimodal.

## Mask fitting

- **Growing the mask natively removed two corrections rather than compensating for them.**
  Growth on the proxy needs a native annulus, because the upscaled contour lands on the
  anti-aliasing fringe and biases the median 5 to 20 levels dark, and needs the periodicity check
  on a two-dimensional patch, because a nearest-neighbour stair-step is itself periodic at the
  scale factor. It is free, and at proxy resolution the gate's line splitting flipped three of
  five regions from vertical to horizontal and turned three Japanese verdicts into uncertain ones.
- **The strong-edge rule was carried for the mask and not for the ring one pixel outside it.** A
  bubble stroke inside the annulus is tens of levels of deviation on flat paper and self-similar
  along its length, so it fails the deviation test and triggers the periodicity test at once.
  Four regions on the crowded fixture were routed to the inpaint ladder by a statistic that had
  measured the balloon rather than the paper.
- **No threshold could have separated the two.** A stroke's peak autocorrelation is 0.91 against
  0.58 to 0.81 for real halftone fields, so the stroke scores higher than the thing the test
  looks for. Only the pixel set could fix it: the ring is restricted to the paper on the mask's
  own side of the page's strong edges.
- **Screentone is not far below the strong-edge threshold.** On the crowded fixture 24 to 29% of
  screentone pixels are strong: dots at level 60 on paper 246 is 0.82 of the page's peak gradient
  against a threshold fraction of 0.5. What keeps a mask off screentone is that its border lands
  on a dot, so the same test fires for the opposite reason to the documented one. The test named
  for the claim built its tone at 200 on paper 250, a fifth of the real contrast.
- **Periodicity is autocorrelation over the annulus only:** over its bounding box every
  flat-paper balloon on the dialogue fixture came back periodic, since a column of glyphs is
  self-similar at its own pitch. Edge crossing is gradient magnitude on the border only, since
  testing the interior rejects the first candidate on every page.
- **The fail threshold is relative to the page's noise floor.** A fixed 8.0 sits below the border
  deviation of flat white on a JPEG raw at quality 75, which is 5 to 12 from blocking alone. It
  is the larger of 8.0 and 2.5× the page noise sigma, measured once per page from its flattest
  decile, and the denoise trigger is relative for the same reason.
- **A region carries two masks, and treating them as one put a visible box on the page.** On
  flat paper every candidate's deviation is zero, so selection ratchets to the end of the series
  and the mask is the seed grown by 4 + 11 × 2 = 26 proxy pixels, 64 native at a 2.34 scale: a
  65×169 detection box became a 38,985 px mask, 355% of the box's own area.
- **That growth is free for a fill and ruinous for a model.** LaMa answers a hole a level or two
  off the page's tone, measured at 2.5 8-bit levels of drift even 65 px from the hole, and a
  mask grown across a balloon gives that offset a long smooth border to show itself along. One
  region: 33,912 pixels written and a visible blob, against 12,467 and none through the narrower
  mask. The model's input is unchanged either way.
- **A region is never dropped for want of an admissible mask.** When the first growth step's
  border sits on a strong edge, no member of the series is admissible, but the radii below it
  are not members and each is tested in turn. On seven such regions this also cut residual ink:
  337, 406 and 8,311 pixels against 642, 627 and 11,666.
- **The denoiser is bilateral over a 5×5 window with its range sigma set to the page's noise
  floor,** the same measurement its trigger uses, which lets one rung serve a clean scan and a
  JPEG raw with no tuned constant between them. Twenty-five samples cut a deviation by up to 5×,
  while a wider window costs quadratically and starts averaging the structure it must preserve.
- **A 5 px dilation composed with a radius-1 feather escapes a 6 px edit margin.** The element
  is a disc at radius 5 and a square at radius 1, so the composition reaches the square root of
  37, about 6.08 px, at four corners per region. One dilation by 6 does not. Off by 0.08 px on
  four pixels, found by a test asserting the bound rather than by inspection.

## Long strips and memory

- **The measured budget, process RSS including the webview.** Webview 120 to 180 MB; core idle
  about 40 MB; detector session about 195 MB against a design figure of 50; **detector allocator
  arena about 1.2 GB**; script model about 10 MB; MI-GAN session about 20 MB against a design
  figure of 90; LaMa session about 350 MB on WebGPU and 410 MB on the CPU provider; decode
  window and working buffers about 30 MB.
- **The arenas invert by provider at roughly 30×.** MI-GAN's is about 15 MB on WebGPU and 345 MB
  on the CPU provider, LaMa's about 10 MB and 310 MB, because on WebGPU the activations live in
  GPU buffers that never enter the resident set. Neither number is predicted by anything on disk.
- **Whole-run peaks, eight copies of a twenty-region page through one held session:** 2.07 GB at
  flat fill and denoise, 2.45 GB with LaMa, 2.24 GB with MI-GAN. Independent of document length
  and of region count.
- **Length independence holds; the level was four times the estimate.** Over 200 synthetic
  800×1280 pages at the two cheap rungs: 5 MB before any model, 327 MB with three held sessions,
  1506 MB after the first detection at 1024², 1607 to 1707 MB for pages 3 to 200, peak 1.89 GB.
  The peak arrives inside two pages and does not move over the remaining 198.
- **The arena is what a budget written as a list of resident objects has no row for, and it
  steps once per distinct input shape.** One inference over the fixed 1024² input adds about
  1.2 GB and never returns it; over eight copies of one page it rises 1.2 GB on the first
  detection and 400 MB more on the second page, the first segment crop of a different height,
  then does not move. A step, not a slope.
- **A held LaMa session is two objects.** A reading of about 510 MB is the digest's read of the
  weight file, which leaves 198 MB of a 207 MB file resident and never returns it, plus 347 MB
  for the session. The pressure ladder's unload step therefore returns 350 MB, not 510.
- **The obvious pressure signal on macOS reads 0.** The per-process available-memory call
  reports room under a per-process limit, and a plain desktop process has none, so read as bytes
  available it steps every machine all the way down at the first reading and the step latches. A
  constant signal is not conservative; it is a disabled feature that looks like a working one,
  and it was linked and called rather than read about because the failure is silent.
- **The signal used is the kernel's own memory pressure level,** polled between regions, the
  only moment at which what is resident is the sessions rather than a region's buffers. Under
  pressure the order is largest and most optional first: refuse and unload the redraw sidecar,
  then unload LaMa, then drop to one window in flight (one under 16 GB of machine memory, two
  above). Windows and Linux have no equivalent signal and take no step.
- **Resident set size cannot see unified memory, and that error survived a whole revision.** Six
  instruments on one guarded 512² render: three resident readings agree at 2.883 GB and three
  footprint readings agree at 6.576 GB, so there is nothing to average. A mid-render sample says
  where it is: graphics accelerator 3,842 MB, large allocations 153 MB, small allocations
  140 MB, untagged 133 MB, and 46 MB of owned footprint that is unmapped and that no resident
  reading can ever contain.
- **The instrument does not merely understate, it does not move.** At 1024² the true peak is
  11.46 GB and the resident reading is 2.90 GB, eighteen megabytes above what it read at 512²
  while the real cost grew by 4.9 GB. A peak below the 4.6 GB weight size should have wanted an
  explanation, and the runtime self-reported 4.90 GB in the same run.
- **Split planning.** Splits target 2000 px with a tolerance of 500 widening to 1000 and 1500,
  triggered below a strip aspect ratio of 0.33, and a candidate row is accepted only below 10%
  of the page median. Rows are scored by edge energy, not ink density, which has the wrong sign
  on inverted content: white text on a black panel is a low-ink row. The profile is one float
  per strip row, so a 200-page webtoon is 3.2 MB of profile and one page of pixels.
- **The interface's chapter-scale allocation was region state, not pixels.** Opening a chapter
  answered with every page's region list, four thousand objects for a 200-page chapter, live for
  the session. Three pages' regions are resident now, plus per-page counts and a review index at
  about 90 bytes per flagged region. Undo history was the other half: a command held as a
  closure over two region snapshots pinned every page the user had edited.

## Colour fidelity and export

- **The canvas-based export path corrupts every file it touches, before any engine runs.**
  Canvas has one pixel format, 8-bit sRGB with alpha, so it promotes grayscale raws to RGB,
  truncates 16-bit to 8, discards the ICC profile, cannot represent CMYK or indexed sources,
  destroys source alpha by filling white first, and re-encodes untouched pages. The intermediate
  is corrupted the same way, so it is not salvageable by patching.
- **Twelve fixtures round-trip byte-identically** through the pure-Rust codecs: gray and
  gray-alpha at 8 and 16 bits, RGB and RGBA at 8, RGB at 16, three ICC variants, indexed with a
  free palette slot, bitonal, and CMYK TIFF. Bitonal was added because a 1-bit image is the only
  one whose rows pad, the case a length assumption gets wrong.
- **Writing the PSD format was cheaper than hardening a crate that writes it.** What this
  application asks of the format is small and fixed: a header, an ICC resource in image resource
  1039, a background layer, one masked layer per region in one group, and a merged image. With raw
  channel data, which every reader accepts, every length in the file is arithmetic on the
  dimensions, nothing is buffered in order to be measured, and there is no run-length writer to
  drop a byte. That is a few hundred lines, and in those lines grayscale, alpha, CMYK and 16-bit
  cost the same as 8-bit RGB: a channel count, a mode code, a bytes-per-sample. Vendoring would
  have bought a hardened 8-bit RGB path plus a grayscale patch and left 16-bit deferred. Two of
  the format's less obvious rules are followed the way Photoshop follows them, because a reader
  that is not Photoshop would not have caught either: a 16-bit document keeps its layer records
  under the `Lr16` additional-info block with the standard layer info empty, and CMYK is stored
  inverted. An independent reader opens every fixture the writer produces, layered and flat, at 8
  and 16 bits, with the mode, depth, layer tree, layer masks and merged pixels expected. The files
  are larger than Photoshop's own, roughly the uncompressed page per layer, which is the price of
  the property that makes every length computable.
- **The mask file is made of the ink, not of what an engine painted.** The union of each patch's
  ink mask, the lettering the edit removed, rather than the union of the applied masks: on a flat
  fill the applied mask is the whole grown balloon, and a file made of those is a page of white
  blobs where the balloons were, which is not what a typesetter asking where the text was wants. A
  layer's own mask inside a PSD stays the applied one, because a layer mask states what the layer
  contributes.
- **Pages mostly do not arrive in a format this stack writes, so they are converted once, at
  ingest, into the library.** A JPEG, WebP, GIF or BMP is decoded by a pure-Rust codec and written
  as a lossless PNG, a CMYK source as TIFF, into the job's own folder, and a PNG or TIFF takes the
  same road with a byte-for-byte copy in place of the decode. That file is the page from then on,
  so everything downstream of ingest sees PNG and TIFF and nothing else, and the user's folder is
  read once and never read or written again. Four codecs is where the list stops because all four
  are pure Rust: AVIF wants `dav1d`, HEIC wants `libheif` and JPEG XL wants `libjxl`, and taking
  any of them reopens the decision that removed the C image library. Two consequences are stated
  rather than absorbed: a chapter's disk cost is now a second copy of every page, on the order of
  a gigabyte for a 200-page colour chapter with no free-space check in front of it, and the resume
  hash is over the job's own copy, so a user re-cropping the scan they ingested from no longer
  marks anything stale. What the hash still catches is the failure it was written for, now over a
  file this application owns: truncation, corruption, or the page going missing under a job about
  to resume onto it.
- **The sRGB and ICC chunk trap is worse than "never set both".** The encoder writes the profile
  in the else branch of a test on the sRGB intent, so a source PNG carrying both chunks, which
  the specification allows, silently loses its profile on any re-encode that copies the decoded
  header across. The intent is kept out of the header and the sRGB chunk written by hand.
- **Indexed sources get flat fill alone.** A denoiser averages levels and a palette index is not
  a level: the mean of index 3 and 5 is index 4, an unrelated colour chosen by whatever order
  the palette happens to be in. Mapping back to the nearest entry is per-pixel near-white
  snapping, and inpainted output is continuous-tone and unrepresentable in a palette at all.
- **CMYK is restricted to the two exact rungs.** The model rungs are RGB-only and the round trip
  is not invertible: rich black at C60 K100 becomes RGB black and returns as K100, so an
  inpainted patch carries different ink separations from its neighbours. Invisible on screen,
  ruinous in print, which is the only reason CMYK is supported.
- **A silent substitution is a different failure from a downgrade.** A format the build could
  not produce was mapped to "same as source", so a user who chose JPEG received PNG and was told
  the export succeeded. That never violated the pixel contract, since the substitute was always
  lossless. It violated the word "silently", and only one of the two is visible in a test.
- **Passthrough is decided by per-page intersection with the global patch set, not by patch
  ownership,** which is the answer already sitting in the manifest. Take ownership and the page
  below a join reports zero applied patches, is copied byte for byte, and arrives with the
  bottom of a bubble missing, while its file is bit-identical to its source and so passes the
  fidelity test, which is an upper bound on change and cannot see a change that did not happen.
- **The subset property holds by construction, which is exactly its limit.** The compositor only
  writes inside the mask plus isolation, so no model response can break it, and the test
  therefore says nothing about whether the set written was the right size. Reading it as though
  it did is what put a visible box on a page.
- **Model output is not bit-reproducible, and not only across providers.** The same provider and
  version can differ between consecutive runs, with a documented scatter-operator case cured
  only by a single intra-op thread; machines differ through instruction-set dispatch; the graph
  optimisation level changes outputs. Golden tests are therefore tolerance-based at about 1e-4
  on the CPU provider only, and everything upstream of the models is made deterministic instead.

## Runtime and packaging

- **Fetching the runtime after install costs one entitlement.** The signing matrix was run five
  ways: an ad-hoc process loads an ad-hoc runtime; a Team ID process under the hardened runtime
  refuses it, the system reporting different Team IDs; with library validation disabled it
  loads; a runtime re-signed with the same Team ID loads with no entitlement, which is only
  available for a library that ships in the bundle. Every official release is ad-hoc and
  linker-signed with no Team Identifier, so the design requires disabling library validation,
  which weakens the process for every dylib and is recorded as a trade, not a free sidestep.
- **The runtime is not one build, and providers differ per archive:** macOS arm64 at 31 MB with
  CPU, CoreML and WebGPU; Windows x64 and arm64 at 76 and 77 MB, CPU only; Linux x64 and
  aarch64 at 9 and 8 MB, CPU only; the DirectML package at 13 MB plus its 202 MB dependency;
  the two Windows CUDA archives at 449 and 353 MB with CPU, CUDA and TensorRT; and no Intel
  macOS archive published at all.
- **The provider name strings are in every build and prove nothing.** A check that grepped them
  reported that all six archives carried every provider, which is how it was found to be
  worthless. What ships only where a provider was built is the provider factory headers and the
  provider libraries. The script had nonetheless drifted back to the discredited method, so the
  artefact meant to re-derive the table could not have produced it.
- **Three facts fell out of reading them properly.** The CUDA archives carry no DirectML, which
  makes the two mutually exclusive, and no CUDA runtime or cuDNN, which makes CUDA the only
  provider here with an install behind it. From 1.28 the native WebGPU provider ships as a
  separate plugin, so the macOS archive is the one that still has it compiled in. And the
  Windows GPU download is 215 MB and not 12, because that path is two artefacts and the second
  is the large one, which had been the wrong number in an argument used against CUDA.
- **The separate WebGPU plugin is one artefact for four platforms, and it is taken.** 39 MB, one
  package covering Windows x64 and arm64, Linux x64 and macOS arm64, and it registers into any
  runtime from 1.24.4 up, which is every build in the table. It therefore ships as a companion
  download beside every Windows and Linux x64 runtime, is registered once at load and reported as
  absent, built in, registered or failed rather than failing the load, and is skipped where the
  runtime already has the provider compiled in, which is macOS. The library API for plugin
  providers already existed, so nothing was hand-rolled through the C API. On Windows the
  runtime's own directory is put on the library search path before the plugin is opened, because
  the plugin is loaded by name and pulls in two shader libraries by name, and neither searches the
  directory it came from. Linux gained two NVIDIA archives on the same terms as Windows, 424 and
  241 MB, whose digests came from the release's asset record without downloading them.
- **Pixels reach the webview over a custom protocol, and Windows is the constraint.** Base64
  data URLs inflate payloads by 33% and decode on the main thread; one reported migration of a
  150 MB response went from about 50 s to under 60 ms. The Windows webview raises its
  resource-request event on the host UI thread and pauses page load: a maintainer benchmark
  measured about 5 ms on macOS against about 200 ms on Windows for 10 MB, roughly 50 MB/s, with
  no range or streaming responses, so responses must be whole tiles.
- **The Windows runtime imports the Visual C++ runtime outright, and nothing shipped it.**
  Parsing the PE import directory of the shipped
  `runtimes/packages/.dml/runtimes/win-x64/native/onnxruntime.dll`, its regular imports are
  `MSVCP140.dll`, `MSVCP140_1.dll`, `VCRUNTIME140.dll` and `VCRUNTIME140_1.dll` beside
  `KERNEL32`, `ADVAPI32`, `SETUPAPI`, `dbghelp` and ten `api-ms-win-crt-*` names. Regular and not
  delay-loaded, so they resolve when the library is mapped and their absence fails the load
  outright. The ten `api-ms-win-crt-*` names are the universal CRT and are part of Windows 10, so
  only the four `140` names are a user's problem. They are in neither NuGet package, the
  installer bundled WebView2 and not them, and a machine without the redistributable could
  therefore do nothing at all: `LoadLibraryExW` fails with 126 and the interface called it
  `unloadable`, a sentence with no remedy in it. Fixed twice over - the libraries are staged
  beside the executable from the build host's own Visual Studio redist folder, and the loader
  reports a missing dependency as its own case so that a machine the staging missed is told what
  to install.
- **App-local is the right place for them because of how `ort` opens the runtime.** It calls
  `libloading::Library::new`, which is `LoadLibraryExW(path, NULL, 0)` with no
  `LOAD_LIBRARY_SEARCH_*` flags, so the runtime's own imports resolve through the standard search
  order, whose first entry is the directory the application loaded from. Tauri puts bundled
  resources in that same directory on Windows, verified by running `tauri_utils`'s own
  `ResourcePaths` rather than read out of the documentation. The trade is real and is not free:
  an app-local copy gets no Windows Update servicing, so a CRT security fix ships as a release of
  this application.
- **Two operational traps around the runtime download.** A quarantined copy is refused outright
  and loads once the attribute is cleared, which a browser download sets and a library download
  does not; and the dylib's own load command names a versioned filename, so a copy saved under
  another name fails to resolve itself with an error that reads exactly like a signing failure.
- **Throughput has a target because nothing else measured it:** at least 20 pages per minute on
  the two cheap rungs. Detection at 350 to 383 ms a page on CoreML leaves that budget intact.

## Models and licensing

- **Mirrors re-tag checkpoints under licences their sources did not carry.** Three separate
  mirrors here did it: one detector export is tagged Apache-2.0 while the same author's weights
  repository and the upstream project are both GPL-3.0, and one inpainting export is tagged MIT
  while its source checkpoint comes from a GPL-3.0 project with undisclosed training data. Trace
  weights to their training source. The same standard applies to bug reports: a claim that a
  defect was fixed upstream is a claim about a version, and the version pinned here still had it.
- **Every YOLOv8-derived detector is AGPL-3.0 through its training framework,** regardless of
  the repository's tag, and the framework's position is that this holds even for models trained
  from scratch. Two Apache-tagged candidates were voided by it. Check architecture lineage, not
  the model card.
- **Verified clean.** The LaMa architecture upstream is genuinely Apache-2.0 with no
  non-commercial clause, and the manga finetune declares MIT at 204,544,673 bytes with a
  published ONNX export at 207 MB and opset 17, so the default inpainter was never
  licence-blocked and the revision that demoted it had no licensing basis. The script model is
  Apache-2.0 at 3.72 MB and MI-GAN's code is MIT, though MI-GAN's weights inherit a lineage
  through three earlier generative models whose licence may cover code alone, which blocks an
  optional tier rather than the product.
- **One mirror survives the rule rather than being caught by it.** The optional Japanese reader's
  weights are an ONNX export by the same mirror as the inpainter's, three files of 343,454,249,
  117,480,262 and 30,216 bytes, and they are clean because their source is: the upstream model
  declares Apache-2.0, its training corpus is disclosed in the model card, and its own lineage is
  an Apache-2.0 vision encoder with an Apache-2.0 Japanese character decoder. There is no
  permissive re-tag of a restricted checkpoint here, only a conversion of a permissive model.
  Traced to the training source, as the rule requires, and not accepted because the mirror said so.
- **A Manga109-trained detector would have answered several open questions and is not takeable.**
  The published ones are Ultralytics YOLO, so AGPL-3.0 through the training framework whatever
  their own tag says, and the dataset itself is licence-gated, which blocks training a replacement
  as surely as the framework blocks using theirs. Two separate reasons, either one sufficient.
- **Exemplar synthesis was removed on patent grounds.** The PatchMatch patent has a 2008
  priority and is active until 2031, with a family member to 2030, and the reference
  implementation is licensed for non-commercial research only. Reimplementing it in another
  language does not help: clean-room defeats copyright, not patents, so treating a rewrite as
  risk elimination was risk conversion, from a build problem into a legal one.
- **One cloud provider's terms are incompatible with the users' own inputs.** They take a fully
  paid, royalty-free, perpetual, irrevocable, worldwide and sublicensable licence over inputs
  and outputs and state they may be used for training, with opt-out only by manual email. Users'
  inputs are third-party copyrighted scans and they cannot grant that licence.
- **The only viable Rust PSD writer is dangerous rather than merely immature.** One star, two
  commits, no CI, 215 downloads, an author who states the code was machine-generated and never
  hand-audited, and a fixture round-trip harness excluded from the published crate, so its own
  test suite never round-trips a file. Its write path panics on at least five conditions instead
  of returning an error, and its run-length writer silently drops out-of-bounds writes with call
  sites defaulting on failure, so an encode failure produces an empty channel: a corrupt file
  that reports success. Its pixel container is interleaved 8-bit with a stride hardcoded in three
  functions, so 16-bit would have been a fork rather than a feature. It was not taken: the format
  is written in the core instead, which cost less than hardening the crate and gave every mode and
  both depths rather than a hardened 8-bit path with 16-bit deferred.
- **The image library was reversed on its own headline requirement.** The C library first chosen
  hard-codes indexed images to RGB at load, so the palette is destroyed before anything else
  happens. The rest of the case collapsed with it: about 18 MB per platform (17.78 MB on
  darwin-arm64, 19.15 MB on win32-x64), 29 transitive libraries mostly dead weight for PNG and
  TIFF, LGPL components that put static linking off the table, and a binding whose own README
  warns its image type is not thread safe.

## Things that turned out wrong

- **"The CoreML provider cannot run LaMa."** It runs, 53% slower, after a 23.7 s session build.
  The conclusion held and the reason did not, and a reader designs around a hard failure.
- **"The inpainter is CPU-only and that is accepted."** It is 1.9× faster on WebGPU.
- **"MI-GAN is the CoreML-accelerated fast tier."** CoreML makes it 1.6× slower; it is fast
  because it is 28 MB. "No export work" was wrong too: the published file is a pipeline.
- **"WebGPU ships in the stock runtime build on every platform."** True of the one build
  measured, written in the same session as a warning against generalising a one-platform figure.
- **"`libloading` opens the runtime with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR`, so `DirectML.dll`
  is found beside it."** It does not: `libloading` 0.9.0's `Library::new` is
  `load_with_flags(filename, 0)`, flags zero. What actually resolved `DirectML.dll` was the
  `SetDllDirectoryW` call added for the WebGPU plugin, working by accident on a delay-loaded
  import. A comment naming the wrong mechanism is worse than none, because the call it depends on
  reads as removable. The dependency is now stated, and the library is pinned by absolute path so
  that resolution no longer rests on who owns the one directory slot `SetDllDirectoryW` holds.
- **"A 1600×2400 page is not downscaled at all."** It is scaled by 2.34 into a fixed 1024².
- **"The box classes are bubble, text bubble and free text."** Two language classes; the three
  names belonged to a different model.
- **A memory budget of about 250 MB and 500 MB.** Measured at 2.07 GB and 2.45 GB, and it then
  turned out to exist in three places, only one of which was ever corrected.
- **25 GB for the local redraw model.** That measured how a caller drove the model: the
  reference implementation keeps its memory discipline in a callback only its command-line front
  end registers, so a library caller gets an unbounded cache and an encoder that is never evicted.
- **2.89 GB for the same rung once it was built here.** The wrong instrument: 6.58 GB. And the
  guards were claimed at 8.6×, a ratio between two different quantities, rather than 2.3×.
- **An 8,768 ms render.** It has never reproduced; six runs land between 19.5 s and 24.4 s.
- **The per-process available-memory call as the pressure signal.** It returns 0 for every
  process on the machine.
- **The fitted mask as the hole handed to a model.** On flat paper that is 355% of the detection
  box, and it produced a visible light box in the panel.
- **"Screentone is far below the strong-edge threshold."** A quarter of it is above, and the
  rule that comment explains works for the opposite reason.
- **"Outside-mask pixels never leave the machine" for the cloud rung.** False: the tone fit and
  the drift gate need the ring, so a bounded crop including surrounding art is transmitted.
- **"A stroke-tight hole leaves the least ink."** True of eleven fixture regions and false on the
  first real scan it met, where the tight hole is what the model draws a glyph into.
- **"The only viable Rust PSD writer must therefore be vendored."** The format this application
  needs is a few hundred lines, and vendoring would have bought less for more.
- **"The small-box drop only catches speckle."** It caught seven lines of dialogue on twelve pages,
  each of them inside a balloon the balloon detector was sure of.
- **"A bubble box is a weak answer and a paper reading may overrule it."** Not when the sure bubble
  boxes cover the region between them, which is what a balloon drawn as two lobes looks like.
- **"The ring walk can always answer the paper question."** Not for a box whose frame sits tight
  against its lettering, where there is no room for a band and the answer came back as picture.
- **"A Japanese-only reader has no place in the design."** True of the gate and false of the gate's
  failures, which are a reading problem and not a classification one.
- **"A CUDA flavour puts the inpainter back on the CPU."** Not since the WebGPU plugin ships
  beside every Windows and Linux x64 runtime.

## What has not been measured

**Detect text here, a Detect of one area (2026-10-04).** The Selection tool's right-click over bare
paper starts a Detect held to a box around the pointer (docs/detect-clean.md, "`run_clean` on one
area"). The models read a 1024 px window at the page's own resolution, and what they find is added
to the page's detections.

Measured: on scan 19 of the 28 reference scans (1080x1536; CTD, Ogkalu Small and SAM-TS-L on this
computer, all text, padding 0) the page Detect stored 18 regions. Each was deleted in turn and
asked for again at its own middle, and 18 of 18 came back, with boxes within 2 px of the deleted
ones for 16 of them. One area Detect took 7 to 10 s against 9 to 17 s for the page.

Not measured, or known to be incomplete:

- Whether the window finds text the page Detect misses. On scan 19 a grid of 24 spots added
  nothing, but that page is only 1.5 times the window. The gain should be on larger pages, where
  the whole page is scaled down further; no page above 1536 px was tried.
- The window groups text by what it sees. On scan 19 the caption at the top left came back joined
  to the outside text under it. The pixel cut stored the right lettering, but as outside text,
  where the page Detect had it inside a balloon, and its box was 7 px smaller on each side. The
  user can set the text type from the region menu. Nothing corrects it automatically.
- A cloud stage sends the whole page, as a page Detect does, so the cloud cost of one area is the
  cost of the page. Not run against a deployment.
- A long strip and a page with a rotated source were not run with real models; the unit tests
  cover one 1600x2400 page.
- The area is a fixed box, 16% of the page's width. There is no way to drag the area.
- The mock backend detects the whole page when asked for an area.

**Modal routing region and result download resume (2026-10-04).** The gateway can be deployed behind
Modal's proxy in `us-east` (default), `us-west`, `eu-west` or `ap-south` (`routing_region`, see
docs/cloud-provisioning.md), and a result download now resumes with `Range` when its transfer stops.

Why: from Dhaka (about 250 ms round trip to us-east-1) the download direction from us-east dropped to
7 to 11 KB/s on 2026-10-04 around 20:50 local time, while a nearby edge gave 8 MB/s and uploads were
fine. A 500 KB Qwen result took 63 s between the gateway's `GET .../result -> 200` and `result.png`
appearing in the attempt folder; a 1152x960 result (about 1.8 MB) never arrived and the attempt stayed
`accepted`. The same step took about 2 s on 2026-10-03 and earlier on 2026-10-04. reqwest's blocking
30 s timeout applies to each read, so a trickle survived and one stall failed the download, with no
retry.

Measured with a throwaway CPU-only Modal app serving 1.8 MB of random bytes through both proxies, three
runs each, about an hour and a half after the slow period (the us-east path had recovered by then):

| route | TCP connect | first byte | 1.8 MB download |
| --- | --- | --- | --- |
| `us-east` (default) | 0.26 to 0.28 s | 0.92 to 0.97 s | 2.8 to 3.4 s (0.52 to 0.65 MB/s) |
| `ap-south` | 0.06 to 0.08 s | 0.36 to 0.39 s | 0.8 to 1.7 s (1.0 to 2.3 MB/s) |

The probe also confirmed that a web Function with `name=` and `routing_region=` deploys, is found by
`Function.from_name`, sits behind proxy auth (401 without a token) and gets a
`<label>.ap-south.modal.run` URL.

Not measured:

- The `ap-south` route during a slow period of the us-east path. The comparison above was taken after
  the path recovered, so it shows the route is faster, not that it avoids the collapse.
- A real installation moved to `ap-south`: provision update, token re-key to the new origin, a Qwen
  render and its review, all in the installed app. Covered only by the fake-SDK driver test, the mock
  setup screen and unit tests.
- Whether the proxy-to-container hop (Mumbai proxy, container placed by Modal, usually in the US) adds
  delay to the one-second status polls.
- The resume path against the live gateway on a stalled link. It is covered by a loopback test in
  `http.rs` and by the gateway route test.
- Result payloads above 2 MiB still pass through Modal's us-east object storage whatever the routing
  region (Modal's documentation). The largest result seen is 1.8 MB at 1152x960; the wire limit is
  16 MiB.

**Hayai text reader (2026-10-02).** `gate/hayai.rs` reads regions with Hayai OCR v2.5 Nova (ja, zh,
ko, en; exported to ONNX in `spikes/hayai-ocr`, hosted at `bixii/hayai-ocr-v2.5-nova-onnx` and pinned
by commit and hash in `weights.rs`). It is the optional text reader switch in onboarding and Settings
(`ocrRescue`, group `hayaiOcr`), under both text policies, and always on under Cloud GPU detection.
With its files installed a run holds as `gateSkippedNotText` any region it would clean without a
script reading, or without a text box, whose reading is not lettering (`Reading::is_lettering`); it
rescues failed verdicts in a balloon in any CJK script (`Hangul_ocr`, `Han_ocr`, `Japanese_ocr`),
in a weakly found balloon only at 0.75 or more (`shape_rescued_by`), and overrules a confident
`Latin` only with a confident kana or Hangul reading (`overruled_by`). manga-ocr is not opened then.

Measured with the headless real run (Detect only), reader off then on, regions cleaned:

| pages | CTD + Ogkalu Full, legacy | + SAM-TS-L, All text |
| --- | --- | --- |
| Black Jack ch. 1, 11 pages | 37 to 42 | 84 to 56 (28 held, nearly all art) |
| `~/dev/120 noisy png`, 10 pages | 106 to 111 | 130 to 121 (8 of 9 held are art) |
| apocalypse 109 webtoon (Korean), 12 strips | 2 to 5 | 33 to 23 |
| 8 public-domain Taiwanese comic pages (Wikimedia Commons) | 81 to 82 | 188 to 166 |

The Hayai download was run through the app's own fetch code (`weights.rs#fetch_verified`) from the
pinned URLs; all three hashes matched and the downloaded graphs read the test balloon. The final
code (reader provider rules, CPU retry, cloud combination) was rerun headless on Black Jack with
those downloaded files and gave the same 42 and 56. Providers and
sizes are in `accel::HAYAI` and the spike README: CPU wins on the M5, CUDA helped on a cloud L4, FP16
reads the same at half the size but is slower on the CPU, int8 misreads. Only FP32 (573 MiB) is hosted, by
choice (2026-10-02): it reads every test region right and is the fastest on the Mac; FP16 would
only speed up NVIDIA machines. Not yet done:
- Korean sound effects drawn as single strokes (슈) read as kana (イ) and stay held: 7 of the webtoon's
  26 sound effects in the All text run. Chinese single-character sound effects and title blocks that
  share a region with art are held too (about 9 of 22 holds). Every threshold is from these four
  sets; nothing was swept.
- No real cloud run with CTD and Hayai on this computer beside cloud Ogkalu Full and SAM-TS-L. That
  mixed run was refused before 2026-10-02 and has never run end to end. CTD and Hayai have no cloud
  stage; they are light and give the same answer here, so a cloud capability for them was not built.
- DirectML, ROCm and the Windows and Linux CUDA flavours were not run. CUDA is an unmeasured candidate
  from the L4 timing; DirectML is not offered because WebGPU showed the vision graph's antialiased
  `Resize` fails mid-run, and the CPU fallback in `Hayai::read` was tested only through unit code.
- The per-region Try again path was not run with the reader.
- No run of the desktop app window with the switch on: computer control of a test build was declined.
  The onboarding and Settings screens were checked in the Vite mock; the download and Detect only
  headless.

**Mask padding (2026-10-02).** Text cleanup's Mask padding slider grows each fitted mask by 0 to
32 page pixels at Detect (and in a one-pass run), and its Apply re-pads masks already detected from
their stored unpadded pair (`region.rs#set_detection_padding`). Unit tested on synthetic masks and
checked in the Vite mock only. Not yet run:
- A real Detect and Clean with padding on the user's scans (`~/dev/120 noisy png`), in the app or
  the headless real run: whether a padded mask helps LaMa or the fill, and whether the quality
  check declines more regions once the mask reaches into its ring.
- No default other than 0 was chosen; nothing measured says which padding suits which page.

**Padding merge (2026-10-05).** Detections a padding runs together (overlapping, or nearest pixels
within 4 px) are stored as one, at Detect and in `set_detection_padding` (`region.rs#run_together`,
`made_one`; docs/detect-clean.md). Unit tested on synthetic masks. Not yet done or run:
- A one-pass run (`auto` on a page with no stored detections) cleans each region as it is found
  and merges nothing; only Detect and a later change of padding merge.
- The mock pads boxes and never merges, so the merged notice and a vanishing Layers row were not
  seen in the Vite mock, only in tests.
- The 4 px reach is a choice, not a measurement: no real scan was used to pick it.
- No real Detect and Clean of a merged region on the user's scans: a merged region drops its fit
  and cleans as a hand mask, and two balloons merged across their wall start on LaMa.
- A merge cannot be undone except by Detect on the page again; no split was built.

**Canvas scroll room (2026-10-05).** A zoomed page and a long strip keep half a viewport of empty
canvas above and below (`CanvasStage.svelte`, "Scroll room"), a saved scroll is measured from the
page at rest (`editor.scrollRoom`), and the saved scroll is restored once the pages have loaded
instead of against the loading placeholder, which lost it for a zoomed single page. Unit tested,
and checked in the Vite mock in Chromium. Not yet done or run:
- Not seen in the shipped WKWebView. The room below the page is a bottom margin on a flex item;
  an engine that leaves that out of the scroll area gives the room above only.
- No room at the sides: the page still stops 104 px from the left and right edges.
- The user also reports that scrolling up sometimes stops or jumps back before the page top. It
  was not reproduced (single page, 115% and 240%, Chromium mock) and no cause was found. A wheel
  over a floating window does not scroll the canvas, which is by design and may be part of it.

**Masked FLUX.2 Klein, local and cloud (2026-10-01).** Klein now redraws only the hint's lettering,
grown 8 px, and holds the rest of the crop (`deploy/cloud/common/flux.py`, sidecar `backend/sdnq.py`
and `backend/mflux.py`), with MangaTranslator's prompt in `flux.rs`. Chosen on one black-and-white
page, 12 regions (docs/research/manga-inpaint-models.md). Not yet run:
- A colour page, under the new prompt or the hole.
- A live gateway deploy of `mc-flux2-klein-inpaint-v1` / `-9b-inpaint-v1`; Beam at all. The helper
  needs `npm run helpers`.
- The sdnq backend's masked render on a real GPU (unit tested only); the mflux one ran on the M5.
- `RenderRate` still prices the edit; the masked render measured about 1.4 times its GPU time.
- Qwen recipe v2 (`mc-qwen-image-edit-2511-v2`, FukidashiErase beside Lightning): no live deploy,
  no colour page, no Beam. It still paints a black balloon white.

**Cloud Klein inpaint returns the lettering; Qwen seed retry (2026-10-02).** On c32 p20 the masked
Klein recipe sent back both sound effects untouched, on 4B and on 9B. The mask was right. diffusers
0.39 `Flux2KleinInpaintPipeline` conditions on `image` itself, and at 4 steps the model redraws the
hole as a copy of that condition. A LaMa pre-fill of the hole, also passed as the reference, came
out clean on 16 of 16 seeds and settings on 4B and on 9B, and 4 of 4 on Qwen; white, blurred and
Telea fills left patches or copied the strokes. Qwen v2 as shipped left both sound effects on seed 1
(the pinned seed) and removed them on seeds 2 to 6. The Qwen recipe is now v3: a render whose
lettering changed less than twice as much as the rest of the crop is rendered again with the next
seed, up to two more times (`deploy/cloud/common/qwen.py`). The same day the aizendazai46 Qwen volume
turned out to lack the FukidashiErase LoRA. Its ready marker predated the LoRA, the provisioner
counted a marker by name only, and the worker refused the snapshot. The provisioner now reads the
marker and the seed status's byte total, so a recipe that gains a file seeds again. Not yet done:
- The Klein fix (LaMa pre-fill on the client, before upload) is not built; cloud Klein Clean still
  returns the lettering.
- One page, black and white. No colour page, no Beam, no live v3 deploy at the time of writing.
- `MIN_EDIT_RATIO` (2.0) rests on six renders of one crop; low-contrast lettering may need another
  threshold. A retried render costs up to three times the GPU time and `RenderRate` does not price it.

**Interface languages (2026-10-01).** The interface speaks English, Korean, Japanese, Spanish,
Brazilian Portuguese and French (`src/lib/i18n/`). A first run starts in the OS language when it
is one of these, the Welcome screen and Settings > General offer the choice, and it is a
frontend preference (`session.language`). The five translations were machine-written in one pass
and checked only for keys, placeholders and dashes (`locales.test.js`). Not yet done:
- No native speaker has read any of them. Terms chosen without a reviewer include the lettering
  mask, the script gate, Text-shaped review and the cloud setup wording.
- The tray menu and the macOS app menu (`src-tauri/src/lib.rs`: Show and Quit Manga Cleaner) are
  English in every language; Rust does not read the preference.
- Only the Welcome screen was looked at in each language. French, Spanish and Portuguese run
  longer than English, so tight rows (tool bar, Pages list, footers) may wrap or clip.

**Background jobs and GPU sharing (2026-10-01).** Detect, Clean, cloud Clean, local denoise and
cloud denoise now run as jobs: a progress dialog can close, the Jobs button on Home and in the
editor lists them, and up to 3 runs go at once on different chapters (`run::MAX_RUNS`, one per
chapter). Cloud denoise keeps 2 pages in flight with progress and Stop. With `closeToTray` off, a
close, the tray Quit or Cmd+Q asks first while jobs run (`jobs.rs`, `QuitJobsDialog`). On the cloud,
the FLUX 4B worker loads 2 pipeline copies on an L4 and takes 2 renders at once, and `AnalysisGPU`
takes 3 inputs (`deploy/cloud/common/capacity.py`). FLUX 9B and Qwen hold 2 copies only on an
L40S (Modal), by decision: Qwen's two are about 43 of 45 GiB and the 9B's peak is not measured; an
extra copy that fails to load leaves one, and an out-of-memory render is retried alone. Unit tested only; not yet run:
- No live Modal or Beam deploy of the concurrency change. The render gain is a guess (10 to 30
  per cent; both copies share one CUDA stream), the large-crop threshold (1 MP runs alone) is a
  guess, and Beam's 2 workers per queue are untested. Measure: the same 40 real crops with 1 then
  2 copies, `nvidia-smi` sampled, about 25 to 30 L4 minutes. The helper needs `npm run helpers`.
- The quit question and the macOS Cmd+Q menu swap are not seen in the real app. The Dock menu's
  Quit is not guarded. `confirm_quit` waits 5 s at most, then ends in-flight cloud pages.
- Detect-then-clean chaining is still editor-scoped: leaving the editor mid-detect keeps detection
  running but does not start the clean step.
- Denoise runs do not count toward `MAX_RUNS` and have no per-chapter lock. Finished jobs do not
  survive a window reload.
- Fixed, unit tested only: Modal answers a web request past 150 s with a 303 and `http.rs`
  refuses redirects, so a denoise page or detection tile that queued or ran that long failed as
  `gateway_unreachable` while the GPU could still finish and bill it. Pages and tiles are now
  submitted and polled (`POST /mc/{denoise,analysis}/v1/jobs`, `GET .../jobs/{handle}`,
  `POST .../jobs/{handle}/cancel`; `cleaner_core::cloud_job_wire`, `inference::gpu_jobs`,
  `modal/backend.py` `ModalGpuJobs`). Every request is bounded at 90 s or less; a page may take
  1500 s and a tile 900 s before it is cancelled. The handle hashes the request digest and an
  attempt id, so a submit repeated after a lost answer spawns nothing. A Stop now cancels the
  pages and tiles in flight on the gateway. A gateway that does not list `denoise.jobs` or
  `analysis.jobs` in `/mc/v1/capabilities` (deployed before this, and Beam) still gets the
  synchronous routes and still fails past 150 s; those routes now wait 120 s, then cancel the
  call and answer 504. The gateway timeout went from 660 s to 300 s. Not measured live: a
  redeploy, a cold-start page past 150 s collected by polling, the cancel of a running page (Modal
  `FunctionCall.cancel` without terminating the container), whether a remote `DenoiseError` or
  `AnalysisUnavailable` keeps its type through `FunctionCall.get` in the gateway, and how long
  Modal keeps an uncollected output. A job whose app stopped polling stays counted for up to
  30 min and holds the analysis idle release back; the container still scales down on its own.

**Denoise tinted grey pages pink (2026-10-01).** Deli Health chapter 11 was denoised from its
cleaned pages with `realcugan-3x-conservative` on the cloud. Its raws are RGBA with three equal
channels. The 16 pages with a LaMa patch came back as RGB with a pink cast (R about 5 to 9 levels
over G in the screentone midtones, across the whole page, not only in the patches); the 6 pages
with only flat fills came back grey. Cause: LaMa leaves up to 18 levels of chroma in its fills on a
grey page (54 patches, median 11), both denoise engines call a page grey only when every pixel is,
so one fill sent the page down Real-CUGAN's colour path, which tints grey. Fixed on the desktop
side: `cloud_denoise::cleaned_composite`, the input of local and cloud denoise, now folds the
composite to grey when the page's source is neutral. Unit tested; not yet re-run on that chapter,
whose existing `denoised-cleaned` files keep the tint until it is denoised again. The LaMa chroma
itself is fixed too: `engines::model::answer_mode` reads a model's answer as grey on a page that is
grey but stored as RGB or RGBA (`Raster::is_neutral`), for LaMa and for cloud renders
(`PreparedRender`). With the real model on Deli Health chapter 11 page 5, a LaMa patch went from
13 levels of chroma to 0. Not changed: patches already drawn keep their chroma until they are
cleaned again, and a page whose underlay still holds such a patch no longer counts as grey, so
new patches on it are read in colour until the old one is redone.

**Denoise history and compare (2026-10-01).** A chapter's denoise rows are now kept per run (newest
10 runs, with preset, local or cloud, raw or cleaned input, and the source a taken page had), the
chapter list shows a Denoised chip with a dropdown of runs, and a compare dialog wipes a run's
denoised page against the raw one (`src-tauri/src/denoise_history.rs`,
`src/lib/dialogs/DenoiseCompareDialog.svelte`). Unit, DOM and Vite-mock checked. Not tried in the
built app: the binary IPC answer of `denoise_compare_image`, and its speed and memory on large
pages. Rows written before this change have no preset or target and show as unknown.
History now lives in the chapter menu ("Denoise history"), the compare dialog has a full-window
layout (CSS, not the Fullscreen API; not yet tried in WKWebView), and "Replace pages with
denoised" is offered when at least one page is ready: pages with no denoised file stay raw and
are counted. First real cloud run after the grey fix (Deli Health chapter 11 as c49, 22 pages,
`realcugan-3x-conservative`): pages 5 and 6 failed because Modal preempted the GPU container
mid-run (the app log says "Container terminated due to preemption"; two 500s, then the other 16
pages ran). A failed page is not retried, by the consent rule that each page is sent once; whether
to add an automatic retry on preemption or a "Retry failed pages" action is open. Not checked:
the journal recorded those two as `unknown_remote_state` rather than `Failed`, so the 500 did not
reach `rejection_code` as `UnexpectedStatus { status: 500 }`.

**The colour picker's eyedropper in the macOS app (2026-10-01).** The picker's "pick a colour
from the screen" button now works in the macOS app through AppKit's `NSColorSampler`
(`src-tauri/src/screen_color.rs`, called from `src/lib/api/screencolor.js`), because `WKWebView`
has no `EyeDropper`. It compiles and a DOM test covers the call, but nobody has yet clicked it in
a built app, so the sRGB conversion and the popover staying open while the loupe is up are
unchecked. Linux (WebKitGTK) still has no eyedropper; the desktop portal's `PickColor` call is the
route there if it is wanted.

**Leftover specks between the letters of a fill (2026-09-30).** `fit::with_stray_lettering` now
adds small ink islands the lettering mask missed (furigana, dakuten dots, stroke tips) to the seed
on flat paper, so the fit grows past them instead of falling back to a one-pixel mask. On
Uncensored Dungeon Streamer chapters 1 and 2, 7 of 16 fallback regions kept specks against 3 of
296 grown ones. Not yet done: the fix has not been re-run on those chapters, and stored detections
keep their old thin masks until they are detected again. `quality::assess` is still blind to ink
left inside a mask's bounding box but outside the mask, so a flat fill passes there whatever it
leaves; scoring those pixels would let a bad fill escalate to LaMa as the pick design intends.

**The app on a real Windows or Linux desktop (2026-09-29).** A read-only code audit of commit
`91edc3b` is in `docs/windows-linux-audit-2026-09-29.md`: 4 blockers, about 20 major and 40
minor findings, each with `path:line`. A launch check (`src-tauri/src/smoke.rs`) now opens the
installed app on every Windows and Linux release build and loads the ONNX Runtime; it passed
once locally in an Ubuntu 22.04 arm64 container (WebKitGTK 2.50.4, tile fetch answered), and has
not yet run in CI. No model has run on Windows or Linux, and nothing has run on a real desktop,
a GPU, or a clean Windows image. That document's "Needs a real machine" list is the open
checklist, first of all the NVIDIA blank window and the AppImage on Ubuntu 24.04.

**Cloud setup on a new computer (2026-09-29).** The Modal SDK (1.5.5) is frozen into the
`manga-cleaner-provisioner` sidecar on all three targets, so onboarding needs no Python, Modal CLI
or download before the key screen, and the key is copied from modal.com. Checked: the installed
macOS helper under `env -i` signs in to the real Modal API and is refused with "Token not found";
an x86_64 Linux helper frozen by `build-cloud-provisioner.py` in a Debian 12 container does the
same in fresh Debian 12 and Ubuntu 24.04 containers with no Python, no Modal and no system CA
bundle. `--online` adds that sign-in to validation runs of the release workflow, on each OS. Not
done:

- **Windows has not signed in to Modal from the frozen helper.** Only the installed self-check
  with `PATH` reduced has run there (run 36219174240). The next `validate_only` run does it.
- **The Linux helper was frozen on Debian 12 (glibc 2.36), not the Ubuntu 22.04 runner.**
- **The one-file helper unpacks into the temp directory on each run.** A temp directory mounted
  `noexec`, or antivirus quarantining the unpacked files on Windows, is untested.

**Several Modal accounts (2026-09-29).** Profiles, runtime tokens, consents, grants and journals
were already keyed by profile id, so a second setup in another account was already saved beside
the first. What changed is Settings > Cloud: each Modal row names its account, read from the
endpoint host (`endpointAccount`, the part before `--`), rows can be renamed, and a note over two
or more rows says the default is the switch and no key is asked. Not done:

- **No switch outside Settings.** The Text cleanup panel's Cloud GPU choice does not say or change
  which account runs.
- **A new setup is still named `Modal (<id>)`.** The helper's apply answer carries no workspace
  name, so the account shows on the row's second line, not in the name, until renamed.
- **Switching or renaming during a cloud run stops it.** Every `inference.json` write bumps every
  profile's epoch (`write_inference_config_at`), so a batch in flight fails closed with
  `cloud_clean_profile_changed`. A rename could skip the bump; the writer was left as it is.
- **Not driven with two real accounts.** Evidence is DOM and mock tests and the Vite mock. On a
  local ad-hoc build each account's two keychain items ask once for Always Allow.

**Clean with on detected and declined regions (2026-09-29).** A detected region's menu offered
Clean and Clean on cloud GPU. Clean on cloud GPU went through the run's batch plan
(`prepareCloudClean` over a region scope), which keeps every LaMa pick on this computer, and text
outside a bubble is saved with a LaMa pick by default: the "cloud" clean ran LaMa here under the
quality metric and sent nothing, so a region the metric declined was declined again on every try.
Both entries are replaced by Clean with, the list a cleaned region shows: the local models this
machine has, and Cloud while a cloud endpoint is ready. A local model runs as named (`applyTool`
with `engine`, `Choice::Exact`) and is not scored against. Cloud asks consent for this one region
and renders it on the endpoint whatever its pick (`cleanDetectedOnCloud`, which the Layers row's
Clean on cloud GPU now uses too; the region-scoped batch helper is gone). A region the metric
declines in a run now keeps the lettering its fit started from on its untouched row, stored as a
held candidate's (`Job::decline_detection`, `RegionOutcome::Declined`), and its menu offers Clean
with. A model picked there, or for a gate-skipped or held row, runs as named (`cleanAnyway` with
`params.exact`). Before, a declined row offered only Show on page and Delete, and a model picked
for a held row was only a start the same metric could refuse again.

- **Decided 2026-10-01: a Text cleanup run with Clean on set to Cloud GPU sends its LaMa picks.**
  Before, they were cleaned here by LaMa ahead of the first batch, and text outside a bubble is
  picked LaMa by default, so a cloud clean ran LaMa on this computer first. The user ruled that the
  cloud means the cloud: those regions now go to the cloud GPU and count in the cost range. Only the
  mixed choice still cleans anything here, and only Fill and Solid colour picks.
- **A declined row cannot go to the cloud directly.** Cloud consent binds to a stored patch or
  detection, and a declined row is neither. It can be cleaned here with a picked model, and the
  layer that makes offers Clean with > Cloud.
- **A declined row keeps no inside or outside answer,** because `inside_bubble` on an untouched
  row is how the counters tell a held candidate. Clean with names the model, so none is needed.
- **Rows declined before this change keep their box alone** and are cleaned from what the
  detector finds under the box.
- **Not run in the shipped app.** Unit tests cover each path, and the browser mock showed the new
  menu and a LaMa pick cleaning a detected region. No real cloud render of a detected region was
  made, and a picked model on a declined region was not tried on real scans.

**One in/out answer for text (2026-09-29).** A run asked whether text sits in a bubble three
ways: the engine pick and the stored detection read the Ogkalu boxes alone, the gate combined them
with the paper walk (and under the All text policy never ran, so the paper was never read), and a
held candidate used the grouping's balloon confinement. All of them now read one function,
`balloon::in_bubble`: inside when Ogkalu says inside, or when the paper walk reads a closed band of
fill around the lettering. Paper only rescues; it no longer vetoes an Ogkalu inside
(`Interior::settles` is gone). The walk is stricter: a band counts only when a thin outline closes
it at least 3 px out, or when nothing ends it across the whole walk, and the reading of the paper
between the strokes (`inner_paper`) is removed. Both leaks this closes were sound effects drawn with
a white outline over art: the outline passed as a band as soon as it was deep enough, and between
the strokes it passed as paper. On the stored evidence of three real chapters (c38, c42, c43; 1,106
clean groups) the paper rescues 122 groups Ogkalu put outside: the same 121 the strict walk rescued
when it was first measured, plus one group whose stored lettering lies wholly below its page, which
is an artifact of reading that evidence against a single page. The 3 groups the paper used to veto
(a Bubble grade over paper that reads as picture) are now inside. On the 28 noisy scans
(`~/dev/120 noisy png`, CTD and Ogkalu small, `spike-gate-probe`) 374 regions: no Ogkalu-inside
region became outside, 1 went from outside to inside (16.png at 837,125, short text beside a
Bubble@0.92 shape over tone, which the paper used to veto), and 28 went from inside to outside, all
of them regions Ogkalu put outside and the old reading rescued (26 through the between-the-strokes
reading, 2 through the early band). By eye, 11 of those 28 are sound effects or stray marks, 4 are
unboxed lines over art, and 13 are real text boxes: 11 frosted interface-style text windows with a
faint pattern behind the lettering (18, 19, 23, 25 and 26.png) and 2 narration blocks on white
paper that runs into tone (12.png and 25.png). Under the default Review choice those 13 are now held
as text outside a speech bubble, where before they were gated and cleaned.

- **A sound effect over sparse vertical speed lines still reads as flat paper.** The lines are too
  sparse to break a ring, so the walk finds an unbroken band and the effect is rescued inside.
- **The 13 boxed-text regions the strict walk no longer rescues on the noisy scans** (frosted text
  windows, narration beside tone) have no rule that separates them from outlined effects. They are
  held for review by default and cleaned only under the outside opt-in or All text.
- **A Bubble grade is never vetoed now,** so short unboxed text whose centre sits in a nearby
  balloon's box (16.png at 837,125) is inside. Only that one crop was looked at.
- **Stored detections keep the inside they were saved with.** A Clean from rows detected before this
  change starts from the old answer until the page is detected again; nothing recomputes it.
- **Not run end to end.** No Detect, Clean, one-pass, cloud or All text run of the app was made on
  real pages with the new answer. The run paths are covered by unit tests on synthetic pages
  (`an_all_text_run_still_gets_the_real_inside_answer`,
  `a_held_candidate_is_inside_by_the_same_answer_a_region_gets`), and the real-page evidence is
  the gate probe and the stored-evidence probe only. How many held candidates change on real pages
  was not counted.
- **The stored-evidence probe read around each group's raw bounds,** while a run reads around the
  region's tight box (2 px wider, 3 on the right). The effect of those pixels on the counts above
  was not measured.

**Flat Fill, and Denoise fill removed (2026-09-29).** Denoise fill (rung 1) is gone, because pages
are now denoised whole before they are cleaned. Fill (rung 0) paints one flat colour through the
mask and nowhere else: the per-channel median of the 4 px paper ring just outside the mask
(`fit::Fitted::paper`), with no plane (`crates/cleaner-core/src/engines/fill.rs`). Saved projects
still load: a `denoise` patch engine, detection pick, run pick or ceiling reads as `fill`, and a
stored `fill_and_denoise` route reads as `fill`. `EDIT_MARGIN` drops from 6 px to the isolation
radius, 5 px, since no rung writes wider now. In a Mixed cloud clean, a region saved with the
Denoise fill pick used to go straight to the cloud; it is now tried here first as a fill. Unit
tests cover all of this. Not measured: the flat fill on the real scans in `~/dev/120 noisy png`;
and routing, which still measures the ring's deviation about a fitted plane, so paper with a mild
gradient can still route to Fill and now gets one flat colour where it used to get the gradient.
Measuring the deviation about the flat median instead would send those regions to LaMa, but it also
changes how far the fit grows every mask, so it was left for a separate change.

**Patch layers instead of flattened cleaned tiles (2026-09-29).** Every edit used to move the URL
of every `cleaned` tile on the page (and on longstrip neighbours), twice: once when the edited
region came back and once when the page reloaded. WebKit blanks an `<img>` whose `src` changes
until the new bytes decode, so the source under it showed through and every cleaned spot flashed
its lettering. The editor now draws `source` tiles and one canvas per patch over them
(`editor/PatchLayers.svelte`), each served by the `layer` route in `src-tauri/src/tile.rs` and
versioned by `tile::layer_appearance`, which leaves out opacity; a canvas keeps its old pixels
until the new image is decoded. Measured on a copy of the release-validation chapter c32 (25
RGBA pages at 1136x1601, 223 cloud patches, with every 5th set to 55% opacity, every 7th turned
12.5 degrees and every 11th moved): drawing the layers over the source proxy tile in compositing
order gives the flattened `cleaned` tile exactly on all 451,553 fully covered pixels. On the
132,330 pixels at a patch's anti-aliased rim or under a faded patch, 93% are exact, 5.9% are one
level off, and 368 are seven or more off (worst 52). Those are the one-proxy-pixel ring where a
patch edge lies over lettering: the flattened tile averages the patch with the page beside it,
and the layer is averaged with transparency and then drawn over the page's own average, which
still holds the ink under the patch. Serving each of the 223 layers took 122 ms in all at
opt-level 2, against 3.2 s to flatten the first tile of the 25 pages. Export is unchanged and
still composites through `cleaner_core::composite`. Not measured: the flicker in the shipped
WKWebView app (no desktop control); pages whose alpha is actually partly transparent, indexed
pages at partial opacity (export snaps to the palette, the screen does not), and 16-bit pages,
where the screen can differ by the same rim effect or by rounding; a longstrip on real data (unit
tests only); and scrolling cost, now that every mounted page and one neighbour either side keep
their regions resident so their layers are drawn before they scroll into view.

**Try again wider (2026-09-29).** A second re-run kind, `retryWider`, sits beside Try again on local
mask rows and in the region menu. It hands the model the stored mask as its hole
(`Geometry::StoredWider` in `src-tauri/src/region.rs`), which is what every re-run did before the
hole was pinned to the stored lettering: each press is `ISOLATION_RADIUS` (5 px) wider than the
last, and plain Try again afterwards repeats the last wider hole. Unit tests cover the growth on a
page and on a long strip. Not measured: the quality of a real LaMa run in the shipped app; a cap on
the growth (none exists, so enough presses reach a balloon outline or the art, and nothing stops
the hole at a strong edge); cloud renders and text-shaped patches, where it is not offered and the
native side refuses it; and the mock, which runs it as plain Try again without growing the mask.

**Cloud page denoise (2026-09-28).** A whole-page denoise backend now exists on the cloud path
only: `deploy/cloud/common/denoise.py` (waifu2x ONNX, OpenModelDB `.pth` through spandrel),
`/mc/denoise/v1/{capabilities,page}` on the gateway, run on the analysis GPU role, and the desktop
client with a prepare, confirm and start grant flow (`src-tauri/src/inference/cloud_denoise.rs`)
that writes denoised copies of the cleaned pages to a folder. One chapter (46 pages, 12 variants)
ran on an L4 through `spikes/denoise/modal_compare.py`: 0.7 s per page for cunet grain, about 5 s
for swin `art_scan` grain, 4.4 s for MangaJPEG, 6 s for a 2x sharpen, 15 s for the 4x digimanga
model. Not yet measured or built: which variants to ship as presets (waiting on the user's visual
review), the cost estimate rates in `cloud_denoise.rs` (guesses, not fitted to those timings), a
deployed-gateway run (only the ephemeral spike app ran), the UI,
onboarding, and in-app storage of denoised pages. Uploads are the `cleaned` tile rendering,
normalised to 8-bit grey or RGB, so 16-bit, palette and CMYK pages come back as 8-bit.
A second round on five pages added MangaJaNai 2x and 4x (the model is picked by page height,
as MangaJaNaiConverterGui does; about 10 to 15 s per page), Real-CUGAN 2x and native 3x (2.5 to
4 s, but it smears screentone into blotches), and waifu2x swin `art` 2x. Not tried: 2x-MangaScaleV3
and 2x-Manga-Ora (only on Mega and Proton Drive, no scriptable pinned URL). The desktop recipe
validation in `cloud_denoise_wire.rs` now matches `resolve_step` for all engines and carries the six
presets. Local denoise runs one preset, `waifu2x-scan-4x-n2` (`cleaner_core::page_denoise`, commands
in `src-tauri/src/page_denoise.rs`): on three crops of a real scan (one tinted to colour) it is within 1/255 of
`denoise.py`'s own CPU output (max 1/255, mean under 0.0001/255, on CPU and WebGPU). One 1284x1809
page on the M5 takes 31 to 33 s on WebGPU (the swin graph is only partly placed there, the rest runs
on the CPU), 48 s on the CPU with the 1.28.0 release runtime and 76 s with the downloaded one; CoreML
builds only partitioned and then fails its first run, so it is refused. Not measured: peak memory of
a local run, Windows and Linux providers, the other local presets (their models are PyTorch only),
and holding the run slot while one is going. A local chapter run now reports progress per tile
(`denoise://progress`) and stops at the next tile on `cancel_denoise_local`, keeping the pages
already saved; checked by unit tests and the dialog's DOM test, not yet in the shipped app. A cloud
run still has no progress or stop: `start_cloud_denoise` answers only at the end. Where denoise runs
now asks the selected deployment (`cloud_denoise_presets`, no GPU started) and offers Cloud only when
it lists page denoise, and only the presets it lists; not yet tried against the deployed gateway.
Plan: `docs/denoise-engine-plan.md`.

**Updating a cloud setup (2026-09-28).** A finished Modal setup could not get page denoise: Apply
refuses a finished installation and Resume refused any change of options, and onboarding offered no
setup at all once cloud use was on. Resume now takes new options with the hash of a plan made for
them (`controller.py#_handle_resume`, `journal.py#replan`): the journal records the new plan, the
weights step runs again and the last seed's state is forgotten, so the denoise models download into
the same volume. Settings > Cloud has Update on every setup-made Modal endpoint, and Update has Change
options, which plans the recorded choices on the Review screen and resumes with the approved hash.
The gateway now reports `code_digest` (`deploy/cloud/common/release.py`, the shipped `common/` and
`modal/` sources) in `/capabilities`; `build.rs` hashes the same files (`cloud_code_digest.rs`) and
Settings says when a setup runs other code. Checked by the helper's fake-SDK tests, the cloud and Rust
digest tests on one golden tree, and the dialogs' DOM tests. Not measured: an update against the real
account, the seed's time to verify the models already on the volume, and Beam, whose Resume does not
redeploy and so gets no code update.

**Update without pasting the Modal token (2026-09-30).** Every Update asked for the Modal token
again, because setup always dropped it and the proxy token the app keeps cannot redeploy. Setup now
offers "Remember this Modal token" (on by default), and the desktop keeps it in the keychain under
the profile's setup role; Resume, Update and Clean up of that setup then send the profile id and the
desktop fills in the key, which never reaches the webview. The "older cloud code" notice on
`mc-d0dhwx` was checked the same day and was true: its last deploy (2026-09-29 16:46 UTC) predates
edits to `deploy/cloud/modal/app.py` and `deploy/cloud/common/manifest.py`, and the installed app and
its helper both carry digest `31294819…`. Two ways it could have been false are closed: a development
build that runs the live Python now compares with the checkout's digest at run time, and
`npm run helpers:install` refuses an app built from other cloud code.

**Denoise before cloud in setup (2026-09-28).** Setup asked where page denoise runs after the cloud
step, so a cloud GPU set up in that run never got page denoise, and the onboarding Update hid it
behind Change options. The Denoise step now comes before Cloud and always offers the cloud GPU; the
choice is a wish the cloud step carries out. `CloudProvisioner`'s `wantDenoise` ticks page denoise
on every plan it makes (new setup, reused installation, update), and on Update its only way on is
Continue to that plan, so the update cannot skip it. When cloud is on but its setup lacks page
denoise, the cloud step says so and makes Update its primary button. Checked by the onboarding and
provisioner DOM tests. Not handled: choosing the cloud GPU for denoise and then Not now on the cloud
step leaves the target on Cloud with nothing to run it, and nothing local is downloaded; Settings >
Denoise then refuses Cloud and the person picks again there.

**Replace pages with denoised (2026-09-28).** Every local or cloud denoise now records each file it
wrote in the manifest (`denoised` rows: source, path, and the page appearance it was read at). When
every page of a chapter has a row whose file still exists, the chapter menu offers **Replace pages
with denoised**. It copies each file into `<job>.mtclean.d/pages/denoised/<digest>/<page name>.png`
and points the source row at it (`Job::replace_source`). Detections, their masks and lettering,
`examined` and `errored` stay. Only pages denoised from their bare source and not cleaned since are
replaced: a page with any patch (shown or hidden), text-shape plan, revision or correction, or whose
appearance moved after denoise, is kept, because a file made from a cleaned page already has the
cleaning in it and every patch would be drawn twice. One unusable file replaces nothing. Local
denoise now refuses a page that changed during the run (`denoise_page_changed`), as cloud already
did. Checked by core and backend unit tests, the mock, and a mock run in the browser. Not done: no
run in the real app; no way back in the interface (the old page files stay in `pages/`, unused); a
refused batch leaves its copied files in the sidecar with nothing naming them; a colour page
replaced from a cloud denoise loses its ICC profile, since the upload drops it.

**Beam paused (2026-09-28).** Beam connections are off in this version. The native side refuses
them in three places: `provider_paused` in `inference/mod.rs` stops `build_client_for_profile`
before the keychain read, `CloudHttpClient::new` returns `ProviderPaused`, and `run_provisioner`
refuses every Beam setup, resume and cleanup. The UI marks Beam Paused: the provider radio in cloud
setup and in the manual endpoint form is disabled, a saved Beam endpoint shows a Paused badge and
cannot be the default or be tested. Saved Beam profiles stay in the config and keychain untouched.
Checked by the Rust client and transport tests and the dialogs' DOM tests. Not tested: the
`run_provisioner` refusal itself (it needs an app handle the tests do not build), and a Beam profile
that was already the default, which now fails its jobs with `provider_paused` instead of falling
back to local.

**Smooth solid shapes and turned layers (2026-09-28).** A solid Shapes fill is now rasterised from
an exact signed distance (`region.rs#shape_coverage`): anti-aliased edges, an inner outline exactly
`outlineWidth` page pixels wide, and `feather` as a Gaussian soft edge (sigma = feather / 2) rather
than growth. Patch masks may carry partial coverage (0 to 255), which the compositor reads as a
per-pixel opacity and the PSD layer mask keeps. A moved or turned layer is resampled bilinearly,
weighted by that coverage (`Patch::presented`), instead of nearest-neighbour. The draft preview
draws the same geometry in page pixels, so its widths follow the zoom. Checked: core and backend
unit tests (coverage, exact ellipse distance, even-odd lasso, smooth turn, exact move) and the
preview in the Vite mock. Not measured: the committed pixels and a turn in the shipped app, 16-bit
and RGBA pages by eye, and render time for a page-sized feathered lasso. Engine-mode shapes still
use the binary `shape_mask`, where feather means growth.

**Detection masks on the canvas and the Selection tool (2026-09-28).** Each detected region now
draws as its display set (`mask ∪ ink`, what fill and the model rungs may erase) instead of a box,
and tool `6` adds to or removes from it (`edit_detection_mask`, docs/detect-clean.md §3). Checked:
the native edit and the mask image on a copy of the user's chapter c38 (59 PNG pages, 720 x 2500)
through a throwaway test, then drawn by the real `DetectionMasks` component over the real pages in
the Vite dev server with the tile protocol stood in by a local server (a brush add joined the
watermark cat to the TOON region, a rectangle cut a sound effect, a lasso on bare art made a new
region); the tool, previews and settings in the mock. The first shipped build still drew boxes:
the app's content policy let `<img>` reach `tile:` but not `fetch` (`connect-src`), so every mask
load failed and fell back to its box. Neither check above runs under that policy; a test in
`tile.rs` now pins both directives. A review then found the drawn set was not what every rung
erased: rung 1 writes 6 px past the mask and the model rungs 5 px past the lettering, and on
chapters c42 and c38 17% to 22% of LaMa's write lay outside the drawn set. A patch that replaces a
detection is now cut to its display set in `Job::complete_detection`, the one commit every local,
region and cloud clean of a detection goes through (core test on a patch wider than both files).
The selection lasso also spaced its vertices by the hidden brush size (tens of pixels at 160),
and each edit re-fetched every mask on screen; both fixed, with DOM tests. Not measured: the
shipped WKWebView loading
the mask with `fetch` and `createImageBitmap` over `tile://` (the Vite check was Chromium over
http), a Clean or cloud clean of an edited detection, how the hard edge looks where rung 1's
seam blend or a model's isolation ramp is now cut at the display set, and a Windows build. Not built: undo for a
mask edit (a detection has no pixels to restore, as for deleting one), a gesture that crosses a
longstrip join (it is clipped to the page it starts on), and more padding than Detect stores (the
stored mask already covers the lettering, its outline and the fit's halo; Add widens it by hand).

**Cloud cost panel removed (2026-09-28).** The floating cost panel, the Modal billing connection
and the app's own usage totals are gone. Modal has no call for the credits left on an account.
The Python SDK 1.5.5 (the newest on PyPI that day) offers `Workspace.billing.summary()`, whose
`adjustments.credits` is the credit applied in the current cycle, not the balance: on the test
workspace `modal billing summary --json` gave `metered_cost` 0.62, `billed_cost` 0 and `credits`
-0.62. The protocol also has an `EnvironmentGetBudget` call with a spend limit, which the SDK does
not expose. Setup no longer keeps the Modal API token in the keychain, since only billing read it.
A resume onto a new endpoint origin and removing an endpoint still delete a token an earlier
build kept. Evidence is unit, DOM and mock tests; the change was not driven in the real app.

**Long-strip join check (2026-09-28).** `strip::join::check_join` called clean cuts duplicated or
misregistered, so detection windows stopped at them and lettering across them was cleaned in
halves. A repeat now needs at least two rows, and must match twice as well as the straight join
and as its own neighbouring rows, which must themselves differ by more than the row tolerance. A
shift must match the join twice as well as the same shift matches the rows next to the join inside
each page. Measured with a throwaway probe (16 probe rows, as the app reads) on two chapters: the
"Oil" chapter (59 PNG pages, 720 x 2500) went from 5 flagged joins to 0 (31/32, 39/40 and 49/50
repeats; 29/30 and 44/45 shifts), and chapter 109 (45 JPEG pages) from 2 to 0 (08/09 and 14/15
shifts). All seven were checked by eye as clean cuts: the repeats were smooth or near-blank rows,
and every shift was slanted line art that also lines up at that shift inside the page (it matched
the join 1.0 to 1.5 times worse than the page's own rows). The fixture's 8-row repeat is still
found. The requested shift rule (abstain when the straight join is within the inside-page row
difference) was not used: the existing misregistration test uses random rows, where that rule
abstains. Not measured: a real repeated or mis-cut join, since neither chapter has one, so finding
them rests on synthetic tests (textured rows with 1-level noise, and the fixture); a real repeat
of art that barely changes down the page now verifies and is not reported; other chapters were
not probed. Pages already detected keep their split lettering until they are detected again.

**Editor scroll fix (2026-09-28).** Code tracing and DOM regressions reproduce the
per-scroll bounding-box reads, distant-jump over-mounting and duplicate page loads.
Real WKWebView momentum scrolling, pinch/zoom paint timing, mixed-width strip
alignment and native `tile://` decode/compositing cost still need measurement.
The dev mock and jsdom do not exercise native image delivery or Safari rendering;
the installed app was not driven during this work.

**Tile delivery and preloading (2026-09-28).** The `tile://` handler was registered synchronously,
and WKWebView calls a scheme handler on the main thread, so every tile read the library index,
parsed the manifest, decoded the whole page and PNG-encoded the result on the thread that delivers
scroll and pinch events. It now runs on the blocking executor, cuts all of a page's tiles from one
decode, keeps them in a 256 MiB cache keyed by the appearance digest (source hash for `source`),
decodes at most three pages at once, and encodes tiles at the PNG encoder's fast setting.
Measured in a release build on `120 noisy png/01.png` (1080 x 1536 RGBA, one tile): decode 14 ms,
proxy 38 ms, encode 128 ms balanced against 7 ms fast (1.46 MB against 3.47 MB). The same page
stacked 13 times (1080 x 19968, ten tiles): 3,263 ms per-tile decode and balanced encode against
661 ms for one decode and fast encode. Tiles were `loading="lazy"`, which WebKit starts only
inside the scroller's visible box, so no page loaded before it was on screen. The strip now mounts
pages and tiles one viewport past both ends, and a paginated chapter fetches the pages either side
of the current one. Ctrl or Cmd wheel zoom applies once per animation frame from an unrounded
gesture scale (a slow pinch at 40% had rounded back to 40% on every step). Paint canvases no
longer follow every resize while idle; each mounted page had a pair sized to the sheet times the
pixel ratio, reallocated on every zoom step while a drawing tool was up. Not measured: scrolling
and zooming in the installed app (verified by unit, DOM and Vite mock tests only), whether WebKit
keeps custom-scheme responses in its memory cache, the tile cache's peak memory on a real chapter,
and trackpad pinch in WKWebView, which may arrive as Safari `gesturechange` events that the canvas
does not handle.

**Mask-decided text groups (grouping version 5, 2026-09-28).** The lettering mask now decides
what is cleaned, and the Ogkalu boxes only say inside or outside a balloon: lettering no box
claimed is a cleaning group (its record keeps `unassignedComponent`; it raises no review flag),
where it used to be a held candidate. Groups whose lettering is within one larger glyph of each
other (at most 96 px) in the same balloon, or both in none, are one job, so an effect the boxes cut
in pieces is one crop. The reach was tuned on the five hand-marked demo pages (fresh SAM-TS-L
masks, Ogkalu Full boxes): at 1.0 glyph all 25 marked text rectangles are hit, one text box still
spans two jobs (a 1,059 px line on page 01 that 0.6 also left split), and no job joins two marked
boxes; 0.6 split one more box. The 96 px bound was set by eye on chapter 109: 48 px left the
effect "치지지직" in pieces and 128 px joined two effects on the same panel. Replayed on the saved
chapter 109 SAM-TS-L masks and Ogkalu small boxes (`text_groups::group` only, no gate, no fit, no
clean): per page, jobs went from 37 to 128 and cleaned lettering from 216,644 to 1,124,654 pixels
(all but specks); held candidates went from 106 to 1. Over page-plus-900-row windows there are 205
jobs and 6 cross a join. Measured art risk: on demo page 21 the mask marks the clock's digits and
hands (1,851 px) that the protected-art rectangle covers; with no box on them they now form a
cleaning group outside any balloon, so they are held under the default outside choice (Review) and
erased under Clean or the all-text policy. How often SAM marks art elsewhere is not measured (the
fusion report's page 35 suspects are among the new lettering). A reach join stops at 900 px on a
side, but a text box or an unboxed layout chain taller than the 900 px long-strip detection overlap
was already possible, and a segment that owns one clipped at its crop still cleans only the
clipped part; unboxed chains now reach that path as regions rather than candidates. No clean was
run on the new groups.

**Outlined effects outside balloons (2026-09-28).** A headless run of the app's own run code
(Detect, then Clean, outside text set to Clean) with SAM-TS-L and Ogkalu small over chapter 109
pages 18 to 24 as a long strip found 18 regions; 15 were cleaned. The effects there are black
letters with a white outline 4 to 5 px wide, which the mask leaves out, and every one LaMa
cleaned came back as white letters: the art around the letters stops the fit's growth a pixel
or two out, so the hole ended inside the outline and LaMa painted the letters' shape in its
colour. Lettering outside a balloon is now grown by half its stroke width before the fit
(`fit::outlined`; strokes there are 10 to 25 px). On the same pages the white letters are gone;
15 regions are cleaned (one effect that was refused before is now cleaned and one that was
cleaned with white letters is now refused), and 3 are refused by the quality check (`edgeEnergy`
on "악" and on two effects about 300 px wide on page 24). On three of the noisy scans (pages 04
to 06, CTD and Ogkalu small) the 5 outside regions changed as follows: one white-outlined effect
that came back with white blobs is now refused (`histogram`) and left untouched, and the rest
look the same or cleaner. Not measured: other chapters, effects whose outline is wider than half
their stroke, and whether LaMa or a cloud render can clean the refused large effects. Two
effects stay held under every setting except all text: the script check reads the stylized
"쩌적" (page 19) as not Chinese, Japanese or Korean, and gives "슈" (page 18) low confidence;
the same check also holds a face on page 18 that the mask marks as lettering. The default
outside choice (Review) held all 16 outside effects on these pages and cleaned only the one
balloon. SAM-TS-L took about 11 to 13 s per window on the CPU.

**Local SAM-TS-L on tall long-strip windows (measured, left whole).** A local detection window is a
page plus up to 900 rows of the next, and SAM-TS-L reads the whole window at 1024 on its long side
(about 0.41 scale for a 690 x 2500 window), where the cloud reads 1024 tiles at full scale. On
chapter 109 windows 03, 07, 12, 20, 25, 34 and 40 (CPU), the whole read found the same lettering
as the cloud's tile plan read locally and stitched by core; its edges are blockier on small
dialogue, and the few components only one read found were specks (at most 145 px) or art each
scale mislabels differently (whole: 3,419 px of rock on window 25, a page with no text; tiles: a
5,038 px bandage on window 40). Masks agreed at IoU 0.53 to 0.88 on pages with text. Tiling took
27 to 37 s per window against 9 to 12 s whole, so local SAM stays whole. Not measured: how the
blockier edges change a cleaned result, and scans much larger than 1024 on both sides, which the
whole read shrinks further.

Left open by the cloud integration release-fix effort of 2026-09-27, in the order of its plan:

**Qwen-Image-Edit-2511 as a cloud render model (shipped 2026-09-29).** The 4-bit SDNQ base with
the 4-step Lightning LoRA as a PEFT adapter, on an L40S (`deploy/cloud/common/qwen.py`). Measured
on the same 29 real crops (`docs/research/qwen-image-edit-2511-cloud-plan.md`, "Benchmark,
2026-09-29"): as clean as bf16 at 40 steps and cleaner than Klein 9B, at 7.3 s per crop warm.
A live install through the provisioner helper on the `onlybixi` workspace seeded, deployed,
rendered 8 real crops through the gateway with valid digests (first job 75.5 s cold, then about
15 s end to end) and was cleaned up. Not measured: Beam (RTX 5090, no price in the Beam table), an
L4 (peak VRAM 21.5 GiB leaves under 1 GiB spare), the desktop app's own Rust client and UI against
a Qwen endpoint (only its unit tests and the Python wire client ran), a cold start from a volume
that was not just written, and more seeds: the seed changes the result more than the prompt does
(seed 2 was worse on both leading prompts, seed 3 better), and the client pins seed 1 for every
recipe. Sound-effect results still rest on 5 hand-masked regions from two pages. Spend: about
$3.20 of `onlybixi` Modal credit for the benchmark and the live install (metered cost 3.67 before,
6.89 after; billing can lag); the test app, volume, dict and proxy token were deleted. The recipe prompt was
switched on 2026-09-30 to a longer "erase and rebuild the artwork" wording that another person uses
with a masked edit service; that wording is not scored on the 29 crops, so the scores above belong
to the earlier prompt.

**Text grouping (plan section 1).**

- **The detector comparison now has five-page human annotation evidence.** The user marked 25
  text rectangles and one protected-art rectangle on the copied demo pages. Fresh CPU inference
  on the same source pixels found 14/25 text rectangles with CTD, 20/25 with Ogkalu Full and SAM,
  and 20/25 with all three. No cleaning-group bounds touched the protected-art rectangle. These
  are coarse rectangle hits, not pixel accuracy or a measured false-erasure rate. SAM used fresh
  whole-page masks on all five pages with source and graph hashes recorded. CTD took 4.57 s,
  Full 4.62 s and SAM 51.98 s total, excluding model load; median per-page times were 0.86 s,
  0.93 s and 10.51 s. Other local work was active. The saved form completion flags remain false,
  but the user explicitly confirmed marking all pages. The evidence is in the validation folder's
  `annotation/model-evidence-fresh-sam`. Cloud costs, pixel annotations and representative
  false-erasure rates remain open. SAM was not run on the six real noisy scans. The default
  detector was not changed.
- **Grouping now holds lettering that no text box claims.** With a box detector selected, unclaimed
  lettering becomes a held candidate, listed and never cleaned. On the demo pages that holds sound
  effects on pages 04 and 21, and a clock face that was erased before; with the default CTD and
  Ogkalu Small it adds 12 unassigned-mask rows across the 6 real scans and 39 across the 5 demo
  pages. Whether this holds back more text than art needs the annotated set above.
- **Strip-segment ownership is proven by fixtures only.** A region or candidate seen only in the
  overlap before its owning segment is now kept until that owner runs and written once if the owner
  misses it, and a candidate whose lettering a region claims is no longer listed (`settle` and
  `list_candidates` in `run.rs`). The grouping harness measures `text_groups` alone, so no real long
  strip was measured, nor the memory of keeping a segment's crop and segmentation until its owner
  runs. A candidate that crosses a verified join from page p to p+1 is listed on page p, and regions
  found on p+1 cannot retract it. A candidate left with only specks after a region claims its
  lettering is dropped, and two segmentations that split one component differently could leave part
  of it listed.
- **Group ids changed with grouping version 4.** Ids now digest each component's exact pixels, so a
  re-detected page mints new ids and any state keyed by a version 3 id does not carry over. Ink an
  estimate finds outside its own balloon (plus 3 px) with no other box on it is neither cleaned nor
  listed; it measured 0 px on the demo pages and the real scans.
- **Some grouping cases are flagged or listed twice rather than resolved.** An unconfined box
  straddling two balloons stays one group, flagged `crossesBalloon`, not split. A box whose glyphs
  sit in a held candidate gives a `maskMissingUnderBox` row beside the candidate row. Bubble boxes
  are axis-aligned, so where two overlap at a corner a glyph can be held instead of cleaned; this
  was not seen on the real scans.
- **Evidence files are swept only when a chapter opens.** `sweep_evidence_files`
  (`project/mod.rs`) removes evidence that no group or patch names; orphans from a re-detect or
  `remove_detection` stay until the next open. The analysis preview scopes group ids by the source
  sha alone, since it has no page id, so its ids differ from the run's.

**Mask coverage and the white fringe (plan section 2).**

- **Preprocessing 2.0.0 now has 21 live render checks.** The packaged saved-detection Clean
  committed 21 new Modal renders on five demo pages. Offline stage capture on copies reproduced
  21/21 journal crop and hint digests and 21/21 stored patch masks and composites. Across those
  crops, 293,854 RGB pixels changed within applied alpha and none outside it. The same 21 group
  IDs, lettering digests, box IDs and model evidence survived Detect to Clean, and all 21 held
  rows remained unchanged. Fitted detection masks and feathered render supports intentionally
  differ. Ten eligible flat-background crops now measure a median absolute core-to-surround step
  of 0.429 levels before alignment and 0.021 after it, with p90 1.338 to 0.131. Three
  mixed-art rings were excluded. Five additional legacy-region 2.0.0 renders now reproduce all
  journal crops, hints and stored composites (see the texture comparison below). Older raw returns
  had reproduced 227/227 crops and hints, with an
  18-background core step falling from 11.2 to 0.0 levels before the stricter tone gate. The
  128 px context widens crops and uploads; GPU time must be measured rather than inferred.
- **The 2.0.0 recipe improves texture loss, but does not fully fix it.** The old sample had 11 of
  60 screentone returns and 55 of 117 gradient or line-art returns come back pure white. Five
  new Modal renders now compare the same legacy regions, source pixels and detection ink.
  Journal crop/hint and stored composite reproduction pass 5/5. On conservative non-ink hole
  samples, d189 texture rises from 0.12 to 8.33 against source 45.36 (296 pixels), while absolute
  RGB error falls from 19.31 to 13.26. For d106, texture rises from 1.10 to 36.09 against source
  10.24 (260 pixels), with RGB error falling 161.13 to 28.38; it may add excess texture. For d102,
  texture rises 0.20 to 3.31 against source 15.60 (249 matched pixels), but RGB error worsens
  13.48 to 16.04. The coloured d27 title still leaves a ghost: texture is 1.77 old and 1.91 new against
  source 17.14 (3,243 pixels). White-balloon d190 has no conservative background sample and its
  mask misses visible lettering; its alpha also differs between recipes, so it is not a
  same-support texture control. Old d189 and d102 inputs also contain prior patches absent from
  the isolated rerenders: surrounding input differs at 5,628 and 1,282 pixels respectively. The
  measured hole-background subset differs at 0/296 and 2/251 pixels, but surrounding context can
  change generation. These are descriptive single rerenders, not proof of a recipe-only cause. Evidence is in
  `live/quality-comparison` in the validation folder. CPU LaMa on six old exact crops preserved
  outside-support pixels but retained yellow lettering on d27 and changed its colour heavily;
  blanket LaMa fallback is not supported. New successful unmasked cloud commits now carry
  "Check generated texture" when the stored detection fit requires inpainting and no other
  review reason is already present. Existing reasons retain priority, and rerender/reopen keep
  the flag. This routes uncertainty to visual review without changing pixels or claiming that
  damage was detected. Seven of the 21 saved-demo detections meet this route. Focused native
  and interface tests, all required gates and independent review pass; packaged verification
  remains open. Old
  patches without saved fit classification are not retroactively guessed. A reliable automatic
  texture-loss classifier remains unvalidated.
- **The tone gate's thresholds were tuned on synthetic fixtures.** Alignment needs each ring sample
  backed by 3 background colours within 12 levels and 64 agreeing samples, refuses more than 4
  levels of added channel separation unless a third of the background is near the shown colour,
  and caps a correction at 24 levels; added channel separation also needs background samples that
  show the same separation (`engines/render.rs`). The follow-up replayed 227 stored demo returns on copied evidence: 221 aligned and six
  abstained for channel separation (2.6%). All 227 crop and hint inputs matched the journal.
  Those are older raw returns. The new 21-render 2.0.0 pass and five legacy comparison renders
  aligned all 26 (0% abstention);
  eight rings had measurable texture retention, from 0.313 to 0.935. Selected textured crops
  showed no obvious white patch. This small selection is not a representative screentone corpus.
- **Oversized holes are held out, not tiled.** A hole too large for a 2048 px crop even at zero
  context is left out of a cloud clean plan before consent (`tooLargeIds`) and refused before a
  single-region proposal (`cloud_consent_crop_too_large`); nothing tiles it. Page-edge crops are
  cut inside the page, the same way for estimate, digest and dispatch, but are not shifted inward to
  keep full context. The follow-up corrected mock percentage-to-pixel crop estimates and oversized
  single-region refusal, with explicit tests. The mock is an estimate, not a measurement of native
  fitted-mask bounds.
- **Colour-key transparency is honoured only by the tone check.** A Gray or RGB PNG's `tRNS` key now
  survives re-encoding, and tone alignment ignores keyed pixels, but other engines' statistics treat
  them as opaque. Keyed PNGs exported by earlier builds carry a short `tRNS` chunk that decoders
  drop, so their key is already lost. The follow-up now records the actual recipe reach for new
  attachments, including 1.0.0. Legacy records without an exact reach still use the conservative
  2.0.0 estimate. Recipe-specific reach is covered by regression tests.
- **Local FLUX tone recording and the capture tool's guard are narrowly covered.** `region.rs`
  writes the tone report into a local FLUX patch's snapshot, but only `flux.rs` tests that a tone
  is produced; no test drives the sidecar through the recording. The stage capture tool
  (`mask_stages.rs`) refuses the live library only at its macOS path. It writes only new files in a
  fresh output folder, opened without following links, but a folder swapped for a link while it
  runs is not covered; that would need directory descriptors.

**Review flags (plan section 3).**

- **Old input-change flags clear only on proof.** A flag written under the old 768 px window rule
  clears when the layer's recorded digest shows its input unchanged; a layer with no digest clears
  only through Keep. On a copy of the demo chapter, hiding the lowest layer of each page still flags
  33 later layers (161 under the old rule), all old records with no recorded reach. Batch-run layers
  record no input provenance and fall back to an estimated reach, and the batch run measures its
  edge threshold over its whole engine window, which is not a proven bound.
- **Cloud attention flags are rebuilt from the journal, not stored.** `repairNeeded`,
  `cloudResultNotApplied` and `cloudResultUnchecked` are read from the attempt journal at each start
  and after each recovery (`scan_cloud_attention`, `inference/commands.rs`). Page reads wait up to
  30 s for that read, so a slow journal delays the first page load. A committed attempt whose
  chapter left the library flags nothing, and the Vite mock never produces these flags.
- **Cloud re-render review preservation is covered offline.** The follow-up now carries unrelated
  saved review reasons and layer opacity through re-rendering. A successful render resolves derived
  input-change warnings and restores the reason saved beneath them. The service regression covers
  `unusuallyLarge`, 40% opacity, repeated attachment and the saved prior reason. Live confirmation
  against the new deployment is still pending.
- **Held-only page status was corrected in the follow-up.** A page with held candidates now shows
  the held mark and accessible status in resident and summary views, even when its saved page status
  says cleaned. Status and page-row tests cover this; a final packaged-app check is pending.

**Cloud reliability, batching and recovery (plan section 4).**

- **Chunked cloud cleaning has not run live yet.** A plan of 1,025 regions runs as 5 chunks
  of at most 256 under one consent, with cloud permission, profile, recipe and GPU price checked
  before each chunk (`run_plan`, `inference/cloud_clean.rs`). A separate copied 65-page chapter
  holds 273 real stored-mask regions; offline production planning finds 273 eligible and chunk
  lengths 256 and 17. No cloud job from that chapter has crossed the boundary yet. The native
  test re-reads one templated region; the real re-read opens the whole manifest for each region
  and prepare looks up the journal once per region, so time grows with plan size and was not
  measured on a large chapter, nor was the mixed pass's per-page reopen and survey. The
  follow-up indexes detection and page positions during prepare/bind; per-region disk reads
  remain. The GPU-price comparison now rejects unknown-to-known transitions as well as changed
  prices, with a regression test. The GPU-price check still has no live test through the real
  HTTP path.
- **One unknown Modal submission recovered live.** A guarded single-render probe made exactly one
  POST, deliberately discarded the accepted handle and marked the attempt unknown. Authenticated,
  identity-bound lookup recovered it and committed one patch; two repeated recoveries made no
  second patch or render POST (`live/recovery-plan.execution.jsonl` in the validation folder).
  This proves the tested recovery path, not all crash timings. A submission refused before enqueue
  is no longer recorded as unknown. Beam has no lookup contract
  (`lookup_attempt` in `inference/http.rs` answers nothing), so an unknown Beam submission stays
  unresolved and holds its region until the user abandons it and accepts the duplicate risk.
  Attempt journals are never pruned: each start reads every record and audits committed ones
  against one manifest snapshot per chapter, at a cost not measured.
- **Cross-process locking covers job manifests and the library index only.** `settings.json` and
  `inference.json` writes are serialised within one process, not across processes, and journal and
  manifest writes are not ordered by any lock. Builds older than this one take no lock; the
  stale-manifest check catches them, except a writer that renames between that check and the
  rename. The Windows lock code compiled for `x86_64-pc-windows-msvc` in a scratch crate and has
  never run, nor has its lock-file replacement handling. On a file system that cannot lock, only
  the in-process lock holds, with a line on stderr.
- **Busy and stale chapters are reported unevenly.** A command gives up after 5 s (`PATIENCE`,
  `project/lock.rs`) when another process holds the chapter, which can refuse one queued behind
  another process's long edit render; waits on this process's own threads are unbounded. On the
  cloud paths the same refusal reads as `project_error`, `consent_invalid` or `analysis_stale`, and
  a stale library index as the generic `notice.library.changeFailed`. `deleteMask`, `deleteRegion`
  and `keepDependencyResult` now catch and report refusals without recording an undo action,
  covered by frontend regression tests. Reloading a chapter after a stale save resets the selection.

**Layers and opacity (plan section 5).**

- **Pixels do not follow a layer during a gesture.** While dragging, turning or sliding opacity,
  only the outline and frame move; the pixels change when each write lands, for a move or turn on
  release. A live preview would need a layer-only tile variant. A key press while a gesture's write
  is in flight is ignored.
- **Some layer rules are judgement calls.** Clone and heal layers count as fixed redraws, which the
  plan does not decide. A moved paint stroke carries soft edges blended with the page where it was
  drawn. A cloud re-render already carries the layer style; the follow-up added an explicit
  40% opacity preservation regression and corrected the earlier contrary claim.
- **Tile appearance identity was timed on a synthetic chapter only.** Listing 100 pages with 1,000
  layers took about 66 ms in a debug build (a `library.rs` test); no real long chapter was timed.
  The `appearance-v2` digest (`tile.rs`) starts the tile cache cold once after upgrade, a cleaned
  tile requested without its `a=` digest is never cached (run stub headers send none), and in a
  long strip an edit on an evicted page refetches every on-screen tile once. `APPEARANCE_VERSION`
  must be bumped by hand if compositor output changes with no input changing.

**Settings and the Text cleanup panel (plan section 6).**

- **Missing-model notice links were connected in the follow-up.** Notices now have a Models action
  calling `openModelSettings`. Known native model names resolve to their row, and other notices
  open Models generally. Notice DOM and settings-link tests pass; final WKWebView focus and scroll
  validation is pending.
- **Strict Cloud Detect was corrected in the follow-up.** CTD or Ogkalu Small selected with Cloud
  GPU now refuses in the interface and natively before chapter work or upload, naming the local-only
  choices. Local Detect and Clean-only remain valid. Native and frontend tests pass; live validation
  is pending. A legacy stored split of analysis targets still settles to This computer.
- **The Text cleanup layout fixes still need final WKWebView checks.** The follow-up preserves
  the anchor on resize, reserves 62 px for bottom controls, and portals the tight-mode ColorPicker
  to the document body so scrolling rows and animated ancestors cannot clip it. Portal ownership,
  focus dismissal, tight-mode changes and cleanup are covered by DOM regressions. Mixed cleaning now persists as `session.cleanLocalFirst`, defaulting
  to false. Focused DOM, window and persistence tests pass. The 96 px label column still assumes
  English label lengths; real short-window geometry and popover clipping remain to be checked.
- **The unused native Text cleanup entry path was removed in the follow-up.** `apply_tool`
  now handles stored-region edits only; Text cleanup page runs use the prepared run command. Old
  Settings paths, the lock description and the live-demo chapter name were also corrected. The full native test and clippy gates pass.

**Koharu comparison (plan section 7).**

- **Shared render crops were measured and not built.** The harness compares one crop per group
  with a port of Koharu's component packing (512 px cores, 128 px context; `koharu_pack` in
  `text_groups/measure.rs`). On the 5 demo pages with whole-page SAM, packing sends 17 requests
  instead of 25 for about the same crop pixels; on the 6 real scans with CTD and Ogkalu Small, 32
  instead of 79 for 16% fewer. But 10 of 17 and 30 of 32 packed requests write more than one group,
  and 7 of 17 and 21 of 32 write across two balloons. Shared crops would need whole-group packing
  that never crosses a sure balloon, a hint mask limited to member groups, attachment by each
  group's own write support, request-to-group mapping in consent and cost, and a predecessor digest
  over all members, so each group keeps its own crop. The port was checked by reading Koharu's
  code, not by running it; the harness uses lettering bounds rather than the fitted hole, and packs
  our lettering rather than Koharu's dilated layout mask. SAM was not measured on the real scans.
- **Overlapping cloud SAM tiles now have a five-page live comparison.** Analysis protocol 1.1.0
  overlaps
  tiles by at least 256 px and stitches the mask by core ownership (`planned` and `stitch_mask`,
  `cloud_tiles.rs`). Against whole-page SAM on the same decoded demo pixels, pooled IoU rose from
  0.578 to 0.686 and disagreement near seams fell from 62% to 25%, while the upload for a 1136x1601
  page grew about 2.3 times (4.19 MP against 1.82 MP). A separate guarded capture submitted
  exactly 20 tiles, four per page, on the same five 1136x1601 originals. Compared with fresh
  local whole-page macOS ONNX masks, the stitched cloud Linux ONNX masks have pooled IoU 0.7709,
  precision 0.8796 and recall 0.8618 (`live/sam-comparison/comparison.md`). The models share a
  checkpoint lineage, but verified head graph bytes and spatial inputs differ, so this measures
  agreement, not exact backend parity or
  annotation accuracy. On demo page 01 the merged RT box covers text past 1024 px as one hull where the local detector cuts it
  at its own half line.
- **Koharu's mask dilation was compared by geometry only.** At the demo scale Koharu's layout-mask
  radius is 9 px; our fitted core reaches the seed grown by 9 to 14 px, and at least 5 px on the
  fallback. No render compared the two, and the 128 px context was benchmarked only with the local
  model above.
- **Model names are exact in diagnostics, with a few loose ends.** Group snapshots, the analysis
  preview, the run record, run logs, the capture tool and harness reports name each checkpoint,
  revision, execution and spatial input (`describe_models` in `text_groups.rs`). Ogkalu Small has no
  pinned revision and reads as not pinned. The spatial label "whole" does not tell a page from a
  long-strip segment. The mock's analysis evidence has no model descriptions. Graph file names
  (`koharu_samts_*.onnx`), a few native error strings and the gateway's wire errors still say SAM.

**Release gate.**

- **A new Modal gateway is deployed; old-recipe pre-enqueue refusal passed live.** The user completed
  setup of `mc-e833yu` in workspace `k-omiq`, beside the existing gateway. The validation app's
  authenticated handshake passed and its five-page Cloud Detect completed with overlapping
  analysis protocol 1.1.0. The render handshake and all 21 subsequent committed jobs name
  preprocessing 2.0.0 and the pinned 4B recipe. Render wire protocol is separately 1.0.0.
  The app connection check took 1906 ms. The production profile was not switched. One guarded
  old-preprocessing 1.0.0 render POST returned bound `unsupported_recipe` before enqueue
  (`live/old-recipe-plan.execution.jsonl`). This does not test an old worker already in flight
  when the recipe changes. Deployment and job charges are not reported by the gateway.
- **Release gate step 6 is partly complete.** In the packaged validation app, Cloud Detect on
  five copied demo pages saved 21 detections and 21 held areas. Cleaning those saved detections
  then committed 21 jobs and 21 patches, leaving 21 held areas. The journals span 140.541 s from
  first creation to last attachment. All jobs use preprocessing 2.0.0; all reported costs are
  unknown. Snapshots are in `live/detect-baseline` and `live/saved-clean` in the validation folder.
  The copied comparison reproduced every patch and retained every group and held reason,
  with zero changed pixels outside write support. The fresh Detect and clean pass on the
  separate untouched chapter remains open. The isolated library and USD 10 cap still apply.
- **Live Cloud Detect exposed missing usage journals.** Chapter analysis previously dispatched
  tiles without writing analysis attempts, so the usage view displayed zero after real work.
  A follow-up now records durable per-page attempts, tile dispatch, results and unknown prices.
  Independent review found and corrected final-tile cancellation and two-model request-count
  limits. It also corrected a partial-price bug after a later tile lost its response: the whole
  attempt now stays unpriced. Final review and all required gates pass: Clippy, 1,497 Rust
  tests (35 ignored), 1,853 frontend tests, cloud Python 297 run (23 skipped), and provisioner
  Python 159 run (8 skipped). Verification in a rebuilt packaged app remains open. Earlier unrecorded
  analysis usage cannot be reconstructed as actual provider billing.
- **The final interface still needs WKWebView validation.** The Text cleanup panel (its morph, focus, tight
  mode, and Enter on its action), the Settings sections and deep links, layer drag, the turn handle
  and the opacity display were checked in jsdom, Vitest and code only, with some layouts in a
  Chromium mock. The shipped macOS engine can differ in `ResizeObserver`, `animate()`, pointer
  capture and focus timing. The 28 px turn-handle hit area was not tried with a trackpad or pen,
  long model names in narrow selects were not looked at, and Settings heights with groups expanded
  were not measured. The validation window continued changing between observations during the
  attempted switch to the rebuilt app, and computer use was interrupted. Exclusive app control
  was requested before the remaining packaged checks; independent cloud probes continued.
- **The writer behind the earlier manifest rollback is not proven.** Interprocess job locks and
  stale-save rejection now prevent the known classes of lost commit (`project/lock.rs`,
  `Job::flush`). The likeliest writer was the GUI's hand re-clean of `c32-p004-d85` through the edit
  render, which read the manifest between two run commits and saved it between two later ones.
  Timestamps have one-second resolution and there are no GUI logs, so this is inferred.
- **The supplied white-fringe screenshot has no traced source.** Its page was not found on disk, so
  it stays a visual symptom. The measured cause on the demo chapter is raw FLUX tone drift, fixed by
  the 2.0.0 alignment; screentone texture lost in pure-white raw returns is not fixed (see above).
- **Concurrent test runs collide on shared temp paths.** With several `cargo test` runs at once,
  the real-chapter export test and `run::tests` cancel and resume tests failed with NotFound or
  Stale errors under shared temp paths such as `$TMPDIR/cleaner-tauri-export`; each passed alone or
  with a private `TMPDIR`. Under heavy load jsdom tests time out at Vitest's default 5 s, and the
  500 ms bound in the library listing test may fail on a slow machine. The follow-up first full
  gate, run beside frontend tests and model compilation, also hit the five-second interprocess
  lock wait in the simultaneous-writer test and two jsdom timeouts. Sequential reruns passed
  all 1,486 Rust tests and 1,848 frontend tests, with private Rust temporary directories.

**Follow-up validation artifacts (2026-09-27).**

- **The SAM capture probe recovered from two local startup blocks and completed.** Its first
  prepare waited at macOS Keychain and was stopped for rebuilding; the next hit a blocking HTTP
  client inside an async runtime. After the probe's main became synchronous, prepare and the
  approved execute both succeeded. The final capture made 20 tile POSTs across five pages,
  persisted five stitched masks and its progress file ended at `complete` with `totalPosts: 20`
  (`live/sam-capture/analysis-progress.jsonl`). The two blocked starts submitted no GPU work.
  Any OS password entry was left to the user. The isolated fixture creator made a
  65-page, 273-region chapter for real chunk-boundary checks, a one-region recovery chapter,
  and a five-region legacy texture comparison. The latter retain exact source pixels, fitted
  masks and stored ink. Offline production planning confirms chunk lengths 256 and 17; live
  chunk dispatch remains open. The five-region texture comparison completed with the partial
  improvements and remaining failures recorded above.

- The debug probes bind approval to the canonical endpoint and check the selected profile and
  cloud permission before each SAM upload. Their artifact paths reject traversal and symlink
  escape from the validation folder. The unknown-submission, old-recipe, SAM and fixture tools
  compile under the isolated app identity; the separate live outcomes are recorded above.
- The local evidence folder is `/Users/caved/dev/manga-release-validation-2026-09-27`.
  It holds preserved inputs, human annotations, detector and texture comparisons, test logs and
  the validation ledger. The original result folder and production library remain read-only.
- The debug-only `release_probe` bound one unknown-submission render to the validation app,
  exact consent scope and a single-use execution record. Its GET-only capability mode and live
  execution passed. The execution log records one POST, then unknown, committed and two repeated
  recoveries with one patch (`live/recovery-plan.execution.jsonl`). The POST count is a probe
  code-path count, not independent provider billing evidence.
- Ring texture retention alone is not a validated texture-loss detector. The full 227-return
  calibration gives false positives for tiny holes and white balloons and misses damaged
  hatching at a conservative threshold. `source_texture` now records the source-ring statistic
  alongside retention for diagnosis. No threshold or automatic LaMa route was shipped on that
  evidence. Five new 2.0.0 returns now have matched hole-local measurements, but they still do
  not establish a reliable automatic quality gate.
- The original cloud prerequisites and release fixes overlap in 94 files. Applying the release
  patch directly to this branch's HEAD fails without those prerequisites. Reusing the preserved
  integration history would also commit the original-work snapshot. The user approved committing
  all current source items together as milestone 1, including those prerequisites. Helper binaries
  remain excluded. No push or release is authorized.

**Earlier entries.**

- The Run input races were tested with deferred backend answers in Vitest. Rapid
  Enter and click focus timing in WKWebView has not been measured on a device.
- Cloud clean and detection batch ids are now tracked until a run-finished
  event. That timing was not measured against a live cloud GPU.

- **Modal stop and token rotation fixes have only offline tests.** A failed
  `FunctionCall.cancel(terminate_containers=True)` now keeps its render job and
  heartbeat tracked and returns `stop_uncertain`. A failed spawn keeps its claim
  and returns `submission_unknown`. More than 64 queued jobs, separate role
  markers, and replacement token checks were tested with fakes. None of these
  paths has been exercised against a live Modal deployment. Whether Modal
  reports a successful cancel before its GPU container stops billing is unknown.
- **Admission and stop ordering has not been tried live.** The gateway holds a
  process lock through render submission and through analysis spawn and call ID
  recording, so a concurrent stop sees those calls. This has only been exercised
  with local fakes, not with Modal gateway threads and a real Dict.

- **The engine star ratings in setup and Settings.** Efficiency and Lightweight, out of five,
  in `src/lib/model/pipelines.js`, are provisional: they come from file sizes and the per-page
  timings above, not from one benchmark run over every engine on the same pages. The engines
  marked Soon (RT-DETR v2 + COO + SAM-TS, Big LaMa, Qwen-Image-Edit-2511) and the FLUX family
  have never been benchmarked here, so their stars are estimates from published model sizes.
  Replace each row's `rating` once a common benchmark exists.
- **Per-language detection selects languages, not detectors.** The run now reads
  `session.detection` (`RunSelection::from_args` in `src-tauri/src/run.rs`): a skipped language is
  held for review at clean time, a run with every language skipped refuses with a notice, and the
  Japanese choice with OCR turns the rescue on. Every enabled language still runs the same
  CTD + RT-DETR detector, because it is the only legacy detector there is. An outside-bubble
  opt-in region under a partial selection is held for review, since nothing tells which language
  it belongs to.
- **The text-shaped detection path is review evidence on one machine.** RT-DETR v2 and SAM-TS-L
  mask-only ONNX exports exist and are wired as the optional text-shaped review
  (`docs/model-workflow-benchmarks.md`, `docs/component-write-contract.md`). COO MTSv3 is excluded
  while its rights are unresolved. The RT full graph does not merge boxes across its two-tile seam,
  which reference parity depends on. A small whole-page RT run cannot be stopped mid-run (about
  86 ms); cancel takes effect after it returns. Evicting parked sessions before SAM means CTD,
  LaMa and the sidecar reload on next use, at a cost not measured. Completed analysis request ids
  stay in memory as small single-use tombstones for the life of the process.
- **The review's canvas tints and cloud analysis flow were seen only in Chromium.** The SAM mask
  and W tints are now source-pixel canvases (`src/lib/dialogs/MaskTint.svelte`) instead of CSS
  luminance masks, and were checked for exact pixel counts at fit, 1:1 and 400% in the Vite mock.
  Nobody has looked at them, or at the cloud consent, progress and review-only screens, in the
  shipped macOS 11 WKWebView, where `getImageData` on a decoded data URL is expected but not
  confirmed to be untainted. The cloud flow has run only against the mock endpoint.
- **The FLUX, Big LaMa and Qwen cleaners have no downloader.** FLUX models come from the
  separate helper and show as Via helper when it lists them; Big LaMa and Qwen are drawn
  disabled.
- **The tray template icon has not been seen in a real menu bar.** A monochrome template image
  now ships (`src-tauri/assets/tray/`), made from a script and checked only as a file. On a 1x
  screen AppKit downscales the 36 px image, and nobody has looked at the result. The non-macOS
  branch was not compiled here. Close-to-tray, the tray click and the single-instance guard have
  not been run on Windows or Linux.
- **Nothing has been executed on Windows or Linux.** Every DirectML and CUDA entry is an
  unmeasured candidate, reported as such, with no measured peak, so the memory gate never fires
  on one. The same holds for custom-protocol throughput there: no number is offered and the tile
  constant stays provisional, because measuring it on a Mac would reproduce a recorded error.
- **The Windows repairs of this round are all arguments, not runs.** Each is sourced from a
  binary or a vendored crate rather than from a machine, and each names the observation it rests
  on: that bundled resources land in the executable's directory and are therefore first in the
  loader's search order, so the staged Visual C++ libraries are found; that `LoadLibraryExW`
  answers 126 for a present file whose own imports are missing, which is what separates a missing
  dependency from a missing runtime; that `GetEpDevices` lists a DirectML device on a build whose
  RTTI carries `DmlEpFactory`, which is what lets DirectML be declined on a machine with no
  Direct3D 12 adapter instead of failing a session build to find out; that a mapped DLL may be
  renamed within its directory although it may not be deleted, which is what makes a runtime
  replaceable after the first install; and that `SetNamedSecurityInfoW` narrows the settings
  file's DACL. The first CI run on `windows-latest` is what tests the compile half. Nothing tests
  the rest but a Windows machine.
- **The WebGPU plugin on the two platforms it was added for.** Direct3D 12 on Windows and Vulkan
  on Linux, where the only measurement is Metal's: every automatic GPU choice off macOS is
  reported as unmeasured, and a forced WebGPU is still gated against a peak taken on a different
  backend. The loader half is the sharper hole. The runtime's directory is put on the library
  search path so that the two shader libraries the plugin loads by name resolve from the download
  folder, which is what the documented search order says will happen and what no Windows machine
  has confirmed; if it does not, the plugin registers, enumerates no device, and the row blames
  the missing adapter.
- **A quality comparison of the inpainter on screentone.** No published comparison exists and the
  corpus with ground-truth masks has not been run, so there is no manga quality evidence for the
  optional redraw sidecar; it is offered and not recommended. The comparison the removed fast tier
  was the other half of is now history rather than an open question, and so is the question of its
  blend ring.
- **The PSD writer against real Photoshop.** An independent reader opens every fixture, layered
  and flat, at 8 and 16 bits; Photoshop has opened none, and the two rules a second reader would
  not have caught, the 16-bit layer block and inverted CMYK, are followed from the specification
  rather than from a file Photoshop wrote. No text layer is written at all, so the vertical-text
  hazard this item once named is moot. Beside it: the large-document variant is not written and a
  page over 30 000 px a side is refused rather than promoted to it; the 2 GB file-size ceiling is
  now computed before the first byte and a larger file is refused, but the limit comes from the
  specification and no file near it has been opened in Photoshop; a stitched PSD is refused although a
  row-streamed background would make it possible; an indexed or sub-8-bit page is refused rather
  than promoted, because the statement a promotion would need has nowhere to reach the interface;
  and the file carries no provenance record, so a typesetter cannot tell from it which engine
  cleaned which region.
- **A chapter written before pages were imported into the library is not migrated,** and behaves
  exactly as it did, including breaking if the scan folder goes. Nothing hashes the original a
  converted page was made from either, so replacing that original after the chapter exists is
  neither honoured nor reported. The import now checks free space before the doubled write, with
  an estimate that errs high, so it may refuse a chapter that would have fit. Retained Try again
  revisions of legacy regions also use disk, and nothing prunes them.
- **The rescue's thresholds are floors with reasons rather than measurements.** All twelve rescues
  came back fully Japanese and every reading that failed the share test failed it at zero, so the
  whole evidence is two clusters with the band between them empty and the threshold placed in the
  middle of nothing. What it guards against is a reader hallucinating a few characters over
  lettering it cannot read, and nobody has produced that case: the honest way to get one is a scan
  with English lettering inside a drawn balloon that the identifier fails on, which none of the 28
  reference pages contains. The two-character length floor counts punctuation the share test
  abstains on, so one rescue clears a floor meant to mean two characters of evidence; tightening
  it would also refuse two genuine one-kana balloons, which are common in manga. The reading's own
  score is carried for exactly this argument and nothing routes on it. The reader decodes greedily
  where the reference uses four beams, and stops at 64 tokens, which nothing has reached.
- **The adoption floor is untested from below,** now `TEXT_BOX_SCORE` (0.5) in `text_groups.rs`,
  which replaced the adoption path: a text box under it with no mask lettering is held as a
  candidate. It is reused from the score the balloon question already calls sure although the two
  questions differ in what a wrong yes costs: painting over art there, a review row here. The
  lowest box adopted scored 0.53 and nothing between 0.5 and 0.53 was seen either way. A text box
  with little or no segmentation under it now gets a bounded Otsu estimate (`estimate_ink`, at most
  45% of the box) instead of no fallback. A box straddling a segment cut and taller than
  the detection overlap is now joined across the cut and held for review at its full extent
  (`a_box_taller_than_the_strip_overlap_is_held_at_its_full_extent`), on a synthetic strip only. The
  top fragment is processed as an ordinary box before the next crop confirms the join; the join then
  replaces that provisional result with the one held row, so the work may be wasted but nothing is
  painted.
- **The inner paper reading's margin is two points wide on one scan set,** 28 pages of one title
  screened at one line count: the narration boxes reach 14 per cent, the allowance is 16 and the
  nearest thing that must not pass is at 19. A finer or lighter tone puts fewer dots off a capped
  tolerance and a box that is mostly frame puts more, and neither has been seen. The direction of
  the error is the safe one, since too high admits tone and too low sends narration back to review.
  The reading also cannot tell a hand-lettered effect on white paper from narration on white
  paper, because between the strokes there is the same white: two such effects moved to uncertain,
  where they are still held rather than painted. The difference is in the lettering, and nothing
  in this module looks at the lettering. Moot since 2026-09-29: the inner reading was removed.
- **Caption boxes and spiky balloons the balloon detector labels free text** are still held back on
  noisy scans: narration whose interior spread runs past even the widened tolerance, and spiky
  balloons over screentone whose first rings are already tone. The opt-in cleans them; without it
  they are Clean anyway rows. The strip test that decides whether a merge crosses a balloon still
  reads against the old fixed tolerance, which on the same scans over-reports boundaries and
  under-merges.
- **A Manga109-trained detector,** which would have answered the free-text and sound-effect
  questions directly, is blocked twice over: the published ones are AGPL through their training
  framework, and the dataset that would let one be trained here is licence-gated.
- **An optional model that will not open is now named, and was tested only in part.** When the
  reader fails to open, the run goes on without it and raises `notice.run.ocrRescueUnavailable`
  with the reason. The zero-byte truncation case runs in the tests. The case of a nonempty corrupt
  file is skipped when the gate weights are absent. A failure in a real install has not been
  observed.
- **The redraw sidecar's return to its floor between regions,** instrumented and never judged
  for want of a threshold. Nor does its allocator fail fast: the limit is documented upstream as
  a guideline, and a 1024² render exceeded a 12 GB limit by 440 MB and completed normally. What
  is real is the parent's per-region deadline, hang kill and peak check, one region coarser.
- **The second redraw backend on CUDA,** the platform it exists for: the weights would land in
  video memory and the host-side figure this rung declares is not the one that would bind. The
  crop's 24 to 80 px context clamp is unswept too, unlike the resolution and padding beside it.
- **The disagreement between two harnesses on one machine,** 1.7× to 2.1× on the same models.
  The in-process rung peaks came off the same tool that produced the wrong sidecar figure and
  may be the same wrong line of its output.
- **The cloud path has run once on a real Modal account, never on Beam.** On 25 September 2026
  the setup helper deployed to workspace `k-omiq` on an L4 and a contract client rendered a
  synthetic 512x384 crop; the numbers are in [cloud-work-log.md](cloud-work-log.md). Setup took
  191 s, a cold render 71 s and a warm one 10 s, the pipeline held about 7.0 GiB of GPU memory,
  cancel, recovery after scale to zero and cleanup worked, and the whole run metered $0.10 to $0.15, covered by Modal's credits. The
  desktop app did not drive it: the Rust client, the keychain and the consent flow in WKWebView
  are still untested live, and so are real manga crops, larger crops, a workspace with environment
  access control, how long results are retained, the Dict size limits and a render past its
  15-minute bound. The gateway reports no cost, so the consent dialog still shows none. Nothing
  has run on Beam: how an HTTP enqueue body maps to task arguments, the shape of the task id
  answer, reading a Map from inside a container, the invoke URL's format, the name and id filters,
  what `authorized` actually checks, and whether RTX5090 runs the CUDA 12.9 wheels are all unknown.
- **The request limits the cloud runs under are the drafted ones.** The wire contract's size and
  pixel limits were written before any measurement, and cloud execution now runs with them as host
  safety ceilings. None has been checked against what a provider accepts or what the GPU renders
  inside the job timeout.
- **The cloud setup on Windows is an argument, not a run.** The frozen Windows helper has not been
  built, and `cargo check --target x86_64-pc-windows-msvc` cannot run on the Mac this was written on
  (`ring` needs the Windows C headers), so no machine has compiled the `cfg(windows)` code. Stopping
  a setup there kills the helper process alone: a PyInstaller one-file helper runs as a child of its
  own bootloader, and that child may outlive the stop. On Beam, the path separators in the upload
  ignore file are untested there. The release workflow's helper build step has not run either; a
  build without the helper refuses setup with a typed error rather than looking for a Python on the
  machine.
- **A few narrow windows where the account and the app can disagree.** The setup journal now
  records the intent to create Modal's access token before the token exists, so a helper killed
  between the two is detected on the next start as an orphaned token (`ERR_ORPHANED_TOKEN`). The
  app cannot delete that token itself: the user removes it in the Modal dashboard, resume stays
  blocked, and recovery is Cleanup and then a new setup. A render past its 15-minute interactive
  bound stays accepted until the next start resumes waiting on it. Settings writes are now
  serialised by one lock. The gateway reads its own job records through the Modal
  SDK, which unpickles them; the records are written by the app's own worker inside the user's
  account, so the trust boundary is the account, and nothing narrower has been added.
- **Beam cleanup empties the job map but cannot delete it.** beta9 0.1.268 has no call that removes
  a Map, so cleanup deletes every key, lists again, and fails if any key is left. An empty map
  object stays in the account. Cleanup also assumes that listing an empty map answers with no keys
  rather than a refusal, and that Beam's task list answers an unknown task id with an empty result;
  neither has been seen live. A seed download the provider cannot report on is judged gone after 180
  s with no status, or 60 s with no new heartbeat; those bounds are guesses, not measurements of a
  slow cold start.
- **The Modal sign-in inside the helper is bounded at 40 s,** below the app's 60 s limit on inspect
  and plan, so that a slow sign-in fails as a typed error rather than as a killed helper. The SDK
  itself retries for about a minute. A very slow network can hit the bound, and no measured network
  has.
- **The cloud interface has not run in the shipped webview.** It is tested in jsdom and was walked
  through in a Chromium mock. The app ships WKWebView on macOS, where focus and event timing differ,
  and no `cloud://attempt` or `provision://progress` event has yet crossed a real Tauri bridge.
- **The M0 failure ledger has no human labels.** `spikes/m0-ledger` runs the current pipeline on
  real pages and writes one row per region with its artifacts. On three of the user's scans it gave
  22 regions: 21 cleaned and 1 held by policy. Missed discovery, a weak mask and bad inpainting are
  categories only a person can assign, so those columns are empty. A detector seed the pipeline
  drops as empty gets no row. The raw crop and ink are proxies and carry a `_proxy` name; the
  pipeline does not expose its true base mask or candidate box. Times are CPU only (61.1 s, 19.7 s
  and 27.4 s) and the peak RSS is cumulative for the process.
- **No labeled holdout exists, so M5's exit has no numbers.** `spikes/holdout-metrics` computes
  instance and page recall, text-pixel recall, non-text exposure, protected-art damage, false
  candidates per page and correction time from labels, by slice. Nobody has labeled a page, and
  no human mask or reconstruction review has run. The padding default, mask threshold, crop
  overlap and provider limits wait on those numbers.
- **The underlay read sizes are measurements, not budgets.** On a synthetic 4000x6000 page with 96
  intersecting and 32 outside patches, the Gray8 window was 2,611,456 B with a 45,236,224 B peak RSS
  and a 624 ms read; the RGB8 window was 7,834,368 B with a 126,812,160 B peak and a 1,462 ms read.
  No cache was added. Real page histories and GPU paths were not measured.
- **Needs review's Undo is the chapter's last action.** It reverses the latest journal entry, not the
  specific change that flagged the layer, and is labeled "Undo last action". Undoing the one causing
  change is not built.
- **Cloud analysis (M7) has run only against fakes.** Consent, grants, the journal, tile submission,
  validation, stitching and review-only evidence are tested with an in-process contract fake and the
  Python gateway driven in process; no actual HTTP loopback test drives the analysis path end to end.
  A gateway with the job routes (2026-10-01) is polled per tile and Cancel also cancels the tile in
  flight on it; an older gateway answers each tile in one synchronous request, so Cancel there stops
  the tiles not yet sent and a tile already sent runs to its end. Remote
  evidence lives in memory and is lost on restart. No real Modal or Beam account has run it, so
  latency, transfer time, GPU and host memory, the bill, provider retention and training terms, and
  cleanup are all unknown, and the consent screen shows the cost as unknown. Whether remote SAM is
  worth an upload has not been measured against the local path.
- **Cloud Detect is slow from per-call overhead, not from the GPU sleeping (2026-10-01).** A real
  headless Detect (RT full and SAM-TS-L in the cloud, the run's own tiling, stitching and fit) on six
  real pages (one 654x919, five 1080x1536; 42 tile calls) against a scratch copy of the gateway on an
  L4. The analysis GPU stayed warm across pages; the app releases it only after a stage
  (`release_idle`). GPU work was about 3 s a page in every mode (SAM about 0.7 s a tile, RT about
  0.05 s). Cloud time per page: synchronous route one tile at a time 22 to 25 s; the job route the
  current app uses 28 to 34 s (its 0.5, 1, 2 s poll backoff sees a 1.5 s tile late); every request of a
  page at once 13.5 to 15 s; one call carrying the whole page 8 to 11 s. Each synchronous call spends
  about 2.3 s in the gateway outside the GPU: spawn 0.47 s, waiting on the call 1.5 s for 0.44 s of GPU
  work, Modal Dict reads and writes about 0.6 s. Final Detect masks and lettering were pixel identical
  across modes, except one parallel run on a fresh container (16 of 903,566 mask pixels, two boxes
  moved 1 px); two warm parallel runs matched exactly. Parallel requests against the shipped gateway
  all failed: each submit's `_require_installed` reloads the weights volume, and concurrent reloads
  fail with "there are open files preventing the operation". A per-container capability cache fixed
  that. The page call shipped as analysis batches (`analysis.batches`, see cloud-provisioning.md),
  with each tile PNG sent once and a lock around the gateway's volume reloads. Live on the same six
  pages with the shipped code: 8.6 to 8.9 s a page in batches against 26.3 s in tile jobs, the run
  60 to 73 s against 166 s, final masks and lettering pixel identical. Not measured live: a cold
  start inside a batch, a batch cancelled mid-run, and a long strip that needs several batches a
  page.
- **Long-strip joins are safe, not complete.** A box touching the bottom of one crop is cleaned
  before the next crop can confirm that it continues. When the join is confirmed, the provisional
  result is replaced by one held review row and never painted, so that work may be wasted. A real
  tall fragment pair with strong sideways drift across a cut stays as two held review rows. The
  whole-strip box merger can still combine strongly overlapping detections before the continuation
  check sees them.
- **Workflow status trusts a file's size and time.** The status view caches graph and runtime
  digests by path, size and modification time, so an edit that keeps both reads as verified there.
  Analysis, writes and the explicit Check still hash in full. Memory readiness counts parked CTD,
  LaMa and sidecar sessions as room that can be freed without knowing their size; analysis evicts
  them, checks again and refuses by name if room is still short. In Settings, a Check that fails for
  another reason, such as a missing or unreadable file, makes the all-text summary say the checksum
  failed, while the model's own row shows the actual error.
- **The underlay predecessor digest follows manifest row order,** not z-order. A hand-reordered
  manifest changes the digest and reads as drift. Changing that needs a versioned hash and a reader
  migration.
- **Some cloud edges are handled in the interface only.** A cloud render that lands on a mask
  deleted while it was pending is ignored by the interface; what the real backend does there is not
  verified. A plain undo pressed while a render is pending undoes the previous, unrelated entry, and
  the landing render then clears the redo stack. Settings checks that the runtime loads through
  `diagnostics`, but first-launch setup and the text-shaped review's readiness still count an
  installed runtime as ready. The orphaned-token recovery text names Modal's "Settings, then Proxy
  Auth Tokens" path, which was not checked against Modal's current dashboard.
- **No analysis worker is deployed by setup.** The Modal and Beam deployments do not upload SAM or
  RT graphs to the user's account, because the rights decision forbids the app hosting or
  distributing them. A live analysis route therefore answers a structured 503 until someone deploys
  graphs there. Starlette is not installed here, so the Beam mount test is skipped.
- **The cloud analysis interface has known gaps.** The capability list shows SAM and RT until the
  first request asks the endpoint, because listing on open would wake a paid endpoint. A cloud
  result replaces the local review of that page. File errors and three internal tile-planner errors
  still show the generic failure text.
- **Cloud recovery fails closed on old records.** Attempt records written before the underlay
  hashes existed now read as stale, so such a result cannot attach. Revoking a cloud profile leaves
  its unconfirmed proposals in a bounded in-memory cache until they expire; the profile epoch
  refuses them at confirm.
- **The old cloud review causes are still read.** The native library index (`library.rs`) and the
  frontend review model still map the rejected-request review states of the earlier cloud design;
  the accepted one (`review.reason.cloudAccepted`) now loads as no flag (`review_flags` in
  `library.rs`). Nothing writes them any more and no released build ever did, but removing them
  is a change to how saved projects are read, and it was left for later.
- **The cloud token prompt after a macOS update is only reduced, not removed.** The credential
  summary now checks the keychain item without reading its data, so an updated app still reads
  the setup as ready, and the first token read of a launch is kept in memory. Proven against the
  real login keychain for an item another program wrote (`macos_probe_answers_without_reading_data`,
  run by hand). The login-password prompt on the first cloud request after each update remains
  until releases carry a stable signature: the `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`
  and `APPLE_SIGNING_IDENTITY` release secrets are not set. The same prompt applies to the Hugging
  Face token in `weights.rs`, which was not changed. Not observed across a real updater install.
- **Linux without a Secret Service cannot keep a cloud setup.** The helper stores the runtime
  token persistently or fails with `ERR_SECRET_STORE`; there is no session-only path for setup.
- **The session record can still switch the cloud permission off.** `reconcileSettings` lets the
  stored session override `settings.json`, so a lost `localStorage` write turns cloud off on the
  next launch. It fails safe (off, setup kept) and was left as is.
- **Stopping the cloud GPU is proven offline only.** `GET /mc/v1/gpu` and `POST /mc/v1/gpu/stop`
  (`deploy/cloud/common/gpu.py`) run against fakes: heartbeat staleness, idempotent stop, a stop
  with renders in flight. Not run on Modal: that `FunctionCall.cancel(terminate_containers=True)`
  ends a busy container at once, that `modal.experimental.stop_fetching_inputs()` in the
  `release` method sends an idle one away without waiting out the scale-down window, that
  `@modal.exit` runs on either, and how long a stray release cold start costs when the container
  scaled down first. Existing deployments need the cloud setup updated before the row appears.
- **Beam reports no GPU status.** Its gateway answers `supported: false` (and 501 on stop), so
  the app shows no cloud row for Beam. beta9's `ListContainers`/`StopContainer` were not wired:
  the Beam task queues have no exit hook for a heartbeat and the calls were never tried live.
- **The cloud GPU row follows `cloud://analysis` only.** Analyses started through other flows
  (the run analysis commands being added alongside) do not keep the GPU poll
  up by themselves; a container they start still shows once the poll next reads it.
- **How long a released GPU container takes to leave is not measured.** After a stop the gateway
  keeps an idle container's heartbeat until the container removes it, and the row says "Stopping"
  for up to 2 minutes (`STOP_SETTLE_MS`) before it shows the container's own state again. The
  per-role stop, the provider poll that keeps a finished render, and the tracking of parallel
  analysis tiles run against fakes only (`deploy/cloud/tests/test_gpu.py`).
- **Cloud detection in Text cleanup ran live on five copied demo pages.** `analysisTargets`
  routes RT-DETR v2
  full and SAM-TS-L to the user's endpoint under a run grant (`propose_run_analysis`,
  `confirm_run_analysis`, single use, 120 s). Grant checks, crop mapping and stage routing run
  against fakes (`run_analysis` and `run::tests::cloud_stages`), and the packaged validation app
  completed the five-page Modal Detect run. A separate guarded capture compared cloud tiled SAM
  masks with local whole-page masks on those same source pixels (above). Neither Beam nor the real scans in
  `~/dev/120 noisy png` have this comparison.
- **Run source binding and tile-time profile revocation are proven offline only.** A changed
  page is refused before its first tile, and selection or epoch changes stop later tiles in
  fake gateway tests. A live Modal or Beam run has not exercised those races.
- **A cloud run covers one page or one chapter, never a project.** A chapter run proposes the
  pages `run::plan` would walk, capped at 256 pages (`cloud_tiles::MAX_TILES`) and 16 times
  `cloud_tiles::MAX_TOTAL_PIXELS`. The grant only starts the run; the run then holds the consent
  to its last page, with no clock. A failed page is left as it was and the run goes on; a
  changed profile, cloud switched off or a refused key ends it. A project run is refused
  (`cloud_run_scope_unsupported`), and resuming a cloud run is refused
  (`cloud_run_grant_required`) because no consent survives a restart. The mock's `runFail` knob
  still refuses the whole run on its first page rather than failing one page and continuing.
- **Cloud detection prefetch and the idle GPU release are proven offline only.** A run with cloud
  stages analyzes its pages on a producer thread ahead of the cleaning
  (`run_analysis::Prefetch`, bounded at 256 MiB of answers), then sends
  `POST /mc/v1/gpu/stop` with `idle_only` for `analysis`. Order, run-ahead, per-page failure
  notices, stop, cancel, the buffer bound and the single release run against a fake gateway
  (`run_analysis` tests and `run::tests::cloud_stages`), and the gateway side against fakes
  (`deploy/cloud/tests/test_gpu.py`). Not run live: how much GPU time it actually saves on a real
  chapter, whether Modal's `AnalysisGPU` really leaves on the idle release while the heartbeat
  reads idle, how often the release races the provider's own scale-down and cold-starts a
  container (no release marker is written, so that container loads its model once), and whether
  Modal's per-second billing stops at the release or only when the container exits. Deployments
  set up before `idle_only` answer `400` and keep the old idle window until the cloud setup is
  updated. A page is now decoded twice in a cloud run (once to send, once to clean); the cost
  was not measured. Turning cloud off mid-run stops the next upload but pages already analyzed
  ahead are still cleaned with their answers.
- **The run consent shows no cost and no encoded size.** `costEstimateUsd` is always null, and
  `sourceBytes` is estimated from the source file, not the PNG tiles actually sent.
- **Cloud detection on a long strip is proven offline only.** A run sends each page once and a
  detect window that crosses a verified join reads the next page's answer
  (`run::Pipeline::gather_below`, `Prefetch::peek`), tested on the `strip-01..03` fixtures against a
  fake gateway. Not run against a live endpoint. The review (`analysis.rs`) still needs a paginated
  chapter. Region edits and drawn boxes stay on this computer whatever `analysisTargets` says.
- **The review cannot send only a selected region.** `propose_remote_analysis` takes `regions`,
  but the review has no region picker to fill it, so it always proposes the whole page.
- **Strange: Modal GPU snapshots were rebuilt on every cold start and never restored.** Open;
  come back to it. The render `Worker` snapshot is now off (`WORKER_SNAPSHOT` in
  deploy/cloud/modal/app.py), so cold starts load from the volume in about 55 s. In the run
  `Worker` set `enable_memory_snapshot=True` with `experimental_options={"enable_gpu_snapshot": True}`
  (experimental in Modal) and loads and warms the pipeline in `@modal.enter(snap=True)`;
  `AnalysisGPU` snapshots CPU memory only. Live on 2026-09-26 (L4, FLUX.2 Klein 4B, SDK 1.5.5,
  app logs from `mc-live-0926b`), Modal wrote a new GPU snapshot for every new render container:
  after the first deploy (21:24:04), after the adopt redeploy (21:28:34, expected, new code), and
  again at 21:31:40 for a plain cold start on the same deployment, which should have restored the
  21:28 snapshot. Each build was about 55 s of load and warmup plus about 1 min 45 s of snapshot
  writing, so a cold render took 171 to 186 s against 52 s without snapshots
  (docs/cloud-live-modal-2026-09-26.md). The 21:31:40 rebuild is the strange part: the
  deployment, image and class config had not changed since the 21:28 snapshot, which Modal
  reported as created and restored from, and Modal's docs name only a redeploy as the reason to
  build again. No cold start in the run restored an existing snapshot;
  the 7.2 s "cold after stop" render ran on the container the stop had not yet released. Why
  Modal rebuilt (per host, per zone, or an invalidation) is not known, and restore time and
  snapshot cost are not measured. The `SDNQ: OpenVINO MM kernels are not available ... CPU
  device` line during the snap phase is an import-time notice; renders ran on CUDA (1.3 it/s).
  The analysis CPU snapshot was written once (13 s) and its reuse was not observed either.
  To look at it again: turn the snapshot back on, deploy once, then force three cold starts
  on the same deployment (let the idle window expire between renders) and read the app logs
  for "Creating GPU memory snapshot" against "Restoring Function from memory snapshot". Ask
  Modal support whether GPU snapshots are kept per host or zone if the rebuild repeats.
- **A Modal render before seeding fails differently.** The snapshot phase raises
  `WorkerNotReady` so no snapshot freezes an unloaded pipeline. The gateway maps the startup
  error back to `weights_missing` from the pickled exception or its text, but whether Modal
  delivers it after its own startup retries, how long that takes, and whether repeated startup
  failures put the class in a crash-loop backoff that delays the first render after seeding are
  not observed. A stop's release that cold-starts a render container now fails that start
  instead of returning; the stop does not wait for it.
- **ONNX Runtime under GPU snapshots is not tried.** `AnalysisGPU` stays on CPU snapshots
  because nothing shows that ONNX Runtime CUDA sessions survive a `cuda-checkpoint` restore; its
  snapshot holds only the torch and onnxruntime imports, and whether that import time is worth
  it is not measured either.
- **Discovery of existing Modal installations ran live once, with one installation.** On
  2026-09-26 `inspect` found the live install with its recorded `install:options`, and adopting
  it applied in 17.3 s without seeding again. The time ten installations take inside the 50 s
  inspect budget, and whether `list_deployed_apps` with the resolved environment matches the
  default environment in other workspaces, are not observed. Discovery reads the
  `install:options` Dict value, which Modal unpickles, the same trust the weights step already
  gives `seed:state` in the user's own workspace.
- **A GPU stop does not take effect before the next queued render.** Live, a render sent 3 s
  after `POST /mc/v1/gpu/stop` on an idle container ran on that same container, with no new
  start in the logs; the container left before the render after it. The release call and the
  render share one input queue, so the order is Modal's.
- **Adopting an installation on another computer leaves the first computer's proxy token
  live.** The new journal does not know it, so neither Resume nor Cleanup can revoke it; the
  Review step says to remove it in Modal (Settings, Proxy Auth Tokens). Listing and revoking
  proxy tokens by app is not built.
- **Adopting an installation whose seed is still running on the other computer starts a
  second seed.** With no seed in this journal and no ready marker yet, apply pops the status
  document and spawns another `seed_weights` into the same volume. The seed is idempotent per
  file, but two concurrent downloads into one snapshot directory are not tested. Discovery
  shows such an installation as "download did not finish".
- **Beam has no discovery.** beta9 0.1.268 lists deployments with an exact name filter; an
  unfiltered or prefix listing is not verified, so Beam's `inspect` answers an empty list and a
  second Beam setup still downloads again.
- **Installations made before `install:options` list no choices.** Reusing one plans with the
  defaults; choosing a model other than the one on the volume downloads it beside the first.
  The GPU and idle time it was deployed with are not read back from the deployed app.
- **The setup guard only knows endpoints in `inference.json`.** A setup whose endpoint was
  removed with Remove (without cleanup) is not pointed to by the guard; discovery after
  Connect still lists it, and `on_this_computer` routes it to Resume while its journal exists.
- **A render sent before the weights are seeded still starts a GPU.** With snapshots the worker
  refuses to start without weights, so the job fails with `weights_missing`, but only after a GPU
  container was started to find that out. The gateway has the weights volume mounted and could
  check the ready marker before dispatching; that needs a new pre-enqueue rejection code in the
  contract and in the Rust client, and was left out because setup waits for seeding first.
- **Detect then Clean matches one pass on fixtures only.** `detect_then_clean_leaves_what_one_pass_leaves`
  (`src-tauri/src/run/tests.rs`) proves the stored mask, ink, route, thickness, deviation and
  segmentation scale rebuild the same patches as Auto on a synthetic page. It was not run on the
  real scans in `~/dev/120 noisy png`, and a clean after an engine or model update rebuilds the fit
  with the stored values, not the new detector's.
- **A deleted detection cannot be restored.** It has no pixels, and its mask files go with it, so
  `restoreRegion` and undo cannot bring it back. What undo does after `applyTool` cleans a
  detection (the patch replaces the detection in one write) was not tested.
- **A Solid pick in a Clean run now fills with the stored `balloon_color`.** The run's `bubbleColor`
  is used only for a detection with no measured colour. Tested on a synthetic flat page
  (`a_solid_pick_fills_with_its_measured_balloon_colour_before_the_runs`); whether the ring gray
  reads right on real balloons was not checked on the scans in `~/dev/120 noisy png`.
- **The one-pass guard over kept patches is tested with the stub cleaner only.** An Auto pass on an
  unexamined page skips a region at least half inside a patch the page held before the pass
  (`an_auto_pass_leaves_regions_inside_the_pages_existing_patches_alone`). The half-area test is
  the one Detect uses; how often a real detector's box drifts under half from the kept patch was
  not measured.
- **The native cloud clean has run against Modal on saved detections.** `prepare_cloud_clean`,
  `confirm_cloud_clean`, `start_cloud_clean` and `cancel_cloud_clean` are tested with a fake gateway
  (prepare), a fake renderer (the run: three in flight, failures, skips, stop, cancel, one release)
  and the fake HTTP gateway for one detection's render and commit. The packaged validation app
  committed 21 Modal jobs from five pages of saved detections. Beam, the 273-region chunk boundary,
  the gateway's queueing of three concurrent jobs and the render idle release remain unmeasured.
- **The cloud clean cost range is unmeasured.** `JOB_OVERHEAD_SECONDS` (2 s) and
  `SECONDS_PER_WORK_MEGAPIXEL` (10 s, applied to each job's own working size) are guesses, not
  timings; the cold start (55 s) and the idle window (600 s, the deployment maximum) are the high
  end. The deployed idle setting is not available before a container starts, so this bound may overstate
  the default 120 s tail. CPU and
  memory are priced at Modal's published per-second rates for the render worker's `cpu=2.0` and
  12 or 24 GiB; Beam's worker is sized differently and is priced the same. No estimate has been
  compared with a bill.
- **`totalCropPixels` is an estimate.** It sums `estimated_crop` over each region's stored hole, which
  the render now takes as it is, without refitting; the estimate is not clamped at the page edge, so
  it may overcount border regions.
- **Regions of one page render one after another.** A region waits for any region below it within
  768 px plus the crop context, because the attach refuses a result whose underlay changed. On a
  typical page that is every region, so a one-page batch uses one slot, not three. Whether the
  underlay check could be narrowed to the crop was not examined.
- **The render idle release waits on stale jobs.** The gateway counts a job as in flight until it
  is polled to a terminal state, so a job whose app stopped polling holds the release back until
  a plain GPU stop settles it; the GPU then scales down on its idle timer.
- **`applyTool` on a detection in a long strip with a box is untested.** The window is shifted to
  the strip's source and covered by code, but only the paginated case has a test.
- **The Detect / Clean interface has driven a packaged validation run.** The Text cleanup
  Mode, Detect on and Clean on controls, the Detected rows in Layers, the detected canvas outline,
  the Pages list mark and the cloud clean consent are covered by Vitest DOM tests against the mock.
  The packaged macOS validation app ran Detect and a saved-detection Clean through its interface;
  final focus, scroll and narrow-window checks in WKWebView remain open.
- **The cloud clean interface now has one packaged Modal run.** `prepare_cloud_clean`,
  `confirm_cloud_clean`, `start_cloud_clean` and `cancel_cloud_clean` are wired in
  `src/lib/api/tauri.js` to the contract names in `docs/detect-clean.md`; the packaged validation
  app drove the saved-detection Clean path for 21 regions. The
  mock's mixed-execution check (every second Fill or Solid detection accepted, after the start), its
  cost range (8 s per region, 30 s cold start, 120 s idle tail, an L4 at $0.80 an hour, shown as
  0.8x to 1.5x) and its GPU name are stand-ins, not measurements.
- **Detect and clean with Clean on the cloud continues only while the window is open.** The cloud
  half is asked for when the detect run's `run-finished` arrives. Quitting before that leaves the
  regions detected with no prompt on the next launch; Mode set to Clean picks them up.
- **Clean on the cloud over a long strip is proven offline only.** The whole strip is one space for
  `next_ready`, so a region waits for any earlier region within reach, across a join too. Tested
  with a fake gateway (`a_long_strip_is_planned_and_cleaned_across_its_join`), not against a live
  endpoint. A crop near a join may reach past the page-clipped size the estimate counts.
- **Text cleanup's picks now decide every clean.** A Clean run and a cloud clean start each region
  from the panel's picks, not the pick saved at detection (`run::repick_detections`). A Layers
  row or the region menu still uses the saved pick. Not checked in a packaged app.
- **The `apply_tool(autoClean, region_id)` IPC command was not invoked in a Tauri window.**
  Native tests cover its region versus page dispatch, the stored detection edit, its `applied`
  response conversion, and preservation of the other detection.
- **Clean was not run on a host lacking ONNX Runtime.** A unit test covers the mode gate and a
  model-free Clean pipeline covers the lower rungs; an actual missing-runtime installation was
  not available for an end-to-end command test.
- **The grant race was tested at the shared run-slot boundary.** Reservation, busy response and
  release on a pre-start error are tested; a concurrent native `start_cloud_clean` with a real
  confirmed grant was not exercised.
- **The detection orphan sweep has one two-process test.** It removes unreferenced mask and ink
  files on open, only when the opener holds or can take the job lock, and
  `a_reader_in_another_process_does_not_sweep_unflushed_detection_masks`
  (`src-tauri/src/run/tests.rs`) runs the writer as a second process; two independent app
  processes using the same sidecar were not exercised.
- **One parallel Tauri test run saw two cloud renders in flight where its test expected three.**
  `three_renders_in_flight_on_different_pages_and_one_release` passed on the required rerun;
  the cause of that intermittent result was not measured here.

Left open by the cloud usage fixes of 2026-09-27 (setup token reuse, keychain prompts, per-project
consent, held-candidate approval, LaMa routing):

- **LaMa on CUDA and DirectML was not run.** A forced session on either now keeps the CPU fallback
  (`accel::LAMA.partitioned_on`), but only WebGPU was built and run on the real graph, on the Mac.
  CoreML is no longer offered for LaMa: 3.9 s per region and an 8.4 GB peak.
- **Candidates held before the lettering sidecar existed still approve through the box.** Their rows
  carry no mask, so Approve falls back to `text_under`; detecting the page again stores one.
- **A LaMa region edited between the revision check and the local pass taking the job lock is still
  cleaned.** Closing it means passing the expected revisions into `run::clean_picks_here`.
- **No standing project consent can be revoked from the interface.** It clears when cloud engines are
  turned off or the endpoint's profile is removed; there is no project settings screen yet.
- **None of these fixes was driven in the real app.** Evidence is unit, DOM and mock tests only.
- **Changing a detection's text type has no undo and is not offered on held candidates.** The region
  menu's Text type section (`set_detection_type`) flips a stored detection only. Detection mask edits
  have no undo either, and a held candidate row has no stable id to address. Moving a detection
  outside turns a stored `fill` or `solid` pick into `lama` and clears its measured bubble tone;
  moving it back keeps `lama` and does not re-measure the tone. A Clean with its own picks follows
  the flag regardless. Only the Vite mock was driven; the native command has unit tests only.

Left open by the saved Modal setup token (2026-09-30):

- **Not driven in the real app.** Evidence is Rust unit tests and the provisioner DOM tests; no real
  Update of `mc-d0dhwx` ran with a saved token.
- **Setups made before this change have no saved token.** The first Update after it still asks once.
- **A saved token revoked in Modal is only noticed on use.** The fields come back after the refusal;
  nothing checks the token ahead of time.
- **Each ad hoc rebuild adds a third keychain prompt** (the setup token) beside the two runtime ones.
