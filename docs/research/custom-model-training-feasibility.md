# Training a custom mid-size manga inpainting model on $600 of OCI credits

This note assesses whether a custom mid-size manga inpainting model could be trained on a fixed budget of cloud credits. It was written on 2026-08-30. Every figure below is traced to a primary source and
the URL is given inline. Where a number is derived rather than published, the
derivation is shown and the assumption is named.

## 0. The question

The engine ladder (see [features.md](../features.md#the-engine-ladder)) has a gap. Rung 2 is
`dreMaz/AnimeMangaInpainting`, a 51M-parameter big-LaMa finetune that ships as a
207 MB ONNX file and clears a 512² region in 0.63 s on WebGPU. Rung 3a is
FLUX.2 Klein 4B, 4.6 GB of weights in an out-of-process Python sidecar, about
21 s per region at 512². Between a 200 MB model that is fast and a 4.6 GB model
that is slow there is an obvious empty rung: something in the 1.5–3 GB class,
three to four times rung 2's footprint, trained specifically on manga.

The question is whether $600 of Oracle Cloud GPU credits can produce that model.

The short answer is no, and the interesting part is *how far* off it is — the
gap is not 20%, it is between one and two orders of magnitude, and there is a
second, entirely separate blocker sitting in front of the money.

---

## 1. The gate: can the credits even reach a GPU?

This has to be settled first, because it can make the rest of the document moot.

Oracle's own compute service-limits table gives, for every GPU family, the same
value in both the *Trial* and the *Pay-As-You-Go* columns — not a number, but a
link to support:

> "Total GPUs for instances that are created using shapes in the VM.GPU.A10 and
> BM.GPU.A10 series | gpu-a10-count | Contact Us | Contact Us"

and identically for `gpu-a100-v2-count`
(https://docs.oracle.com/en-us/iaas/Content/General/service-limits/default.htm,
and the same rows on
https://docs.oracle.com/en-us/iaas/Content/General/Concepts/servicelimits.htm).
There is no default GPU allocation. A newly created tenancy has none.

Oracle's Data Science documentation states the consequence in plain words:

> "If you're not an enterprise customer, you must create a service limit
> increase to use GPU because by default, your tenancy has zero limit."
> — https://docs.oracle.com/en-us/iaas/Content/data-science/using/gpu-using.htm

The same page separates two failure modes that are easy to conflate:

> "Service limit isn't the same as the shape capacity. The Data Science GPU
> limit is the maximum number of GPU your tenancy can use if the shape is
> available. If all the shapes in the region are in use, you might receive an
> out-of-capacity error even if you have Data Science limit."

So there are two gates, not one: the tenancy limit (a support ticket, granted at
Oracle's discretion) and regional capacity (not guaranteed even after the limit
is granted). The same page adds that "A10 capacity reservations can't be
accepted, and A100 capacity reservations can only be accepted in certain
regions" — meaning the standard mechanism for *guaranteeing* you can start the
instance you budgeted for is unavailable on exactly the cheapest shape.

### 1.1 The trial clock

Oracle's Free Tier documentation is unambiguous about the standard offer:

> "The Free Trial provides you with $300 of cloud credits that are valid for up
> to 30 days."
> — https://docs.oracle.com/en-us/iaas/Content/FreeTier/freetier.htm

and

> "Paid resources that were provisioned with your credits during your free trial
> are reclaimed by Oracle unless you upgrade your account."

$600 is not the standard trial grant. It is either two trials, a promotional or
startup-program grant, or credits on an upgraded (paid) account. This matters,
because the three cases behave very differently:

- **If it is trial credit**, the 30-day expiry is a hard wall. 300 A10-hours
  (see §2) inside 30 days means running one GPU roughly ten hours a day, every
  day, from the moment the limit increase is granted — and the limit-increase
  ticket itself consumes some of those days.
- **If the account has been upgraded to Pay-As-You-Go**, the credits generally
  do not expire on the trial clock, but the service-limit row above shows PAYG
  is *also* "Contact Us" for GPUs. Community reports converge on upgrading to a
  paid account as the practical precondition for GPU shapes appearing in the
  shape list at all; Oracle's own text ("if you're not an enterprise customer")
  points the same way.
- **If it is an Oracle for Startups / promotional grant**, terms are per-program
  and must be read from the grant email, not from the public Free Tier page.

**Recommendation before spending any further planning effort:** open the OCI
Console, go to *Governance & Administration → Tenancy Management → Limits,
Quotas and Usage*, select service *Compute*, and read the current value of
`gpu-a10-count` for the home region. If it is 0, file the limit-increase request
*first* and treat everything downstream as blocked until it is granted. The
answer to "is this project possible at all" lives in that one number.

### 1.2 A billing trap worth naming

Oracle documents that stopping a bare-metal instance does not stop the bill:

> billing continues for stopped bare metal instances because the NVMe storage
> resources are preserved; to halt billing you must terminate the instance
> — https://docs.oracle.com/en-us/iaas/Content/Compute/Tasks/resource-billing-stopped-instances.htm

and that "shutting down an instance using the instance's OS does not stop
billing for that instance." Every A100 and L40S shape on OCI is bare metal
(§2.1). A forgotten `BM.GPU.A100-v2.8` burns the entire $600 in 18.75 hours
whether or not it is training. On a fixed-credit budget this is the single most
likely way to lose the money without producing a model.

---

## 2. What $600 buys on OCI

### 2.1 List prices, from Oracle's own price-list API

Oracle's public price-list service
(`https://apexapps.oracle.com/pls/apex/cetools/api/v1/products/?currencyCode=USD`,
retrieved 2026-08-30, `lastUpdated` 2026-08-14) returns these Pay-As-You-Go
rates, all metered per **GPU** per hour:

| Part number | Product | USD / GPU-hour |
| --- | --- | --- |
| B95909 | Compute – GPU – A10 | 2.00 |
| B109479 | Compute – GPU – L40S | 3.50 |
| B95907 | Compute – GPU – A100 – v2 | 4.00 |
| B98415 | OCI – Compute – GPU – H100 | 10.00 |
| B110519 | OCI – Compute – GPU – H200 | 10.00 |
| B112613 | OCI – Compute – GPU – RTX PRO 6000 | 4.50 |
| B110978 | OCI – Compute – GPU – B200 | 14.00 |

The shape catalogue
(https://docs.oracle.com/en-us/iaas/Content/Compute/References/computeshapes.htm)
determines how many GPUs you are forced to rent at once:

| Shape | GPUs | GPU memory | OCPU / RAM | Form |
| --- | --- | --- | --- | --- |
| VM.GPU.A10.1 | 1 × A10 | 24 GB | 15 / 240 GB | VM |
| VM.GPU.A10.2 | 2 × A10 | 48 GB | 30 / 480 GB | VM |
| BM.GPU.A10.4 | 4 × A10 | 96 GB | 64 / 1024 GB | bare metal |
| BM.GPU.L40S.4 | 4 × L40S | 192 GB | 112 / 1024 GB | bare metal |
| BM.GPU.A100-v2.8 | 8 × A100 80 GB | 640 GB | 128 / 2048 GB | bare metal |

**A100 and L40S have no VM shape on OCI.** The smallest A100 unit you can rent
is eight of them; the smallest L40S unit is four. That converts the per-GPU
price into a much less forgiving per-hour price.

### 2.2 The budget in hours

| Shape | $/hour (node) | Wall-clock hours for $600 | GPU-hours for $600 |
| --- | --- | --- | --- |
| VM.GPU.A10.1 | 2.00 | **300** | 300 |
| VM.GPU.A10.2 | 4.00 | 150 | 300 |
| BM.GPU.A10.4 | 8.00 | 75 | 300 |
| BM.GPU.L40S.4 | 14.00 | 42.9 | 171 |
| BM.GPU.A100-v2.8 | 32.00 | 18.75 | 150 |
| BM.GPU.H100.8 | 80.00 | 7.5 | 60 |

Storage is a rounding error at this scale: block volume and object storage are
both $0.0255 per GB-month (parts B91961 and B91628), so a 300 GB training corpus
costs about $7.65 a month, and outbound transfer from North America or Europe is
$0.0085/GB after a free allowance (part B88327). Budget ~$20 of the $600 for
storage and egress and ignore it thereafter.

**The headline number: $600 of OCI credit is 300 A10-hours, or 150 A100-hours.**

### 2.3 Context: the same $600 elsewhere

The credits are OCI-locked, so this row is context only — but it establishes how
much model the money would have bought if it were cash.

| GPU | OCI list | RunPod Community | RunPod Secure | Lambda |
| --- | --- | --- | --- | --- |
| A100 80 GB | $4.00 | $1.19 (PCIe) | $1.39 (PCIe) | $2.79 (8×SXM) |
| L40S | $3.50 | $0.79 | $0.99 | — |
| H100 | $10.00 | $1.99 (PCIe) | $2.89 (PCIe) | $3.29 (1×PCIe) |
| A10 | $2.00 | — | — | $1.29 |

Sources: https://www.runpod.io/pricing and https://lambda.ai/service/gpu-cloud
(both retrieved 2026-08-30).

OCI list price is roughly **3.4× RunPod's A100 rate and 4.4× its L40S rate**. In
effective compute the $600 of OCI credit is worth about $140–$180 of RunPod
Community time. That is not an argument against using the credits — free money
is free money — but it is an argument against letting the credits define the
plan. Any option that is marginal at OCI list price is comfortable elsewhere,
and any option that is out of reach at OCI list price is *still* out of reach at
one-quarter the price if the gap is 50×.

---

## 3. What training actually costs

### 3.1 Big-LaMa, the model we already ship

The LaMa paper's implementation section
(https://ar5iv.labs.arxiv.org/html/2109.07161, arXiv:2109.07161, WACV 2022)
gives the numbers directly:

- LaMa-Fourier: **27M parameters**, ResNet-like, "trained for 1M iterations with
  a batch size of 30" at 256×256.
- Big LaMa-Fourier: **51M parameters**, 18 FFC residual blocks, batch size 120,
  trained on 256×256 crops of ~512×512 images drawn from "a subset of 4.5M
  images from Places-Challenge", and — the load-bearing sentence — **"trained on
  eight NVidia V100 GPUs for approximately 240 hours."**

That is **1,920 V100-GPU-hours** for the model rung 2 is a finetune of.

### 3.2 Translating V100-hours to OCI-hours

Per NVIDIA's own datasheets:

| GPU | FP16 Tensor TFLOPS (dense) | Memory | Bandwidth |
| --- | --- | --- | --- |
| V100 SXM2 | 125 | 32 GB HBM2 | 900 GB/s |
| A10 | 125 (https://www.nvidia.com/en-us/data-center/products/a10-gpu/) | 24 GB GDDR6 | 600 GB/s |
| A100 80 GB SXM | 312 (https://www.nvidia.com/en-us/data-center/a100/) | 80 GB HBM2e | 2,039 GB/s |
| L40S | 362 (https://www.nvidia.com/en-us/data-center/l40s/) | 48 GB GDDR6 | 864 GB/s |

An A10 has the same dense FP16 tensor throughput as a V100 and two-thirds of its
bandwidth. LaMa's fast Fourier convolutions are FFT- and bandwidth-heavy rather
than pure GEMM, so the honest conversion is **A10 ≈ 0.7–1.0 × V100** for this
workload, **A100 ≈ 2.0–2.5 ×**, **L40S ≈ 2.0–2.9 ×** (its FLOP advantage is
throttled by GDDR6 bandwidth relative to the A100's HBM).

Applying that:

| Option | GPU-hours needed | OCI cost at list | Multiple of $600 |
| --- | --- | --- | --- |
| Reproduce big-LaMa on A10 | 1,920–2,700 | $3,840–$5,500 | **6.4–9.2×** |
| Reproduce big-LaMa on A100 | 770–960 | $3,080–$3,840 | **5.1–6.4×** |
| Reproduce big-LaMa on L40S | 660–960 | $2,300–$3,360 | **3.8–5.6×** |

**Re-training the 51M model we already have, from scratch, costs four to nine
times the budget.** The target model is supposed to be seven to fifteen times
larger than that.

### 3.3 The finetune, which is the affordable operation

The manga specialisation we ship is `dreMaz/AnimeMangaInpainting`
(https://huggingface.co/dreMaz/AnimeMangaInpainting): a single
`lama_large_512px.ckpt`, MIT-licensed, described as big-LaMa "finetuned on
300,000 manga and anime style images". The card publishes no GPU count, step
count, or wall-clock time — I checked the model card, the file listing via the
HF API, and the linked upstream repo, and none of them record the training run.
So the finetune cost has to be derived.

Derivation, with its assumption stated plainly: **if** big-LaMa also ran 1M
iterations (the paper states this for the other models and says big-LaMa differs
only in "the depth of the generator; the training dataset; and the size of the
batch"), then it saw 1M × 120 = 120M sample-presentations in 1,920 GPU-hours,
which is ~62,500 samples per GPU-hour at 256². Scaling by pixel count, 512²
training runs at roughly ~15,600 samples per GPU-hour.

Against that yardstick, 300 A10-hours buys:

- ~18.7M sample-presentations at 256² — about **62 epochs over a 300k-image
  corpus**, or 18 epochs over 1M crops;
- ~4.7M sample-presentations at 512² — about **15 epochs over 300k images**, or
  4.7 epochs over 1M crops.

Either is a real finetune. A 10–15% -of-pretrain compute budget is the
conventional range for domain adaptation, and 300/1920 = **15.6%** sits exactly
there. This is the one thing on the whole list that the budget comfortably
covers.

### 3.4 What the 1.5–3 GB class costs

Convert the size target to parameters. At fp32, 1.5 GB is 375M parameters and
3 GB is 750M; at fp16, the same disk budget is 750M–1.5B parameters. Against
big-LaMa's 51M, the target is **7×–15× the parameter count** (fp32) or
15×–30× (fp16).

For convolutional/GAN generators trained on a fixed dataset, compute scales
roughly linearly in parameters at fixed step count, and a larger model generally
needs *more* steps, not fewer. A from-scratch train of a 375–750M-parameter
inpainter is therefore **13,400–28,800 V100-equivalent hours**, i.e. roughly
**$27,000–$58,000 at OCI A10 list price, or $21,000–$45,000 on L40S** — 35× to
95× the budget. Even at RunPod Community rates it is $10,000–$23,000.

Two published points bracket this and confirm the order of magnitude:

- The **LDM inpainting model** (Rombach et al., arXiv:2112.10752,
  https://ar5iv.labs.arxiv.org/html/2112.10752) is **387M parameters** — the
  bottom edge of the target band, almost exactly 1.5 GB in fp32. The paper notes
  that pixel-space diffusion models of that era required "150–1000 V100 days"
  (i.e. 3,600–24,000 V100-hours) to train, with latent models cheaper but still
  in the hundreds of V100-days.
- **Stable Diffusion 1.5 inpainting**
  (https://huggingface.co/stable-diffusion-v1-5/stable-diffusion-inpainting) was
  trained on "32 × 8 × A100 GPUs" for approximately **150,000 GPU-hours**. At
  OCI's $4/GPU-hour that is **$600,000** — one thousand times the budget.

There is no reading of these numbers under which $600 trains a 1.5–3 GB
inpainter from scratch.

### 3.5 What a *finetune of an existing* large model costs

This is a different and much cheaper operation, and it is the only route into
the 1.5–3 GB band that the budget can reach. Published anchors:

- **LCM-LoRA** distillation of a pretrained Stable Diffusion: "4,000 training
  steps (~32 A100 GPU hours)"
  (https://huggingface.co/docs/diffusers/main/en/using-diffusers/inference_with_lcm_lora)
  — **$128 at OCI list**.
- Full-parameter finetunes of SD-class models on curated 10k–100k-image sets are
  routinely reported in the tens to low hundreds of A100-hours, i.e. $100–$800
  at OCI list.

So: taking SD 1.5-inpaint or the LDM inpainter and adapting it to manga is
**inside the budget**. Building anything at that scale is not.

---

## 4. Architecture candidates at the 0.5–3 GB scale

Screened on the three constraints that actually bind this app: a real mask
channel, ONNX-exportability for in-process ONNX Runtime, and a licence that
survives shipping.

| Model | Params | Code / weight licence | Mask channel | ONNX path | Manga evidence |
| --- | --- | --- | --- | --- | --- |
| **big-LaMa** (advimman) | 51M | Apache-2.0 (LICENSE file, verbatim) | yes, 4-channel | yes, but see §4.1 | via the finetune below |
| **dreMaz AnimeMangaInpainting** | 51M / 204 MB | **MIT** (HF card `license:mit`) | yes | shipped: `mayocream/lama-manga-onnx`, 207 MB, opset 17 | trained on ~300k manga/anime images |
| **MAT** (CVPR 2022) | ~60M | **"The code and models in this repo are for research purposes only"** (README), built on StyleGAN2-ADA | yes | StyleGAN2 custom CUDA ops; hostile to export | none |
| **MI-GAN** (ICCV 2023) | mobile-scale | **MIT**, Picsart AI Research (LICENSE verbatim) | yes | first-class — `scripts/create_onnx_pipeline.py`, pre-converted 512² ONNX on HF | none |
| **ZITS / ZITS++** | ~68M + prior nets | Apache-2.0 (ZITS_inpainting LICENSE); ZITS++ HR-Flickr test set is non-commercial | yes | multi-stage (line/edge predictors + transformer) — several graphs to export and stitch | none |
| **FcF** (WACV 2023) | StyleGAN2-scale | "Apache License Version 2.0 except for the third-party components" — those include NVIDIA's StyleGAN2-ADA | yes | same custom-op problem as MAT | none |
| **LDM inpainting** (LDM-4 big) | **387M** | **MIT** (CompVis/latent-diffusion LICENSE) | yes | VAE + UNet export cleanly; iterative sampling | none |
| **SD 1.5 inpaint** | ~1.06B total | CreativeML **OpenRAIL-M** — a use-restriction licence, not a permissive one | yes: "5 additional input channels (4 for the encoded masked-image and 1 for the mask itself)" | widely exported to ONNX | anime SD finetunes are abundant |
| **PowerPaint** (ECCV 2024) | SD1.5-scale | MIT (LICENSE verbatim) — but inherits SD's weight licence | yes | SD-class | none |
| **BrushNet** (ECCV 2024) | SD-scale + control branch | Tencent's own licence, **not** Apache | yes | SD-class + extra branch | none |
| **MangaInpainting** (SIGGRAPH 2021) | — | custom CUHK licence: grants "academic, research and commercial purposes, without fee", but commercial use requires a **written notice to the author** | yes | multi-stage with ScreenVAE; awkward | **manga-native, screentone-aware** |
| **FLUX.2 Klein 4B** | 4B | Apache-2.0 | via inpaint pipeline | no — sidecar only | already shipped as rung 3a |

### 4.1 The DFT constraint is real and it does not go away by scaling

LaMa's fast Fourier convolutions need the ONNX `DFT` operator. Checking ONNX
Runtime's own kernel table
(https://github.com/microsoft/onnxruntime/blob/main/docs/OperatorKernels.md,
`main`, retrieved 2026-08-30), `DFT` appears in exactly two provider sections:
**CPUExecutionProvider** (opset 20+, float/double) and
**DmlExecutionProvider** (opset 20+, float/double/float16). It does **not**
appear under CUDAExecutionProvider. So a LaMa ONNX graph cannot run on CUDA
without a CPU fallback for those nodes; this is confirmed at the source table.

The consequence for this project: **any scaled-up LaMa inherits the same
execution-provider restriction.** A 400M-parameter FFC network would still be
confined to CPU, DirectML and WebGPU — and on CPU, an 8× parameter increase over
a model that already takes 1.5 s would be catastrophic. Scaling LaMa makes the
worst-case path worse in proportion. That alone disqualifies "wider/deeper FFC"
as the architecture for a mid-size rung, independent of training cost.

### 4.2 The models that actually occupy the 1.5–3 GB band are latent diffusion

This is the structural finding of the survey. Nothing in the GAN/feed-forward
family lives at 375–750M parameters — MAT, MI-GAN, ZITS and FcF are all in the
tens of millions, because that family stopped scaling. The occupants of the
target size band are latent-diffusion inpainters: LDM-4 at 387M (≈1.55 GB fp32),
SD 1.5-inpaint at ~1.06B (≈2.1 GB fp16). Which means **"a model 3–4× rung 2's
footprint" is not a bigger LaMa; it is a small diffusion model** — and a small
diffusion model is a slower, iterative-sampling engine that behaves much more
like rung 3a than like rung 2. The rung being imagined may not exist as a
category: the gap between 200 MB/0.6 s and 4.6 GB/21 s is not mostly a *size*
gap, it is the gap between one-shot convolution and iterative denoising.

---

## 5. Data

### 5.1 Manga109 — usable, but small and encumbered

From the dataset's terms (http://www.manga109.org/en/download.html, retrieved
2026-08-30):

The base **Manga109** (109 volumes, 21,142 pages) is academic-only:

> "The dataset is to be used for academic purposes by non-commercial
> organizations."
> "Redistribution of any part of the dataset to third parties is forbidden."

**Manga109-s** is the commercially usable subset — 87 of the 109 volumes —
and explicitly permits what this project would need:

> "Using the Manga109-s dataset for experiments for machine learning or image
> processing."
> "Using results, or portions of results, obtained from machine learning
> experiments or image processing experiments, for commercial use."

subject to conditions that are all satisfiable but must be honoured:

> "Redistribution of the Manga109-s dataset to third parties is forbidden."
> "When publishing results (including pre-trained models) obtained from machine
> learning experiments or image processing experiments, the use of the
> Manga109-s dataset must be indicated clearly within the published work."
> "Selling manga images within the dataset together with results obtained from
> machine learning or image processing experiments is forbidden."
> "Direct copies or modifications of the manga images within the Manga109-s
> dataset must not be treated as products."

Practical reading: **a model trained on Manga109-s can be shipped commercially,
but the shipped model must carry a visible Manga109-s attribution**, the corpus
itself must never be redistributed (so it cannot go into a public training-data
release), and no sample page from the dataset may be shipped as a demo asset or
fixture. Access is by request through the Manga109-s form, not open download.

The scale problem: 87 volumes is on the order of **17,000 pages**. Against the
~300k images the current rung-2 finetune used, Manga109-s alone is ~6% of the
corpus. It is a superb *evaluation* set and a decent seed, not a training corpus.

### 5.2 Danbooru and the anime web corpora

The Danbooru2021 release (https://gwern.net/danbooru2021) is the corpus the
anime/manga ML ecosystem was built on — 4.9M images, 162M tags. Two problems:
Gwern has taken the release offline over metadata/file inconsistencies, and the
underlying images are third-party copyrighted works aggregated without a
licence grant. The page itself notes the tag data is copyrighted and only
Danbooru and its taggers can license it. For a shipped commercial desktop app
this is a materially different risk posture from Manga109-s, which has explicit
author permission.

Notably, this is what the ecosystem actually did: the synthetic-text pipeline
behind manga OCR/inpainting work used Danbooru2019 anime/manga images with
text-containing images filtered out, then composited randomly generated
Japanese text in non-overlapping rectangles to manufacture paired training data.
It is effective and it is what `dreMaz`'s 300k-image finetune almost certainly
rests on — which is worth noting, because it means **rung 2's provenance is
already less clean than its MIT tag suggests**, and a new model trained on the
same sources would inherit the same exposure rather than reduce it.

### 5.3 The synthetic approach, which is the right one

The training pair a text-removal inpainter needs is (clean page, page with text,
mask). That pair is *manufacturable*: take clean art, render text into it with
real Japanese fonts inside speech-balloon-shaped regions, and the mask is known
exactly because you drew it. This is what the manga-translation ecosystem does
(https://github.com/zyddnys/manga-image-translator, whose default inpainter is
`lama_large` with `lama_mpe` as the alternative), and it sidesteps the hardest
data problem — you never need matched before/after scans.

For this project the synthetic pipeline could be assembled from:

- Manga109-s pages with existing text regions inpainted out or cropped around,
  as the clean-art base (17k pages, licence-clean, attribution required);
- public-domain and Creative-Commons manga/comics (early Tezuka-era works out of
  copyright in some jurisdictions, CC-licensed webcomics, `-nc`-free Pixiv/
  ArtStation subsets) — this is the part that requires real curation work;
- the app's own screentone/halftone synthesis, since the failure mode rung 2 is
  weakest on is screentone continuation, and screentone is *procedurally
  generable* at unlimited volume with perfect ground truth.

Assembling 300k–1M licence-clean crops is feasible but is **weeks of data
engineering, not a weekend** — and it is the part of this project with no
shortcut and no GPU-credit substitute. It is also, notably, the part that would
improve rung 2 regardless of whether any new architecture is ever trained.

---

## 6. Does scale even help?

The evidence says: yes, but not on the axis this app cares about most, and not
without paying in latency.

The LDM paper's inpainting comparison (Table 7, arXiv:2112.10752) puts the 387M
LDM-4 at **FID 9.39 against LaMa's 12.0** on 40–50%-masked Places images, and
reports that human subjects preferred the LDM outputs — *while noting LDM's
LPIPS was slightly worse*. That split is the whole story. FID rewards
plausible, sharp, well-distributed texture. LPIPS rewards fidelity to what was
actually there. LaMa wins LPIPS and loses FID because it produces the *correct*
answer slightly blurred, and diffusion produces a *convincing* answer that is
slightly wrong.

For manga text removal, LPIPS is the metric that matters. The right answer for a
cleaned speech balloon is usually the continuation of a screentone gradient or a
hatching pattern that genuinely exists in the surrounding art — not a plausible
invention. Rung 2's blur is a real defect, but it is a defect of *degree* on the
right answer; a diffusion model's confident hallucination of a wrong screentone
frequency is a defect of *kind*, and it is the failure mode users notice.

This is also why the app's ladder is shaped the way it is: rung 3a exists
precisely for the minority of regions where invention is the right call, and it
is gated behind an explicit user choice and a 21-second wait. Building a middle
rung that hallucinates *by default* would move that behaviour into the common
path.

Set against that, the same evidence shows where big-LaMa's remaining headroom
is: LaMa's weakness is "large, non-homogeneous areas", and CM-GAN
(arXiv:2203.11947) reports cutting FID from 3.864 to 1.628 versus LaMa by
attacking exactly that blurriness — with an architecture in the same size class,
not a larger one. The gains at 512² on structured line art have come from better
losses, better masks and better data, not from parameter count.

**Conclusion for this project: the marginal value of the 20th million parameter
is far below the marginal value of the 20th thousand well-curated manga crop.**

---

## 7. Verdict

### 7.1 The feasibility maths, in one table

| Option | GPU-hours needed | OCI cost at list | Fits $600? |
| --- | --- | --- | --- |
| **Finetune big-LaMa (51M) on a curated manga corpus** | ~150–300 A10-h | $300–$600 | **Yes** — 15.6% of the original pretrain budget, the standard domain-adaptation ratio |
| LoRA / light finetune of SD1.5-inpaint or LDM-4 on manga | ~32–150 A100-h | $128–$600 | **Yes**, but lands on rung 3a's behaviour, not rung 2's |
| Reproduce big-LaMa from scratch | 660–2,700 | $2,300–$5,500 | No — 4–9× over |
| Train a 375–750M inpainter from scratch | 13,400–28,800 V100-eq | $21,000–$58,000 | No — 35–95× over |
| Train an SD-class inpainter from scratch | 150,000 A100-h | $600,000 | No — 1,000× over |

### 7.2 Options, ranked

**1. Finetune the existing 51M big-LaMa harder, on better data.** This is the
only option where the budget, the architecture constraints, the licence position
and the app's quality axis all point the same direction. It keeps the 207 MB
ONNX, the 0.63 s latency, the opset-17 export and the existing `lama.rs`
plumbing untouched — the deliverable is a new `.ckpt` through the same
conversion path. The realistic win is on rung 2's known weak spot (screentone
and halftone continuation under large masks), attacked with procedurally
generated screentone training pairs where the ground truth is exact. $600 is
*enough* for this, not marginal for it.

**2. Do nothing on training; spend the effort on data and evaluation.** A
measured, versioned manga-inpainting eval set — built from Manga109-s pages with
synthetic masks — costs zero GPU-hours and is a prerequisite for option 1
anyway. Without it there is no way to know whether a finetune helped. This
should happen first regardless of which option is chosen.

**3. Finetune a pretrained latent inpainter (LDM-4 387M, MIT) on manga.** The
only route that actually reaches the 1.5–3 GB band inside the budget, and the
licence is clean (CompVis MIT) unlike the OpenRAIL-M route through SD. But it
produces an
iterative-sampling engine whose latency profile belongs next to rung 3a, and
§6 argues its hallucination behaviour is wrong for the default path. Worth a
spike only if rung 3a's 21 s proves to be the blocker and a 2–4 s diffusion
middle rung would change the product.

**4. Train anything from scratch.** Out of reach by 4× at the *current* model
size and by 35–95× at the target size. Not a budget question that more careful
scheduling solves.

### 7.3 What $600 realistically buys

**300 A10-hours, or 150 A100-hours — about 15% of the compute that produced the
51M model already shipping.** That is one good finetune with room for two or
three failed attempts, provided the data is ready before the clock starts. It is
not a new model, at any size.

### 7.4 Open risks

- **The GPU quota gate is the binding constraint, not the money.** OCI ships
  every GPU family at a zero default limit for both Trial and Pay-As-You-Go
  tenancies, and Oracle's own docs say non-enterprise customers must file a
  limit increase. If that request is declined, or granted only in a region
  without A10 capacity — and A10 capacity reservations "can't be accepted" —
  the $600 buys nothing. **Verify `gpu-a10-count` in the Console before any
  further planning.**
- **The 30-day trial clock**, if these are trial credits, requires ~10 GPU-hours
  a day every day, starting after the limit-increase ticket clears. Confirm from
  the grant terms whether the credits are trial (30 days) or PAYG/promotional.
- **Bare-metal instances bill while stopped.** A forgotten
  `BM.GPU.A100-v2.8` consumes the entire budget in 18.75 hours. Prefer
  `VM.GPU.A10.1`, which is also the cheapest per GPU-hour, and terminate rather
  than stop.
- **Manga109-s attribution is a shipping obligation, not a footnote.** Its terms
  require that use of the dataset "be indicated clearly within the published
  work" for published pre-trained models. If a Manga109-s-trained model ships,
  the attribution must land in the about box and the model card, and this needs
  an entry in the About section and the [model rights decision](../model-rights-decision.md) before the first training run,
  not after.
- **Provenance of any non-Manga109 corpus is unresolved.** Danbooru-derived data
  is what the ecosystem uses and what rung 2's finetune probably rests on, but
  it carries no licence grant. Training a *new* model on it would be a
  deliberate act by this project rather than an inherited third-party position —
  a meaningfully worse posture than shipping someone else's MIT-tagged weights.
- **The 1.5–3 GB band may be the wrong target.** Nothing in the feed-forward
  family lives there, and the things that do are diffusion models whose latency
  and hallucination behaviour put them next to rung 3a rather than between the
  rungs. The premise that a "mid-size" rung exists should be re-examined before
  any budget is spent chasing it.
