# Documentation

An index of the Manga Cleaner docs, grouped by who needs them. For an overview of the project, start at the [main README](../README.md).

## Using the app

| Document | Answers |
|---|---|
| [features.md](features.md) | What does the app do, tool by tool? Covers the library, editor, Text cleanup, the engine ladder, manual tools, layers, long strips, export, Settings, cloud setup and the default shortcuts. |

## How it works

| Document | Answers |
|---|---|
| [architecture.md](architecture.md) | How is the app put together: the Rust core, the Tauri shell, the Svelte interface, and the out-of-process helpers? |
| [detect-clean.md](detect-clean.md) | How do Detect, Clean and Text cleanup work as separate steps: stored regions, commands, the cloud clean and the interface? |
| [component-write-contract.md](component-write-contract.md) | Which hosts and sources may write a SAM-TS-L component into a page, what does an approval bind, and how do writes persist? |

## Cloud GPU

The optional cloud GPU runs on your own Modal or Beam account. These documents are for people changing or reviewing that path.

| Document | Answers |
|---|---|
| [cloud-contract.md](cloud-contract.md) | What is the `/mc/v1` HTTP contract between the app and a cloud gateway: endpoints, capability handshake, request digest, and uncertain submissions? |
| [cloud-security.md](cloud-security.md) | What is the threat model: crop-only transmission, credentials, profiles, policy and grants? |
| [cloud-consent.md](cloud-consent.md) | How does a cloud render become a one-time, scope-bound grant, and what stops anything leaving the machine without one? |
| [cloud-transport.md](cloud-transport.md) | How does the app send requests and decode results within bounded limits? |
| [cloud-journal.md](cloud-journal.md) | How is every cloud render attempt recorded, cached and recovered, so every attempt ends? |
| [cloud-provisioning.md](cloud-provisioning.md) | How does the setup helper turn a pasted token or API key into a working endpoint, and what is not verified live? |
| [cloud-frontend.md](cloud-frontend.md) | Which calls, modules and flows make up the Svelte side of cloud rendering and setup? |
| [cloud-configuration-memory.md](cloud-configuration-memory.md) | How are each deployment's cleaning and denoise capabilities saved and restored? |
| [cloud-verification.md](cloud-verification.md) | What was the early test baseline and which architecture decisions came out of it? A phase record. |

## Models and measurements

| Document | Answers |
|---|---|
| [model-rights-decision.md](model-rights-decision.md) | Which detection models may the desktop app import, download or bundle, given their published terms? |
| [model-workflow-benchmarks.md](model-workflow-benchmarks.md) | What decisions and benchmarks sit behind Text-shaped review: SAM-TS-L export, RT-DETR variants, chapter runs and the limits of the supervised write? |
| [provider-qualification.md](provider-qualification.md) | Which ONNX Runtime providers have been measured for SAM-TS-L, on what hardware? |
| [findings.md](findings.md) | What was learned by measuring: hardware, engines, memory, colour, packaging, and a record of what has not been measured? |
| [research/cjk-auto-cleaning.md](research/cjk-auto-cleaning.md) | How does Chinese and Korean text reach automatic cleaning without a separate OCR model? |
| [research/custom-model-training-feasibility.md](research/custom-model-training-feasibility.md) | Could a custom mid-size manga inpainting model be trained on a fixed cloud-credit budget? |
| [research/manga-inpaint-models.md](research/manga-inpaint-models.md) | How do open models compare at removing manga text, and what is the masked FLUX.2 Klein recipe? |

## Contributing

| Document | Answers |
|---|---|
| [developer-guide.md](developer-guide.md) | How do I set up the repository, run the tests, and cut a release? |

## Other components

| Document | Answers |
|---|---|
| [../sidecar/README.md](../sidecar/README.md) | How does the out-of-process FLUX.2 Klein sidecar work, and what are its two rendering backends? |
| [../infra/site/README.md](../infra/site/README.md) | How does the public site and the release bucket behind cleaner.komiq.cc work? |
| [../spikes/](../spikes/) | Which experiments and measurement harnesses back the numbers in the findings? One folder per experiment. |
