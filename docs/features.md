# Manga Cleaner: what the app does

Manga Cleaner is a desktop app that removes Japanese text from manga and webtoon pages. By default it runs entirely on
your machine: no account, no upload, no Python. An optional cloud GPU is off by default, runs on your own Modal or Beam
account, and a confirmed render uploads only a crop around one region. It finds speech bubbles and free text, keeps only
the Japanese, fits a mask at the page's own resolution, and cleans each region with the lightest engine that can do the
job. Pixels outside an edited region are unchanged in supported lossless exports. Native mode, depth and compatible
colour descriptions are preserved; the export dialog reports necessary format or metadata changes.

## Home and projects

The app opens on a library. A **project** points at a folder of scans and holds one or more **chapters**; a chapter is
the thing you open and edit. **New project** asks for the source folder, a name, and the page mode (single page or
longstrip). The mode is permanent: it decides how pages are stitched and split in every chapter of the project, and
the dialog says so before you press Create. **New chapter** asks for the project, a title, a chapter number and a
source folder. The number starts at one past the highest the project holds and is yours to change; it is stored
exactly as you typed it and never renumbered, so a gap stays a gap, and Create is blocked on a number the project
already uses. Leave the folder empty and the chapter reads a subfolder named after it, or the project's own folder.
Only one chapter may read the project's own folder; a second attempt is refused, and the refusal names the chapter
that already has it.

Creating a chapter **copies its pages into the library**. PNG, TIFF and JPEG originals are copied byte for byte
into the chapter’s `pages/` directory. WebP, GIF and BMP become lossless working PNGs there, and their exact original
bytes are archived beside it in `originals/`. Relative paths, original/working hashes, frame selection and conversion
version are recorded. Animated sources use the first visible canvas and report the frame count; a static full-canvas
GIF retains palette indices and transparency. Available compatible ICC and WebP EXIF metadata reach the working PNG. The scan folder is
read at that moment and never written to or read again, so you may move or delete it and the chapter still opens,
cleans, resumes and exports. The New chapter dialog says so, because a folder you were not told you could delete is
one you will keep. The cost is a second copy on disk, on the order of a gigabyte for a 200-page colour chapter. Before
anything is written, the app estimates the space the copies need, errs on the high side, and refuses the chapter with
the numbers when the disk has less free; a page whose copy still cannot be written is listed with the other skipped
files rather than failing the chapter.

Also on the library screen: **Open project** (with a filter field), **Rename** (the library's label only, the folder
on disk keeps its name), **Remove** (out of the library, pages on disk untouched), **Delete chapter**, **Copy source
path**, **Continue cleaning** on a chapter interrupted mid-run, and **Settings**, which includes the About section
with the version, licence, source offer, model versions and cloud terms. Deleting a chapter always removes its row,
its job file and the library's own copies of its pages; a tick in the same dialog also deletes the folder of scans it
was read from, except the project's own folder, which is refused so that deleting one chapter cannot empty the
project. Nothing goes to the system trash and undo does not reach across it, so the dialog names the folder and the
safe button is the primary one.

Each card shows the chapter and page counts, the mode, pages
cleaned out of the total, regions needing review, files skipped, and where a run stopped. Files are checked as a
chapter is read. A file whose header will not parse, that does not decode completely, that is not an image this build
reads, or whose colour mode has no policy here is **skipped and listed** with its reason, never cleaned halfway.
Dotfiles, `__MACOSX`, `Thumbs.db` and `desktop.ini` are skipped as junk. Files with identical contents are
deduplicated by the stored native or converted file hash, so the same JPEG twice is one page; `001.jpg` beside `001.png` produces a warning
rather than a guess, and two sources that would convert onto one name get a stepped name and the warning still fires.
The chapter reports how many files were converted. AVIF, HEIC and JPEG XL need decoders this build does not ship and
are refused with the rest. Archives (CBZ, CBR, ZIP, PDF) are not accepted; point a chapter at a folder of images.

## The editor

The page sits in the middle, with four floating windows you can move, resize, collapse and close: **Pages**,
**Layers** (the toggle calls it Layers & review), **Tool options** and **Help**. The top bar carries the way back to
the library, the project and chapter name (editable in place), the view controls, and a Settings menu that also holds
Export. **Hold O** to see the original scan under your edits, or **Shift+O** to pin it on; a wipe slider sweeps
between the two. **M** toggles the mask overlay, which outlines every region at once. With the overlay off, a region
is outlined only while you point at it, focus it or select it. The bottom bar carries zoom (fit, a percentage, and
1:1), undo and redo, previous and next page, and previous and next region needing review; pinch and modifier-scroll
zoom as well. Page order is right-to-left by default and is set per project. A card in the corner lists the models
currently holding memory, with a button per row to free one; anything still needed loads again by itself. Notices
about skipped files, cloud rejections and memory pressure stack in the bottom-left corner, never over the image. The
editor autosaves, and reopening a chapter restores the page, scroll position, zoom and the pending review set.

Everything the app draws **on** the page is sky blue: region outlines, the region and canvas focus rings, the brush
cursor's footprint, the clone source cross, and a shape or an AI brush trail while you draw it. Manga is mostly black
ink, so a near-black mark disappears exactly where the work is. The declined and needs-review outlines keep their own
colours, because those say which state a region is in rather than merely where it is, and the paint brush's own colour
is still yours to choose. Pressing a control in a floating window never moves the page underneath it either: the
editor and the windows clip rather than scroll, and a row is brought into view by moving the one box that genuinely
scrolls, so Retry, Delete and the engine picker leave a zoomed canvas exactly where you put it.

## Pages and Text cleanup

The **Pages** window is a text list, not thumbnails, and it doubles as the progress indicator: during a run the marks
tick over page by page, so there is no separate progress bar. Each row shows the page number (or the position, in
longstrip), its status, and how many of its regions are done.

| Mark | Meaning |
|---|---|
| `·` | Not cleaned yet |
| `●` | Cleaning now |
| `✓` | Cleaned |
| `✓ 2` | Cleaned, 2 regions need review |
| `!` | Skipped, with the reason on hover |

**Text cleanup** is the automatic job. Its **Mode** is Detect and clean by default: find text, keep what the script
gate reads as Japanese, fit masks, pick an engine per region, clean, and composite, all in one run. Mode can also run
just one half: **Detect** finds and stores every region it sees, all text and outside bubbles too, without cleaning
any, so the review decides what stays; **Clean** cleans regions already stored, each one starting from the engine it was
detected with. **Text** and **Outside bubbles** are Detect and clean's choices and appear only in that mode. Its other options are **Scope** (Page, the default,
Chapter, or Project, offered only while nothing in the run goes to the cloud), **Detect on** and **Clean on** (This
computer or Cloud GPU, set independently, each disabled with the reason when the cloud cannot take it), **Text**
(Chosen languages or All text), **Speech bubble text** and **Text outside bubbles** (the engine each kind of text
starts a local clean on: Fill and LaMa by default, or Solid colour) and **Outside bubbles** (Hold for
review, the default, or Clean anyway; hidden under All text, which cleans outside bubbles regardless). **Mask padding**
(0 to 32 px, 0 by default) grows every mask a run fits by that many page pixels, so more of the area around the
letters is cleaned. From the keyboard, `;` and `'` step it, `Q` steps the Mode, `V` the Scope, `Cmd`/`Ctrl`+`Enter`
presses the run button and `Cmd`/`Ctrl`+`.` cancels the run. The slider is shown for every step, Detect and clean included, and every step grows its masks by
the value it holds, so the two steps and the one pass leave the same masks. Its **Apply** button re-pads the masks already detected on the page (or, for Chapter and Project,
the open chapter) to the slider's value. Each one is grown from its unpadded mask, so going back to 0 gives the
detected mask back exactly; a mask edit made over a padded mask is kept through later paddings. Cleaned layers never
change. FLUX and the
cloud engine are chosen per region only, after detection. The engine picks are a **starting point, not a ceiling**: if
the quality check rejects what an engine produced, the run climbs to the next engine and tries again. A clean done on
this computer never goes past LaMa; the heavier engines are reachable per region, where you choose them and wait for
them. A region LaMa declines is left exactly as it was and listed for review, which is what the top of the ladder
means.

Text **outside** a speech bubble is left alone unless **Outside bubbles** is set to clean it. Held for review, the
default, sound effects and lettering over artwork are listed with a **Clean anyway** button, which is where the "Text
outside bubbles" engine takes effect one region at a time. Right-click a held region, on the page or on its Layers
row, and **Approve mask** turns it into a detected region instead: nothing is cleaned, its mask shows on the page
where you can edit and pad it, and the next Clean takes it with the rest, on this computer or on the cloud GPU.
**Approve and clean** in the same menu cleans it at once. Set the other way, every out-of-balloon region on the run
goes to the ladder on that engine's rung with **no script read at all**: sound effects, and Latin lettering a
localiser added, are painted over with everything else. That is the row's stated meaning, it is why it is off by
default, and a resumed run goes back to holding them rather than carrying the choice.

With **Clean on** set to This computer, Clean and Detect and clean offer **Solid bubbles only**, off by default. Ticked,
the run cleans only text inside a speech bubble on plain paper, with a flat fill (Fill, or Solid colour when that is the
bubble pick), and only where the quality check accepts the fill. Nothing is redrawn and nothing is sent anywhere. Every
other region stays a detected region with its mask on the page: a bubble over tone or a gradient the fill does not
match, text over artwork, text outside bubbles, sound effects. The clean declines none of them and holds none for
review, so a later Clean with the box off, a Layers row, or a Clean on the cloud GPU takes them. One case is held as in
every run: with a language switched off under Chosen languages, text outside a bubble cannot be told from that
language's, and it waits for its mask to be approved. While it is ticked the panel hides the
rows that would decide nothing: Outside bubbles, the Text outside bubbles engine, and LaMa among the bubble engines.
The run ends by saying how many bubbles it cleaned and how many regions it left detected. The cloud's own version of
this choice is **Clean flat colours on this computer first**, below, which sends the rest to the cloud GPU.

With **Clean on** set to Cloud GPU, the run detects as usual, then asks the cloud clean consent once for the whole
plan (the region count, the pages, the batches and the cost) and, once you confirm, cleans every region on your cloud
GPU: none are cleaned here unless you tick **Clean flat colours on this computer first**, which tries each flat-colour
region on this computer just before its batch is sent and only sends the rest. Declining the consent leaves the
regions detected; nothing is lost. Detecting on the cloud GPU asks its own consent first, since detection and cleaning
are sent, and confirmed, separately.

A run is cancelled from the same place it was started, and the pages already cleaned are kept. When it finishes you
get a count of pages and regions; a chapter with no Japanese text reports "No text found across N pages" rather than
looking like a failure. If the weights are not on the machine, Text cleanup is disabled with a sentence naming
Settings, Models. If the machine runs short of memory mid-run, it says what it will do instead: the FLUX helper stops,
then the redraw model is unloaded and the regions that needed it are listed for review, then it works one page region
at a time.

**You can go on editing while a run works.** A run holds one page at a time, the one it is on. Every other page of
the chapter takes brush strokes, shapes, mask edits, padding, layer changes, deletes and undo as usual, and a cloud
clean leaves the same pages free. An edit of the page the run is on is refused at once, with a notice, and nothing is
changed; it works again when the run moves on. A change to the whole chapter (an export, a delete, padding applied to
every page, replacing pages with denoised ones) waits for the run to end and says so. In a long strip the pages are
cleaned across their joins, so there the run holds the whole chapter.

**Source languages** (Settings, Detection) decide what a run cleans. Each of Japanese, Chinese and Korean is on or
set to Skip, and the choice is captured when a run starts, so a resumed run keeps it. A region whose script belongs to
a skipped language is held for review rather than cleaned, and a run with every language skipped says so and cleans
nothing. The script checker cannot tell Japanese kanji from Chinese Han, so Han text stays eligible while either of the
two is on. Japanese can turn on the optional OCR rescue; it is off unless you choose it, and a run where the reader
will not open says so and goes on without it. With Outside bubbles set to clean and only some languages on, regions
outside a balloon are held, because nothing checks their language.

**Clean English text** (Settings, Detection) is off unless you turn it on. While the Hayai OCR text reader runs, a
region it reads with confidence as English, or another language in Latin letters, is held with its mask and not
cleaned, under either text policy. You can still clean a held region by hand. The choice is captured when a run
starts, so a resumed run keeps it. Without the reader nothing reads the text, and the setting does nothing.

### Detection modes and page review

Detection is one choice, asked in first-launch setup and changeable in Settings > Models and in the Text cleanup panel:

- **Normal** runs Comic Text Detector (CTD) and the small RT-DETR v2 profile with the script checker. It cleans text in
  speech bubbles in the chosen languages and holds text outside bubbles, sound effects included, for review. Needs about
  8 GB of memory; a run peaks near 2.5 GB.
- **Advanced** runs CTD, the full RT-DETR v2 profile and the SAM-TS-L lettering mask, with the Hayai OCR text reader on,
  and cleans all text, sound effects included, without the script checker. Needs about 16 GB of memory with 10 GB free.
  With less free memory it still runs, slower, and a notice that stays until closed offers to stop the run. A cloud GPU
  run is always Advanced.

Neither needs a graphics card. Each card shows what is still to download, and choosing a mode downloads what it is
missing at once. SAM-TS-L setup downloads its pinned checkpoint and source, exports the two ONNX graphs locally, and
verifies their hashes before installation. The model files, the text reader, Clean English text, the language filter
and the text-shaped review are under **More options**. COO SFX detection is not part of this version.

Text cleanup, with the legacy script filter and optional Japanese OCR rescue above, stays the default for every
project. **Text-shaped review** is a separate, optional mode on the current page of a library chapter.

Text-shaped review finds regions and draws the lettering pixels. RT-DETR supplies candidate boxes. The SAM-TS-L
encoder and text head supply the lettering pixels and keep every component no box claimed, so nothing is dropped
silently. This all-text analysis does not use the script checker or the Japanese reader. The review lists every
component. The tinted area is the exact set of pixels a clean may change; the box around it only helps you find it.

Padding (0 px by default, up to 64 px) grows that area evenly around the lettering, and add and remove brushes
correct it. Both are recomputed from the original model output each time, so going from 2 to 5 and back to 2 gives the
first result again. Selecting a component you edited earlier reloads its saved correction, padding and revision
before the next preview. Apply asks the backend to prepare the write, shows it, and writes only if nothing changed in
between; reconstruction may read more of the page, but only the tinted pixels change. See the
[write contract](component-write-contract.md) for the rules behind that. Text outside balloons is held unless you
allow it for the review, a switch separate from Text cleanup's.

Writing is qualified for PNG pages on an Apple M5 with the app's ONNX Runtime on WebGPU only. On any other page type or
machine (JPEG, CPU, Windows, Linux), the review shows the analysis and prepares nothing. Long-strip chapters are not
analysed yet; the review needs a paginated chapter. Every change is one undo step and is saved with the mask it used,
so it reopens and exports the same, and undo and redo pick the exact earlier revision. Turning the option off leaves
saved edits viewable and legacy cleaning unchanged; that is the way to roll the mode back. The measured memory, speed
and parity figures are in the [model workflow benchmarks](model-workflow-benchmarks.md), and the licence position on
each model is in the [model rights decision](model-rights-decision.md).

## The engine ladder

Every clean is done by one of five engines, and the app uses the lightest one that does the job: a heavier model on a
flat balloon is slower and often worse, because it can invent marks where there was only paper.

| Engine | Good for | Notes |
|---|---|---|
| **Fill** | Text on flat paper, the common case | Paints one flat colour exactly on the masked area: the median of a thin ring of paper (4 px) just outside the mask. No gradient, no model, always available. |
| **Screentone** | Lettering printed over a halftone dot screen, including a screen whose brightness ramps | Picked by hand on a Layers row, in the region menu or on the AI mask brush; an automatic run does not choose it yet. Copies the page's own dots from around the hole so they stay in step with the screen, at a brightness carried smoothly across the hole, so a blob that is darker above and lighter below stays that way. First grows the mask over leftover letter outlines and glows that a tight mask (3 px) left behind, up to 12 px out. Where the area is not a screen it fills with PatchMatch. No model; fractions of a second a region. Declines where LaMa declines (indexed, CMYK, one-bit pages). |
| **LaMa** | Screentone, halftone, line art behind the text | The default redraw engine, and the top of an automatic run. About 0.6 s a region on a GPU, 1.5 s on the CPU. |
| **FLUX** | The rare region nothing else reconstructs | Optional helper app you install yourself. Gigabytes of memory, around 20 s a region. Per region only. |
| **Cloud** | Complex art on a weak machine | Off by default, opt in, your own Modal or Beam account, confirmed per render. Per region only. |

Only the engines actually installed appear in a picker. A missing one is left out rather than shown greyed, and the
way to add it is Settings, Models. When no engine's output passes the quality check, the region is **declined**: the
original text is left exactly as it was, a marker stays on the page, and the region is listed for review with the
reason. Some sources restrict the ladder, because a redraw engine's output cannot be represented in them. An
indexed-colour or CMYK page gets Fill alone, which copies one measured source value and so cannot leave the palette
or change the ink separations. A one-bit page cannot hold an inpainted result at all.

## Manual tools

Nine tools sit in a rail on the right, selected with the keys `1` to `7`, `J` (Patch) and `G` (Gradient). Each has a dropdown of parameters; the
dropdown does not have to be open for the tool to work. Masks you draw by hand and masks the automatic pass made are
identical in kind: same row, same actions, same provenance, same export treatment. A hand-drawn mask is
**authoritative**: no detector runs on your gesture and no mask snaps onto a nearby region, so what you painted is
what is cleaned. The ring of paper around it is still sampled, so a fill matches the local paper rather than
defaulting to white.

### Text cleanup

Options: mode, scope, detect on, clean on, solid bubbles only, text, speech bubble engine, outside-bubble engine,
outside bubbles, mask padding with its Apply, and the run button, all described above. It acts on the page rather than on a stroke, so there is nothing to draw.

### Brush

Options: size, hardness, spacing, colour, opacity and flow. The brush paints flat colour and does nothing else.

Size is the same control on every round brush (Brush, Eraser, AI Mask Brush, Clone stamp, and the brush shape of
Selection, Patch and Gradient): 1 to 500 pixels on a log slider, so small sizes get as much of the track as large
ones, a box beside it to type the exact size, the wheel over it, and `[` / `]` from anywhere in the editor, about a
tenth a press. Every number a tool takes has the same slider and box. To
shape a mask that an engine then cleans, use the AI Mask Brush; to remove a mask, use its Layers row or the region
menu, and to take part of one off, use the Eraser.

A stroke is the union of the round dabs the brush sweeps along your path, and only that shape is painted, not the
rectangle around it. Opacity is the most one stroke can cover, however many times it crosses itself. Flow is how
strong each dab is. Hardness is the edge of the dab, and a pen's pressure sets its size; a mouse paints at the full
size. While you draw, the stroke is shown as a see-through layer over the page, so the art around it does not change.

### Shapes

Options: shape (Rect, Ellipse, Lasso, Polygon), mode, and feather up to 20 px. Mode is either **Solid colour**, which
covers the shape in a colour you picked, or one of the four cleaning engines, never both; the colour and opacity rows
appear only while the mode is Solid colour. It starts on Fill, the engine that samples the paper and matches it.

All four shapes are drawn on the page with a live preview of the shape itself: two corners for the rectangle and the
ellipse, with Shift constraining them to a square and a circle; a freehand loop for the lasso; and click-click-close
for the polygon, which finishes on its first vertex, on a double click, or with Enter, and needs three vertices before
any of those will close it. Escape abandons a shape in progress. What is committed is the outline, not the box around
it.

### AI Mask Brush

Options: Clean with (the engine, Fill by default), and size. Paint over the text, and the engine you named cleans
exactly the shape you painted. It differs from the Brush in one way: the Brush commits a Fill, and this runs
whichever engine the row names. The engine is named outright rather than as a starting point, so a hand that picks
LaMa gets LaMa. The list is the same four engines a Layers row offers, under the same names.

### Selection

Key `6`. It adds to or removes from the area Clean erases, or detects text in an area you draw. Draw with a brush, a
lasso or a rectangle: `S` steps the shape. `A` adds, `X` removes and `D` detects, from any tool; `X` or `D` pressed
again goes back to adding. There is no separate Content-Aware Fill tool.
Its old id opens the AI Mask Brush, and the fill choices it offered (Fill or a redraw engine, local or cloud) are the
engine pick on the AI Mask Brush and on each Layers row and region menu. Every cloud render asks first in one dialog.

### Eraser

Key `7`. Options: size, hardness and opacity. Drag over the page to take layers back off it, and the original page
shows again where you erase. It erases Brush paint, shapes, clone stamp strokes and cleaned regions alike, whatever
engine cleaned them. It does not touch the detected masks (use Selection for those), approved review components, or
text-shaped regions. You do not pick a layer first: every layer under the stroke is erased.

One stroke is one undo step, however many layers it crossed. A layer erased to nothing leaves the page, and Undo
brings it back. Erased lettering stays shown after Try again: where a stroke leaves less than half of a cleaned
pixel, Try again leaves the original page there, and where it leaves more, Try again cleans it fully again. Try again
wider grows the cleaned area on purpose, so it can reach back into a small erased spot. In a long strip, a stroke over
a join also erases the part of a layer that hangs across it from the page above or below.

### Clone stamp

The clone stamp copies one part of the page onto another, the way the stamp in a raster editor does. Options: size,
hardness, spacing, opacity, flow, and alignment (Aligned or Non-aligned). It starts as an exact copy: opacity and flow
at 100, aligned.

- **Sample.** Hold the source modifier and click the page to set where the stamp reads from. The modifier is Option
  by default and is a setting, in Settings, Shortcuts, because Alt is not a key every keyboard prints. While the
  modifier is down the cursor is a target. Stamping before a source is set tells you so.
- **Paint.** A click is one dab and a drag is a stroke. The round cursor is filled with the pixels the next dab will
  lay down, and a cross marks the place they come from and moves with the brush.
- **Alignment.** Aligned keeps the distance between the source and the brush from one stroke to the next, so strokes
  read on from each other. Non-aligned starts every stroke at the sampled point. Sampling again starts a new distance.
- **Brush.** Opacity is the most one stroke can cover, however many times it crosses itself. Flow is how much each
  dab adds, so a low flow builds up where dabs overlap. Hardness is the edge of the dab, and a pen's pressure sets
  its size. At full opacity and flow the middle of the brush is the source's own values, unchanged.
- **What it reads.** The page as it stood when the stroke started, with every layer under the new one. A stroke that
  crosses its own source does not copy itself. A source that is off the page copies nothing there, and a stroke with
  nothing to copy is declined.
- **Straight lines.** `Shift`-click draws a line from where the last stroke ended. `Shift` held during a drag keeps
  the stroke horizontal or vertical.
- **Keys.** `[` and `]` make the brush smaller and larger, and `Shift`+`[` and `Shift`+`]` step the hardness by 10,
  as they do for every brush. `C` swaps Stamp for Region copy and `Shift`+`C` swaps aligned for non-aligned. With the
  page focused, `S` samples under the pointer. `Enter` opens one dab, the arrows move it (five steps with
  `Shift`), and `Enter` stamps it. `Escape` drops an open stroke, and pressed again it clears the source.

Each stroke is one layer and one undo step. A project saved when the tool was Clone / Heal keeps its layers, and they
are named Clone stamp.

The Clone tool also has a **Region copy** mode for moving a clipped piece of artwork:

Right-click a copied region on the canvas or its Layers row and choose **Duplicate**, or select it and press **⌘D** (macOS) / **Ctrl+D**. The copy preserves the selection, rotation, and blending, appears slightly offset and selected, and can be moved or edited independently. Duplication is undoable.

- Choose Brush, Rectangle, Ellipse, Lasso, Polygon, or Magnetic lasso and select the artwork to copy. The original
  stays in place. Polygon and Magnetic lasso finish by clicking the first anchor, double-clicking, or pressing Enter;
  Backspace removes the last anchor and Escape cancels. Magnetic lasso bends the path between anchors along visible
  contrast edges. Use closer anchors to follow detailed contours.
- The new copy is selected automatically. Drag its artwork to move it and drag the circular handle above it to rotate.
  Adjustments offers Feather in image pixels, Edge opacity, overall Opacity, and Rotation. Edge controls soften the
  original selection at its destination, preserving the captured pixels and the opaque interior.
- **New selection** starts another copy. Copies, placement, rotation, and adjustment bursts use the normal layer
  history and persist with the project. Export uses the same native masks and transforms as the committed canvas.

### Patch

Key `J`. The Patch tool fixes an area from what is around it, the way the patch tool in a raster editor does, with
less to set up. Select the area with the same shapes as Region copy (Brush, Rectangle, Ellipse, Lasso, Polygon or
Magnetic lasso, Lasso by default). The selection is held rather than committed, and nothing changes until you press
**Apply** on the bar or `Enter` on the page. Escape drops the selection, and a press outside it starts a new one.

- **Auto**, the default, needs no source. Where the selection sits on a halftone dot screen it continues the dots,
  as the Screentone rung does; elsewhere it fills as Content-Aware. The layer records which of the two ran.
- **Normal** copies a source into the selection and matches it to the edge. The source is proposed as soon as the
  selection is held, from the nearby area whose surroundings match best, and drawn as a dotted ghost of the
  selection. Drag inside the selection or the ghost to move it. Where nothing nearby fits, the ghost starts one
  selection-width to the side and the bar says so. **Direction** Source fills the selection from the ghost;
  Destination copies the selection to the ghost instead. **Diffusion**, 1 to 7 (5 by default), is how far the edge
  colour carries in: 1 keeps the source's own values a few pixels in from the edge, 7 blends it fully. A source off
  the page, or exactly on top of the selection, is declined.
- **Content-Aware** fills the selection with patches taken from around it. **Structure**, 1 to 7, is the patch size:
  higher keeps larger shapes together. **Color**, 0 to 10, is recorded with the layer; the fill already matches the
  page's colour exactly, so in this version it changes nothing.
- **Smooth** fills the selection with the smoothest surface that meets its edge, for flat paper and soft tone.

Each Apply is one layer and one undo step, named Patch in the Layers window, with its options recorded. A patch is
read from the page where it sits, so it cannot be moved afterwards; its opacity can still change. Indexed, CMYK and
one-bit pages are refused, as they are for the Brush.

### Gradient

Key `G`. The Gradient tool fills a selection with a gradient. Select the area with the Region copy shapes, then drag
inside it to set where the gradient starts and ends; `Shift` turns the line in steps of 15 degrees. The two ends stay
on the page as handles, and the selection shows a live preview of the gradient while you move them. **Apply** or
`Enter` draws it, and Escape drops it.

- **Type**: Linear, Radial, Angle, Reflected, Diamond or Auto.
- **Auto** (`Shift`+`G` arms the tool with it) needs no line and no stops. Lasso the lettering on a shaded bubble
  and it fills as soon as the selection closes, with the gradient the page has on every side of it, so a ramp that
  eases or bends still meets the page all round. Glow or letters the lasso cut through are taken too, up to 48 px
  past it, and the bubble's outline, its rim glow and the paper beyond are never painted, even inside the
  selection. Where the ring around the selection is no one smooth surface (a screen, a busy drawing) it refuses
  and the page is left as it was; use the Patch tool there.
- **Stops.** The swatch on the bar opens the colour and opacity stops: add, move or remove each one, pick a colour
  stop's colour with the picker or from the page with its eyedropper. Two presets fill them in: Foreground to
  background and Foreground to transparent, where foreground is the Brush's colour.
- **Reverse** flips the gradient. **Dither**, on by default, adds a little noise so a slow ramp shows no bands.
  **Opacity** is the layer's, and **Feather**, up to 20 px, softens the selection's edge.
- **Match surroundings** reads the ring of page around the selection and sets the type, the line and the stops to
  continue it, for a soft shadow or a sky that a letter sat across.
- **As screentone** draws the gradient as the page's own dot screen, its dots growing and shrinking with the tone,
  on a gray page with a screen around the selection. The dots are copied from the screen around the selection, so
  only tones the screen already has nearby come out as dots; a gradient that runs far lighter or darker than its
  surroundings is filled from patches there instead. Where there is no screen, or on a colour page, a plain gradient
  is drawn and the app says so.

The gradient is kept with the layer. Select a gradient layer with the tool armed and **Edit gradient** brings back
its line, stops and options; Apply then redraws that layer in place. Each Apply is one undo step, and a gradient
layer can be moved, locked and faded like any paint layer. Indexed, CMYK and one-bit pages are refused.

## Layers and history

The **Layers** window lists every region on the current page, newest first. Collapsed, a row names the engine that
produced it and carries a delete control. Expanded, it shows provenance: engine, model version, fill mode, elapsed
time, whether it was made by hand or automatically, whether the automatic pass detected it, and for a cloud region the
provider, endpoint, job and attempt, recipe and model revision, and cost only when reported.

Each row can be re-run: **Try again** with the same setting, **Clean with** a different engine, or reopened in the tool
that made it with the mask intact. **Delete** removes the mask, brings the original text back, and takes the row off the
list, leaving no empty placeholder. **Dismiss** takes a flagged region off the list and leaves the page as it is, and
**Show on page** scrolls to a region and selects it. **Restore mask**, on any result a cleaner made (Fill, LaMa, FLUX,
the cloud, a hand mask included), takes the result off and puts the region back as detected with its mask, ready to
pad, edit or clean again. A result cleaned from detected text gets that detected mask back; any other result, such as
one an older build cleaned, gets the mask the clean wrote through. Undo after any clean of detected text does the same,
and Redo puts the same result back without rendering it again. A paint or clone stroke, a region copy or a text-shape
result has no cleaner's mask and says so. Everything here is undoable. Clean with offers Cloud when a cloud
GPU is ready, and Try again on a cloud region asks for confirmation again, as every cloud render does. Clicking a region
on the page selects it under every tool, lights its outline and scrolls the matching row into view. A right click, or
Shift+F10 from the keyboard, selects the region and offers the same three things the row does. On a region that is
detected and not yet cleaned, the menu also offers **Text type**: Speech bubble text or Text outside bubbles, with the
current one checked. Changing it sets which engine pick a later Clean starts the region on and the color its mask is
drawn in. The menu also has that detection's **Mask padding** slider, the same one its Layers row has, with a box
to type the value and the wheel to step it once it has the focus (so the wheel still scrolls the menu): the value it
settles on re-pads that one mask and the menu stays open. An entry picked right after a padding change waits for that
change to be saved.
`;` and `'` step the selected detection's padding a pixel at a time. With none selected they step every detection on
the page, from the padding they share (the panel's Mask padding when they differ), and set the panel's value too. A
run of presses is saved as one undo step, to the page it started on, and Undo while the run is still waiting drops it.
Both are undoable, like every edit of a detected mask.

**Needs review** is a filter at the top of the panel with a count, and it is the review surface. A region is flagged
when fitting failed and a model reconstructed the area; when the region is unusually large for the page; when every
engine failed the quality check, so the text was left alone; when the script gate skipped it, because it read as not
Japanese or because the gate was not confident enough (with the optional Japanese text reader installed, a balloon the
gate could not read is read once more before it lands here); or when the text is outside a speech bubble. Gate-skipped
regions carry a **Clean anyway** action. Declined regions carry a marker on the page itself, since they are the one
case where nothing visibly happened. The bottom bar's up and down arrows step through the filtered set without entering
the panel. Undo and redo are global across every tool and are **written to the project folder**, capped at 500 entries,
so a chapter reopened tomorrow can still take back what was done today. Their tooltips name the edit they will reverse.
Every kind of edit is a step: layers and masks drawn, cleaned, re-run, moved or deleted, and also detected masks deleted,
brushed, padded or retyped, held regions approved or dismissed, and a whole Detect run, which is one step. A step that
cannot be put back because the page changed in a way the history does not know about says so and changes nothing.

## Long-strip mode

A project created in longstrip mode treats a chapter as one continuous strip rather than a stack of separate pages.

- Nothing concatenates pixels, at any point, including export. The strip is a coordinate system laid over the source
  files.
- A bubble that crosses a join between two files is read and cleaned as **one region**. At export the patch is split
  at the page boundary, and both halves share one fill tone, because it is the same tone written twice.
- The strip is split into working segments at rows with the least detail, never through detected text, preferring a
  page join where one is close enough. Where no good row exists the split falls back to a fixed height and says so.
- The Pages window lists **positions** rather than files, and choosing one scrolls the strip to it. Previous and next
  page move by viewport, and the Layers panel lists what is on screen rather than the whole chapter.
- Duplicated overlap rows and misregistered joins between consecutive files are reported rather than silently
  stitched, and an unverified join is treated as a page edge.
- Memory is bounded: previews cap the short edge and tile the long axis, the scroller keeps about three screens, and
  only three pages' region state is held at once. A chapter of any length opens the same way.

## Export

Export is on the Settings menu in the top bar, and on **E**.

**Formats.** PNG (the default), TIFF (also the right choice for a stitched longstrip, and the only one that carries
CMYK among raster choices), **PSD**, and CBZ, which is a zip of the per-page files stored rather than deflated. JPEG and WebP are refused
rather than approximated, and the refusal says what would work instead. PSB is not written.

**Layout.** One file per page, or one file for the chapter. Stitching a paginated project is refused, as is stitching
into a CBZ or into a PSD. Stitched pages must agree on mode, depth, profile, gamma/chromaticities, cICP and
transparency; indexed stitching is refused. Validation occurs before outputs are published.

**Masks.** Flattened is the image alone. Asked for separately, a PNG or TIFF export writes **a mask file beside each
page**, `001.png` and `001_mask.png`, an 8-bit grayscale PNG that is white where the lettering the page's edits
removed was and black everywhere else. It is the lettering, not the area an engine painted: a file made of the applied
masks would be a page of white blobs where the balloons are, which is not what someone asking "where was the text"
wants. For a PSD the same choice is the layered document instead, the untouched page as the Background and one masked
layer per region in a Cleaned group, and a layer's own mask stays the area it contributes. A CBZ refuses separate
masks, because a reader shows every file in the archive as a page.

**PSD.** Per page, in the source's own mode and depth: grayscale, gray with alpha, RGB, RGBA and CMYK at 8 and
16 bits, with the embedded profile carried through. Flattened, it is one Background layer holding the cleaned page.
Either way the merged image the file carries is the composite, so a reader that ignores layers still sees the cleaned
page, and a hidden region is a hidden layer that is still in the file. An indexed or sub-8-bit page, and a page over
30 000 px on a side, are refused for PSD rather than converted, decided from the chapter's own record before a folder
is made; the refusal names PNG, which carries them as they are.

**Destination.** A new sibling folder (the default), the source folder, or a folder you type or choose as a full path.
The source folder is never written to: choosing it gets you a refusal, and the dialog says so under the row rather
than letting you find out by pressing Export.

**Before you press Export** the dialog validates every page, displays actual output formats and explains format
fallbacks and metadata changes. It also reports flagged regions and any stitched gutter pixels. The plan is tied to
source/patch/manifest state; a changed chapter requires a fresh plan. Outputs are staged before publication. A runtime
publication failure reports completed files and the failing path. The finished notice uses the actual formats.

**What is preserved.** Supported lossless exports retain native decoded samples outside the applied masks, including
16-bit low bytes, palette indices and alpha. Orientation moves samples without changing their values. A compatible,
untouched same-format export is an exact byte copy; CBZ retains untouched JPEG bytes. Edited RGB/gray JPEGs use PNG,
and edited CMYK/YCCK JPEGs use TIFF. JPEG recompression is not offered.

PNG writers share one metadata policy: exact gAMA/cHRM/sBIT/cICP/mDCV/cLLI declarations and safe ancillary chunks
are retained where valid. Edits remove stale significant-bit/content-light summaries; precedence conflicts are
normalized and disclosed. TIFF/PSD use equivalent supported ICC descriptions where possible and otherwise refuse.
General EXIF thumbnails and geometry are omitted on re-encode; imported originals retain them. Invalid profiles and
unsupported HDR preview/paint transforms fail explicitly. These guarantees do not claim every ancillary field survives.

Previews transform supported Gray/RGB/CMYK profiles and SDR declarations to tagged sRGB. Untagged RGB and grayscale
use documented sRGB display assumptions; untagged CMYK uses a multiplicative-ink assumption, without changing stored
metadata. Colour keys are compared at native precision. Downsampling uses linear light and premultiplied alpha.
Brush, selected fill and model boundaries use the same interpretation. Brush modes remain 8-bit Gray/GrayAlpha/RGB/
RGBA; native clone copies remain exact. Live brush and clone stamp previews come from the backend’s commit renderer,
as a see-through layer on the page's proxy grid that holds only the painted pixels.

TIFF alpha association is explicit: native premultiplied values remain unchanged and previews unassociate only the
display derivative. Associated-alpha pages export as TIFF; PNG requests fall back and PSD refuses. Editing those
pages currently refuses. Non-ICC TIFF transfer/white-point/primary descriptions also refuse managed processing until
a tested equivalent interpretation exists; their native imports and unchanged TIFF exports remain available.

EXIF orientations 1–8 are applied to display and re-encoded output. Existing native-coordinate edits are intersected
before orientation; new oriented seam edits keep one history identity with native parts for each participating page.
Older chapters, including previously converted JPEG pages without an archived original, remain readable.

## Settings

Settings is on **,** from anywhere. Changes apply at once and are remembered between sessions. It is six tabs,
General, Models, Acceleration, Cloud, Shortcuts and About, and it opens on General (or on the Cloud tab when opened
from a cloud link), so the preference rows are the first thing on screen with nothing to press. Every tab is one
fixed-height scroller, so the dialog is the same size and the Done button sits in the same place whichever tab you are
on. General rows, in order: **Theme** (light, dark or system), **Selection color, speech bubble text** and
**Selection color, text outside bubbles** (the two colors detected masks are drawn in on the page, blue and burnt
orange by default, so the two kinds of text are told apart at a glance; a mask whose place is unknown uses the speech
bubble color, and a color picked when there was only one is kept as that one),
**Selection opacity** (how strong both fills are), **Keep running when closed** (close to tray: the window
hides while downloads continue, and the tray icon reopens or quits; off, the close button quits; shown only when a tray
icon exists), **Reading direction** (the default for new projects; an open project keeps the direction it was made
with), **Cloud GPU** (says whether it is on, plus **Open Cloud settings**), **Original view** (whether O shows the
original only while held or toggles it on), **Language** (English, 한국어, 日本語, Español, Português or Français; every
string in the app, errors included, is in the catalogue), and **Setup** with **Run setup again** (walks through downloads,
defaults, the cloud GPU and app behavior again).

### Models

The engines and the runtime are downloaded after install rather than bundled, which keeps the installer small. This
section is a list of files: one row per model plus one for the runtime, each saying what it is, how large it is and
whether it is here, with **Download**, **Cancel**, **Check**, **Delete** and, when a stopped transfer left bytes
behind, **Discard partial**. A row with a remainder says how much is already here and that the next press continues
from there. Downloads are verified against a published checksum, and Check re-reads a file you suspect.

The script gate and the Japanese reader each show as one capability: their component files, sizes and pinned
SHA-256 values can be inspected, but they install and update together. The install stages and verifies every missing
file before it switches anything on, so a failed transfer leaves the files already installed intact. Removing the
Japanese filter does not remove the RT-DETR weights, which other workflows share.

One row is optional in the strongest sense. The **Japanese text reader** (manga-ocr, three files, about 460 MB) is
offered here and, unticked, in the first-launch setup: nothing depends on it, and a machine without it reads scripts
exactly as this app did before the reader existed. Installed, it re-reads a balloon the language checker could not name
a script for, and a reading that is at least two characters and at least 60 per cent Japanese lets the region clean
instead of going to review.

On a first launch, the app walks through a six-step setup with one choice per step, each skippable with Skip setup, and
Run setup again in Settings, General replays it. Step 1 is Welcome. Step 2 downloads the models: the required set (the
engine runtime and the models that find text and speech bubbles) as one locked row, the redraw engine, manga-LaMa, as a
tick that starts on, and the Japanese text reader as a tick that starts off, with one Download button, pause, resume and
retry, and you can continue while it runs. Step 3 chooses the defaults: accelerator (Automatic recommended), and when
the AI redraw helper is installed, its folder, model and engine. Step 4 is Cloud GPU (optional): Not now leaves cloud
off; Set up now opens the setup in place and turns the cloud permission on when setup succeeds and the new endpoint
answers its first health check (if the check fails, the endpoint is still saved and selected, the permission stays off,
and it can be tested later from Settings, Cloud); while the setup helper is at work (checking the account, planning, or
creating or removing resources in it), the first-launch setup cannot be closed (Escape and Skip do nothing). Step 5 sets
app behavior: keep running when closed (close to tray), and the default reading direction for new projects. Step 6 is
Done: a summary read back from the settings in force, and New project. The setup is shown once; Settings, Models is the
way back for anything declined. A weight the app did not download (found beside the executable, in the bundle, or in a
folder you set) reads as installed elsewhere and has no Delete button. For an offline install, put the verified files in
one of those folders by hand and the rows read Installed. On Windows and on Linux x64, where more than one runtime build
is published, the runtime row carries a picker: the default works with every graphics card, and the CUDA builds need
CUDA and cuDNN installed by you. Changing the picker downloads nothing on its own, and the row says which build is
actually installed when that differs from the one chosen.

An optional **Hugging Face token** field sits at the bottom, because some downloads come from huggingface.co, which
limits anonymous transfers. It is kept in the operating system's credential store, sent to huggingface.co and no other
host, and never shown again once saved: the field is empty on every open and a line beside it says whether one is
stored. Where there is no usable credential store, or the keychain is locked, the app says which of the two it is and
keeps the token in the settings file as plain text rather than failing quietly.

### Acceleration

A picker of Automatic plus every device the installed runtime reports, and under it a read-only line per model saying
where it will run and whether that was measured here or guessed. A device that cannot be used is listed and disabled,
carrying its own reason (not available in this runtime, would split the model and run it slower, needs CUDA and cuDNN
installed, or this machine has less memory than that device needs). Automatic picks the fastest device known to work
for each model, which is not always the same one. A change takes effect on the next run; anything already loaded keeps
the device it started on.

Every Windows and Linux x64 download brings the **WebGPU plugin** beside the runtime, which is the cross-vendor GPU
path that carries the operator the redraw model needs. On Windows it sits next to DirectML, and a machine on a CUDA
build runs the detector on CUDA and the redraw model on the plugin rather than falling back to the processor. On Linux
x64 it is the whole GPU path and it needs the system Vulkan loader; without one the device is listed and disabled with
that as its reason. Linux aarch64 has no GPU package published for it and stays on the processor. None of those
placements has been run on real hardware, so each is reported as a guess rather than a measurement, and the memory
check, which needs a measurement to act on, never declines one.

### Shortcuts

The shortcut sheet, which is also what **?** opens, is where a shortcut is changed. Every chord in it is a button:
click one and the row listens, the next combination you press becomes the binding, Escape cancels, and Backspace or
Delete clears it (the row then reads Unbound and the command keeps whatever pointer route it has). A combination
another shortcut already answers is refused, not swapped, and the refusal names the shortcut holding it. Alt
combinations belong to the operating system and are not intercepted. A rebound row grows a Reset control, and the foot
of the sheet carries Reset all to defaults. The sheet also carries a **Pointer** group, which holds the clone stamp's
source modifier: Option, Command, Control or Shift, drawn as the keycaps of the machine you are on, because Alt is not
a key every keyboard prints. Escape is the one binding that cannot be changed. Only your differences
from the defaults are stored, so a binding survives a build that renames a default chord.

### Cloud

The cloud GPU is **off by default**. The main switch, **Use a cloud GPU**, is on the Settings Cloud tab, and the
General tab has a **Cloud GPU** row that reports its state and links to the Cloud tab. While off, nothing leaves your
computer, and any cloud action is refused before anything is sent. Text cleanup's clean reaches the cloud only when
Clean on is set to Cloud GPU, and only after the cloud clean consent for that exact plan; the local FLUX helper
remains its own separate engine choice.

Setting up a cloud GPU uses your own Modal or Beam account, which bills you directly. You can start from **Set up with
Modal or Beam** on the Cloud tab, or from step 4 of the first-launch setup. After you paste a Modal token or Beam API
key, choose FLUX.2 Klein 4B or 9B or Qwen-Image-Edit-2511 (L40S only) and optional SAM-TS-L/RT-DETR analysis models. On Modal you can also choose **No cleaning model**: setup then installs only the analysis models and page denoise you select, downloads no cleaning weights, and creates no FLUX worker. Such a setup needs at least one analysis model or page denoise; choosing it with nothing else selected selects both analysis models. Cloud cleaning is not offered on it until you update it and choose a cleaning model. The app presents a plan for approval
showing the GPU type, idle timeout, cost notes, and selected weight downloads. Nothing is created until you approve it.
A bundled helper then creates, in your account, a volume for model weights (seeded on a CPU), job storage, an on-demand
FLUX worker, a separate on-demand analysis worker when selected, and a gateway that the provider opens only to calls carrying the endpoint's
credential; it checks the new endpoint, and the app saves it and selects it as your cloud GPU. For Modal, setup also
creates an access token that can only call that endpoint. Your Modal token is kept only when **Remember this Modal
token** is ticked (the default), so updating the setup later does not ask for it again. Beam has no separate access
token, so the endpoint is called with your own Beam API key. Either one is kept in the operating system's credential
store, never in a settings or project file. On Beam, setup also stores your key as a secret in your Beam account,
because the gateway uses it to start the GPU jobs. Setup shows each step as it runs and can be stopped. What finished is
kept, an unfinished setup is listed under **Needs attention** with Resume and Forget, and Resume continues from where it
stopped without redoing what is done. Removing an endpoint takes it off this computer only, unless you also choose to
delete its cloud resources: then the app lists what will be deleted, including the model weights, and deletes only what
this app created. You can also connect an existing public HTTPS endpoint by hand under **Connect an existing endpoint**.
Several endpoints can be kept, including setups in different Modal accounts: run setup again with the other account's
key. Each row names its Modal account and can be renamed. Each keeps its own access token, so choosing another default
switches accounts without entering a key again.

GPU workers scale to zero after the chosen idle timeout. Analysis capabilities appear only after the selected graphs
pass size and SHA-256 verification. CTD remains local.

The app does not report cloud spending. Modal offers no way to read the credits left on an account, so check usage
and credits on the provider's billing page. A remembered Modal token is used only to set up, update or clean up that
setup, and removing the endpoint deletes it.

When enabled and an endpoint is ready, its FLUX model appears alongside local models, with a leading cloud icon, in the AI mask brush and Layers/region **Clean with** menus. **Try again** on a cloud region uses its cloud endpoint. Every cloud render asks first, every
time, in a dialog ("Send this region to your cloud GPU?"): it states what is sent (a crop of a given size around the
region and its mask, while the rest of the page and project stay local), the destination endpoint, and the cost billed
by your provider. A confirmation allows that one render and nothing else, and nothing is sent without one.

When cloud detection has had a request out for about 30 seconds and the provider has started no GPU for it, the corner
card shows a **Waiting for a GPU** row for that setup. The provider most often has no GPU of that type free; the
detection goes on by itself when one starts. The row's button stops the waiting work, and the row becomes the GPU's own
once one is up. A cloud render and a cloud denoise do not show this row.

A running cloud render displays a status card in the bottom corner with elapsed time, current phase, a Cancel button,
and a note that the first render can take 1 to 3 minutes while the GPU starts. Cancelling stops the render at the next
safe point and asks the gateway to cancel the job; a render that finished before the cancel reached it is still applied,
because it has already run and been billed. A failed cloud render does not fall back to a local engine: the region is
left unchanged and a notice explains why. When it is not known whether the endpoint received a render, it is never sent
again on its own. At the next start (or, when the cloud GPU is off then, the first time it is turned on) the app applies
results that finished while it was closed and lists anything that needs attention on the Cloud tab; nothing is ever
resubmitted. Completed regions store cloud provenance (provider, profile, job and attempt identifiers, recipe, model
revision, and cost only when reported) and can be re-run after asking for consent again.

### The optional redraw helper

Three rows appear for the optional FLUX helper app: the folder it is installed in, which runtime to load it through
(Automatic, MLX on Apple hardware, or SDNQ on any GPU), and which of the models in its weights folder to use. Only
models actually present are offered. A machine without the memory, without a GPU, or without the chosen runtime's
packages is told which of those it is.

## Updates

The app checks for updates on launch and from a **Check for updates** button in Settings. When one is available you
get the version and its release notes, and the choice of **Download & install** or **Later**; the download reports its
percentage and finishes with **Restart Manga Cleaner**. Updates are downloaded from cryptographically signed
manifests. An update never silently changes a model or a default under a project you already have: a re-run after a
change is recorded as a new provenance entry beside the old one. There is no background telemetry. Statistics are
written into the project file and readable in review; crash reports are written locally and surfaced on the next
launch, never sent anywhere without an explicit action.

## Keyboard shortcuts

Every tool, and every option a tool's bar shows, is reachable from the keyboard: the common ones have a key each, and
`\` moves focus to the bar's next slider, dropdown or box for the rest. Every binding but Escape can be changed or cleared in Settings,
Shortcuts. A letter bound without Shift also answers with Shift held, unless the table gives it a Shift binding of its
own. The brackets and braces also match with Option or AltGr held, which is how many layouts type them. Tool, view, zoom, navigation and panel keys work while a
chapter is open.

| Key | Does |
|---|---|
| `1` | Text cleanup |
| `2` | Brush |
| `3` | Shapes |
| `4` | AI mask brush |
| `5` | Clone stamp |
| `6` | Selection: add to or remove from the detected masks, or detect text in a drawn rectangle |
| `7` | Eraser: take paint or a cleaned area back off the page |
| `J` | Patch: fix a selected area from what is around it |
| `G` | Gradient: fill a selected area with a gradient |
| `Shift`+`G` | Gradient, Auto type: fill a selection over a shaded bubble from all round it |
| `Cmd`/`Ctrl`+`Enter` | Apply the held Patch or Gradient selection; otherwise press Text cleanup's run button (the first press opens Text cleanup when it is not showing) |
| `Cmd`/`Ctrl`+`.` | Cancel the run |
| `V` / `Shift`+`V` | Text cleanup scope: next / previous (the first press opens Text cleanup) |
| `S` / `Shift`+`S` | Next / previous shape of the tool (the Selection tool when the tool has no shape); with the canvas focused, the clone stamp samples its source instead |
| `Q` / `Shift`+`Q` | Next / previous mode, type or engine of the tool |
| `A` / `X` / `D` | Selection: add, remove, detect in an area (`X` or `D` again: add) |
| `C` / `Shift`+`C` | Clone stamp: Stamp or Region copy / aligned or not |
| `[` / `]` | Smaller / bigger brush, about a tenth a press, for every round brush |
| `Shift`+`[` / `Shift`+`]` | Softer / harder brush |
| `I` / `Shift`+`I` | Less / more opacity (a region copy being placed: its own opacity) |
| `K` / `Shift`+`K` | Less / more flow |
| `Y` / `Shift`+`Y` | Less / more spacing |
| `\` | Focus the tool bar's next slider, dropdown or box (then the arrows, the wheel, Space or a typed value) |
| `;` / `'` | Less / more mask padding: the selected detection, else every detection on the page |
| `O` (held) | Show the original while held |
| `Shift`+`O` | Pin the original on |
| `M` | Mask overlay |
| `R` | Show only the regions that need review |
| `W` / `Shift`+`W` | Less / more of the original's wipe |
| `U` | Undo |
| `Shift`+`U` | Redo |
| `Cmd`/`Ctrl`+`Backspace` | Delete the selected layer |
| `Cmd`/`Ctrl`+`D` | Duplicate the selected region |
| `B` | Clean the selected detection, or try the selected cleaned region again |
| `Shift`+`B` | Try the selected cleaned region again, wider |
| `0` | Fit the page |
| `+` (or `=`) | Zoom in |
| `-` | Zoom out |
| `Z` | Actual size, 1:1 |
| `←` | Page left |
| `→` | Page right |
| `Home` / `End` | First / last page |
| `N` | Next region needing review |
| `P` | Previous region needing review |
| `F` | Pages window |
| `L` | Layers & review window |
| `T` | Tool options window |
| `E` | Export |
| `N` (library) | New project |
| `Cmd`/`Ctrl`+`O` (library) | Open project |
| `,` | Settings |
| `?` | This shortcut list |
| `H` | Back to the library |
| `Escape` | Cancel or close |

Arrow keys follow the reading direction: in right-to-left, the default, `←` is next. `N` is New project on the library
screen and next-review in the editor, which is why the two never collide. A bare `Delete` or `Backspace` still deletes
from a focused Layers row.
