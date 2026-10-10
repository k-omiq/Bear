# Manga Cleaner promo video: script (cute cut)

About 2 minutes, 16:9, 1080p, 30 fps. No voiceover: music plus on-screen text.

Built from two kinds of shots, cut together in `promo/anim/promo.js`:

- **JS scenes**: canvas animation with the mascot (a speech bubble with a face), pink paper with screentone dots,
  bouncy type (Fredoka and Nunito) and pink stickers.
- **App scenes**: real app usage, recorded headlessly from the app's own interface (`promo/app/clips/`), shown in a soft
  window with a caption sticker, a slow camera push-in, and the mascot peeking over the frame.

Every scene change is the same transition: a pink speech bubble swells to cover the frame, then opens on the next scene.

## Scenes

| # | Shot | What happens | On screen |
|---|---|---|---|
| 1 | JS | The mascot drops in stuffed with real Japanese lettering, gets itchy, is wiped clean and blushes. | Too much text? / Manga Cleaner / Free and open source |
| 2 | App | Library, open "Volume 2". | Open your chapter |
| 3 | JS | Page 010 flies in; its 31 real detected regions pop on (outside-bubble text dashed). | Finds every word. / Speech bubbles, captions, even sound effects. |
| 4 | App | Detect on page 025, real masks appear. | Detect, for real |
| 5 | JS | Real before and after wipe on page 025; Fill and LaMa Manga steps. | Cleans it like new. / Black and white? Use LaMa Manga. / Better models are on the way. |
| 6 | App | Detect and clean the chapter, pages tick over, hold O for the original. | Clean the whole chapter |
| 7 | App | Layers panel, "Clean with" engine picker. | Every bubble is a layer |
| 8 | JS | Colour page 048, model cards (FLUX.2 Klein 4B, 9B, Qwen-Image-Edit-2511), planned own model. | Colour comics? FLUX and Qwen shine. |
| 9 | App | Colour page, pick cloud FLUX.2 Klein, the real cloud consent dialog. | Pick a model per bubble |
| 10 | JS | The mascot stretches into a tall noodle. | Webtoons? Long strips too. |
| 11 | App | Long-strip project scrolling while it cleans. | Long strips, one scroll |
| 12 | JS | The mascot is covered in static and shakes it off. | Grainy scans? Shake it off. / MangaJaNai, waifu2x, Real-CUGAN |
| 13 | App | Chapter menu, Denoise, the six presets. | Denoise from the chapter menu |
| 14 | JS | A sweaty laptop calms down as facts pop in. | Old computer? No sweat. / About 2 GB peak, No AI on flat paper, frees memory by itself, about 320 MB of models |
| 15 | JS | A cloud buddy floats in; hour dials fill. | Borrow a free GPU. / 20+ h a month on L4, ~10 h on L40S / Only the crop is sent. You confirm every run. / Card verification needed |
| 16 | App | First-launch setup to the Cloud GPU step and the Modal token form. | Cloud setup takes one token |
| 17 | JS | Mac, Windows, Linux tiles, then the end card. | Windows and Linux: collaborators wanted. / Manga Cleaner / Free. Open source. GPL-3.0. / cleaner.komiq.cc |

## What in the app clips is not from the real pipeline

The interface is the real app code, but it runs against the browser test backend with a promo fixture
(`promo/app/promo-mock.patch`). Cleaned pixels, masks, region boxes, engines and declines are real pipeline output.
These are not: the cursor, click ripples and key chips; the drawn dropdown in scenes 7 and 9 (headless Chrome does not
paint native menus; entry names are read from the app); library dates and paths; run speed (mock timers are faster than
the real 26 s detect and 27 s clean for page 010); the loaded-models toast sizes; the cloud account (no real Modal
account, the endpoint name is invented); the long strip is three manga pages stacked, not a webtoon.

## Music

Light, bouncy, about 100 to 110 BPM, with a small build into scene 8 and a lift at the end card. Add it in any editor;
the render has no audio track.
