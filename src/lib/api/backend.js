/**
 * The backend seam.
 *
 * Everything the interface needs from a backend goes through this module, and
 * only this module. No component imports `mock.js`; components import
 * `getBackend()` from here and get whichever implementation is selected, so
 * swapping the mock for Tauri `invoke` calls and a Tauri channel touches one
 * file - this one.
 *
 * Two rules the interface exists to enforce:
 *
 * 1. **Every method is async.** A real `invoke` returns a promise; nothing may
 *    come back synchronously, or the interface would come to depend on timing
 *    the real backend cannot provide.
 * 2. **Progress arrives only on the event channel.** `subscribe(handler)` is
 *    shaped like a Tauri channel: one handler, many events, never a callback
 *    per call. A run reports itself with `page-started`, `region-done`,
 *    `page-done` and `run-finished`; everything transient is a `notice`.
 *
 * Every user-visible string this layer produces is an i18n key plus
 * parameters. No English text crosses the seam.
 *
 * Nothing in `src/lib/api` imports from `svelte` or touches the DOM.
 */

import { createMockBackend } from './mock.js'
import { createTauriBackend, isTauri } from './tauri.js'

/* ------------------------------------------------------------------ */
/* Data shapes                                                         */
/* ------------------------------------------------------------------ */

/**
 * A translatable time like "2 h ago", carried as data rather than as text so
 * the interface renders it in the user's language. What must never cross the
 * seam is the formatted *string* - a real backend has a stored timestamp and
 * a clock and nothing else, so it is the one that picks the bucket. The
 * mock's values are fixture data, chosen by hand; that is a property of the
 * mock, not of this contract.
 *
 * @typedef {Object} RelativeTime
 * @property {string} key - i18n key, e.g. `time.relative.hoursAgo`
 * @property {Object} params - e.g. `{ count: 2 }`
 */

/**
 * `Project` (src/lib/model/types.js) plus what a backend owns. Home is a
 * two-level hierarchy - Projects → Chapters - so a project always carries its
 * chapters, and a chapter always carries its pages.
 *
 * @typedef {import('../model/types.js').Project & {
 *   sourcePath: string,
 *   lastOpened: RelativeTime,
 *   starred: boolean,
 *   interruptedJob: { chapterId: string, pageIndex: number }|null,
 *   conversion: { from: string, to: string }|null
 * }} ApiProject
 */

/**
 * A chapter's `sourcePath` is the folder its own pages were read from, and it
 * is a different fact from the project's: a project points at a folder of
 * scans, and each chapter reads one folder inside it - or somewhere else
 * entirely. It is `''` when the backend cannot say, which is what a chapter
 * created before `createChapter` carried a path has. An empty string is not a
 * licence to substitute the project's path; the two are different questions and
 * a wrong answer to this one names files the chapter does not hold.
 *
 * `review` is the chapter's **review index**: one
 * short record per region needing review, chapter-wide, in page order. It is
 * carried rather than derived because the review set is the one thing in the
 * interface that is honestly chapter-wide - stepping to the next issue may land
 * eleven pages away - and deriving it from the regions is what used to keep
 * every region of every chapter resident.
 *
 * @typedef {Object} ReviewRef
 * @property {string} id - the region's id
 * @property {string} pageId
 * @property {number} pageIndex
 * @property {string} reasonKey - the `review.reason.*` key
 */

/**
 * @typedef {import('../model/types.js').Chapter & {
 *   number: number,
 *   lastOpened: RelativeTime,
 *   sourcePath: string,
 *   sourceFormat: string,
 *   noTextDetected: boolean,
 *   inputReports: NoticeSpec[],
 *   review: ReviewRef[],
 *   denoiseReplacement: {pages: number, kept: number, missing: number}|null,
 *   denoiseHistory: DenoiseSummary|null
 * }} ApiChapter
 */

/**
 * `panels` and `layout` are stand-in image data for the mock canvas, not
 * information a real backend would send.
 *
 * **`regions` is the windowed part**. A page
 * outside the resident window arrives as a header: `regions: []`,
 * `resident: false`, and `regionCount` saying how many it would have had.
 * `resident` exists because `regions: []` otherwise means two different things
 * - "no regions" and "not loaded yet" - and a Layers panel that cannot tell
 * them apart draws an empty list for a page full of masks.
 *
 * `doneCount` and `reviewCount` ride alongside it for the same reason: the
 * Pages list draws a `done / total` ratio and a `✓ n` mark for **every** page
 * of the chapter, and a count taken off `regions` reads zero for every page but
 * the three in the window. The backend reads all three out of the manifest, so
 * they are right the moment a project is reopened.
 *
 * @typedef {import('../model/types.js').Page & {
 *   number: number,
 *   file: string,
 *   sourceSha: string,
 *   sourceIndex: number,
 *   width: number,
 *   height: number,
 *   regionCount: number,
 *   doneCount: number,
 *   reviewCount: number,
 *   resident: boolean,
 *   appearance?: string,
 *   layout: number,
 *   panels: Array<{x: number, y: number, w: number, h: number}>
 * }} ApiPage
 */

/**
 * `appearance` on a page is the native cache identity of its `cleaned`
 * tiles (`tile::page_appearance`), read out of the manifest so it survives a
 * reload and a reopen. The editor draws a page as `source` tiles and a layer
 * per patch instead (`api/tile.js#layerUrl`); the flattened tiles are the
 * home screen's.
 *
 * `kind` and `text` are stand-in image data (what the mock canvas paints in
 * place of a scan). `tool` records which tool last committed the region, for
 * the "reopen in tool" action.
 *
 * `insideBubble` (on `Region`) is real data: a stored detection's balloon
 * answer, `DetectedRegion.inside`, which picks the colour its mask is drawn
 * in. The mock derives it from `kind`.
 *
 * @typedef {import('../model/types.js').Region & {
 *   sourceSha: string,
 *   detected: boolean,
 *   kind: 'bubble'|'sfx'|'outside',
 *   text: string,
 *   tool?: string,
 *   paddingPx?: number
 * }} ApiRegion
 */

/**
 * The undo journal's index - everything the interface holds about a history.
 * The deltas themselves stay on disk.
 *
 * @typedef {Object} HistoryView
 * @property {number} cursor - how many entries are in the past
 * @property {Array<{seq: number, label: string}>} entries
 */

/**
 * @typedef {Object} NoticeSpec
 * @property {string} key - i18n key
 * @property {Object} params - named placeholders; a value may itself be an i18n key (`reasonKey`, `causeKey`, `rungKey`, `fillModeKey`, `modeKey`)
 * @property {'info'|'warn'} tone
 */

/* ------------------------------------------------------------------ */
/* Events                                                              */
/* ------------------------------------------------------------------ */

/**
 * A page has been taken off the queue and is being cleaned. The Pages list
 * shows it as `●`.
 *
 * Not one of the four event types the brief names; added because the Pages
 * list is the progress indicator and there is otherwise no
 * way to say "this page is cleaning now". `runClean` also returns the queue
 * in order, so a consumer that ignores this event can still derive it.
 *
 * @typedef {Object} PageStartedEvent
 * @property {'page-started'} type
 * @property {string} runId
 * @property {string} chapterId
 * @property {string} pageId
 * @property {number} pageIndex
 */

/**
 * One region finished. Emitted for every region of a page, in page order,
 * before that page's `page-done`.
 *
 * @typedef {Object} RegionDoneEvent
 * @property {'region-done'} type
 * @property {string} runId
 * @property {string} chapterId
 * @property {string} pageId
 * @property {number} pageIndex
 * @property {ApiRegion} region - the region in its finished state, mask and all
 */

/**
 * A page finished. The page's mark ticks over in the Pages list.
 *
 * @typedef {Object} PageDoneEvent
 * @property {'page-done'} type
 * @property {string} runId
 * @property {string} chapterId
 * @property {string} pageId
 * @property {number} pageIndex
 * @property {ApiPage} page - the page in its finished state
 */

/**
 * The run stopped, either because the queue emptied or because it was
 * cancelled. A cancelled run keeps every completed region and leaves the job
 * incomplete; `nextPageIndex` is where a resume
 * would pick up.
 *
 * @typedef {Object} RunFinishedEvent
 * @property {'run-finished'} type
 * @property {string} runId
 * @property {string} chapterId
 * @property {'completed'|'cancelled'} reason
 * @property {number} pagesQueued
 * @property {number} pagesCleaned
 * @property {number} regionsCleaned
 * @property {number|null} nextPageIndex - set only when cancelled
 */

/**
 * A transient notice for the bottom-left stack. `key` and
 * `params` are i18n data; this layer never emits English text.
 *
 * @typedef {Object} NoticeEvent
 * @property {'notice'} type
 * @property {string} id - stable key for the notice stack
 * @property {string} key
 * @property {Object} params
 * @property {'info'|'warn'} tone
 */

/**
 * A model or the ONNX Runtime being fetched.
 *
 * The sixth event type, and it is one for the same reason `page-started` is:
 * progress arrives on the event channel and nowhere else, because a Tauri
 * `invoke` cannot return a streaming result and a 207 MB download has to be
 * watchable and cancellable while it runs.
 *
 * `id` is a catalogue row's id or `'runtime'`. `total` is the response's
 * `Content-Length` and may be absent. **Exactly one event per download carries
 * `done`**, whatever happened: `error` is null for a file that arrived and
 * verified, and a string for a failure, a digest mismatch or a cancellation -
 * the row goes back to "not installed" through the same path for all three.
 *
 * @typedef {Object} ModelProgressEvent
 * @property {'model-progress'} type
 * @property {string} id
 * @property {number} downloaded
 * @property {number|null} total
 * @property {boolean} done
 * @property {string|null} error
 */

/**
 * @typedef {PageStartedEvent|RegionDoneEvent|PageDoneEvent|RunFinishedEvent|NoticeEvent|ModelProgressEvent} BackendEvent
 */

/* ------------------------------------------------------------------ */
/* The interface                                                       */
/* ------------------------------------------------------------------ */

/**
 * @typedef {Object} RunHandle
 * @property {string|null} runId - null when nothing was queued
 * @property {Array<{chapterId: string, pageId: string, pageIndex: number}>} pages - the queue, in order
 * @property {boolean} [alreadyRunning] - the chapter already has a run: `runId` is that run, and nothing new was queued
 * @property {boolean} [atCapacity] - as many runs as the backend allows are going (`runId` null); nothing was queued
 */

/**
 * `pageStatus` is the status of the page the region sits on, as the backend
 * holds it *after* the edit. Applying a tool to a region on an unclean page
 * makes it a cleaned page, so a caller that replaced only the region would draw
 * a full track under a "not cleaned" mark - the same reason `deleteMask`,
 * `rerunMask` and `cleanAnyway` all report it. Present on `'applied'` only.
 *
 * `needs-confirmation` is the answer to a cloud engine named without a grant:
 * the interface asks for consent (`editor/cloudflow.svelte.js`) and calls
 * again with one. A cloud render that did not commit answers `failed`,
 * `cancelled` or `unknown`, with the reason in `errorCode`.
 *
 * @typedef {Object} ApplyResult
 * @property {'applied'|'needs-confirmation'|'blocked'|'run-started'|'not-found'|'failed'|'cancelled'|'unknown'} status
 * @property {string} [errorCode] - why a cloud render did not commit
 * @property {ApiRegion} [region]
 * @property {import('../model/types.js').Mask} [mask]
 * @property {string} [pageStatus]
 * @property {string|null} [runId]
 * @property {Array<{chapterId: string, pageId: string, pageIndex: number}>} [pages]
 */

/**
 * `exportChapter` refuses more than one thing, and every refusal is the same
 * shape: `{status: 'refused', reasonKey}`, with the sentence in the catalogue
 * and the backend announcing it on the notice stack. Callers branch on
 * `reasonKey`, never on `status` alone - only
 * `notice.export.refusedOverwrite` has a way out the interface can offer
 * (choose another folder), and the rest are answered by changing what was
 * asked for.
 *
 * The refusals are: a lossy format (`refusedLossyFormat`), PSB
 * (`refusedLayeredFormat`), a format string the backend does not know
 * (`refusedUnknownFormat`), `masks: 'separate-layer'` into a CBZ
 * (`refusedMaskLayers`), a destination that is neither sentinel nor an
 * absolute path (`refusedDestination`), a stitched export of a paginated
 * project (`refusedStitchPaginated`), into a CBZ (`refusedStitchedArchive`) or
 * as PSD (`refusedStitchedLayered`), a PSD of a page PSD cannot carry
 * (`refusedLayeredMode`, `refusedLayeredSize`), and the five
 * `notice.export.stitchRefused.*` a strip whose pages disagree produces.
 * The seam contract is the list this restates.
 *
 * `masks` means two things by format. For PNG and TIFF, `'separate-layer'`
 * writes a mask file beside each page - `001.png` and `001_mask.png`. For PSD
 * it is the layered document: the untouched page as the Background, one
 * masked layer per region in a `Cleaned` group; `'flattened'` is one
 * Background layer of the cleaned page.
 *
 * The whole surface. Implementations: `mock.js` today, a Tauri adapter later.
 *
 * The region-level edits - `deleteMask`, `rerunMask`, `cleanAnyway` - return
 * the page's status alongside the region, because some of them move it:
 * cleaning a gate-skipped region on an unclean page makes it a cleaned page,
 * and deleting the last mask on a page takes that back. A caller that replaced
 * only the region would draw a full track under a "not cleaned" mark. Their
 * undo half, `restoreRegion`, takes the status back the same way.
 *
 * @typedef {Object} Backend
 * @property {(handler: (event: BackendEvent) => void) => (() => void)} subscribe - returns an unsubscribe function
 * @property {() => Promise<ApiProject[]>} listProjects
 * @property {(spec: {name: string, mode: 'single'|'longstrip', sourcePath?: string, readingDirection?: 'rtl'|'ltr'}) => Promise<ApiProject>} createProject
 * @property {(spec: {projectId: string, name: string, number?: number, sourcePath?: string}) => Promise<ApiChapter|null>} createChapter - `number` is the chapter's number and is stored verbatim; omitted, the backend numbers it `max + 1`. `sourcePath` omitted means "wherever the project's own folder implies", and a backend must refuse rather than give one folder to two chapters
 * @property {(spec: {projectId: string, chapterId: string, convert?: boolean}) => Promise<{project: ApiProject, chapter: ApiChapter, pendingConversion: {from: string, to: string, fileCount: number}|null}|null>} openChapter - the chapter arrives as page **headers** plus its review index; `loadPages` brings a window's regions
 * @property {(spec: {chapterId: string, indices: number[]}) => Promise<ApiPage[]>} loadPages - the regions of a window of pages. Indices that are not there are skipped, never refused
 * @property {(spec: {chapterId: string}) => Promise<HistoryView>} historyLoad - the chapter's undo journal as an index: a cursor and one `{seq, label}` per entry, no payloads
 * @property {(spec: {chapterId: string, entry: Object}) => Promise<HistoryView>} historyPush - record a delta that has already been applied; truncates the redo stack, caps the journal, and answers with the index as it now stands
 * @property {(spec: {chapterId: string, direction: 'undo'|'redo'}) => Promise<{cursor: number, entry: Object|null}>} historyMove - move the journal's cursor and answer with the one delta to replay
 * @property {(spec: {projectId: string, name: string}) => Promise<ApiProject|null>} renameProject
 * @property {(spec: {projectId: string}) => Promise<boolean>} deleteProject
 * @property {(spec: {projectId: string, chapterId: string, sourceFiles?: boolean}) => Promise<boolean>} deleteChapter - always removes the chapter's row and the library's own files for it (its manifest, masks and patches). `sourceFiles: true` also deletes the folder of scans it was read from - except the project's own folder, which a backend must refuse to remove so that deleting one chapter cannot empty the project. `false` means the chapter was not there
 * @property {(spec: {projectId: string, chapterId?: string}) => Promise<{project: ApiProject, chapter: ApiChapter, resumedFrom: number, runId: string|null, pages: Array<Object>, mode?: RunMode}|null>} resumeJob
 * @property {(spec: {scope: 'page'|'chapter'|'project', mode?: RunMode, chapterId: string, pageIndex?: number, engineCeiling?: string, bubbleEngine?: string, outsideEngine?: string, outsideBubbles?: 'review'|'clean', bubbleColor?: string, maskPaddingPx?: number, detection?: Record<string, string|null>, detectorModels?: string[], geometryPolicy?: 'legacy'|'text_shape', textPolicy?: 'legacy_gate'|'all_text', ocrRescue?: boolean, analysisTargets?: {rtFull?: 'local'|'cloud', samTs?: 'local'|'cloud'}, cloudGrant?: string, area?: {x: number, y: number, w: number, h: number}}) => Promise<RunHandle>} runClean - selected models, source languages, geometry and text policy are captured for the native run. A stage `analysisTargets` routes to the cloud needs `cloudGrant` from `confirmRunAnalysis`; without it the run is refused (`cloud_run_grant_required`). `mode` is the step (docs/detect-clean.md §3), `auto` when absent. `maskPaddingPx` grows every fitted mask by that many page pixels, 0 to 32 (`mask_padding_invalid` past it), 0 when absent. `area` (page percent, as a region's `bbox`) holds a `detect` of one page to that area: the models read a window around it at the page's own resolution, and what they find there is added to the page's detections, less what the page holds already; nothing stored is replaced and the page is not marked detected. Refused with `detect_area_invalid` off the page, and `detect_area_scope_unsupported` for any other mode or scope
 * @property {(spec?: {runId?: string}) => Promise<string|null>} cancelRun
 * @property {(spec: {tool: string, params?: Object, chapterId?: string, pageIndex?: number, regionId?: string}) => Promise<ApplyResult>} applyTool
 * @property {(spec: {chapterId: string, pageIndex: number, sourceIndex?: number, sourceSha?: string, tool: string, params: Object, requestId: number, revision: string}) => Promise<{requestId: number, revision: string, chapterId: string, pageIndex: number, bounds: {x:number,y:number,w:number,h:number}, png:number[]}|null>} previewPaint
 * @property {(spec: {chapterId: string, pageIndex: number, sourceIndex?: number, sourceSha?: string, bbox: {x: number, y: number, w: number, h: number}, tool: string, params?: Object}) => Promise<{region: ApiRegion, pageStatus: string}|null>} createRegion - a region drawn by hand, with a hand mask already committed
 * @property {(spec: {chapterId: string, pageIndex: number, sourceIndex?: number, sourceSha?: string, mode: 'add'|'remove', stroke?: {points: Array<{x: number, y: number}>, radius: number}, painted?: {kind: 'rect'|'ellipse'|'polygon', points: Array<{x: number, y: number}>, feather: number}}) => Promise<{pageStatus: string, changed: string[], created: string[], removed: string[]}|null>} editDetectionMask - add a gesture to the detected masks on a page or take it away from them; the edited masks are what Clean erases. `add` merges into the detected region the gesture overlaps most, never giving a pixel to two regions, or creates a new detected region where it touches none; `remove` subtracts it from every detected region it touches and deletes any left empty. Cleaned layers never change, and a longstrip gesture is clipped to the page it starts on. `stroke` is page percent with its radius in page pixels, `painted` a shape in page percent. No undo: a detection has no pixels to put back. Rejects with a code (`mask_edit_mode_invalid`, or the chapter-busy code a run holds)
 * @property {(spec: {chapterId: string, pageIndex?: number, regionId?: string, paddingPx: number}) => Promise<{changed: string[], removed: string[], pages: number[]}|null>} setDetectionPadding - an optional regionId restricts the change to one detection in the requested page/chapter; an invalid target rejects with mask_padding_target_invalid. Otherwise grow every stored detection on a page, or on every page of the chapter when `pageIndex` is absent, to `paddingPx` page pixels past its unpadded mask (0 to 32). Always grown from the masks Detect or the last mask edit left, so 0 gives those back. Detections a padding above 0 runs together (overlapping anywhere, or within 4 page pixels) become one: the earliest in reading order keeps its id and takes the others' masks, the others leave the page and are answered in `removed`, and a smaller padding later shrinks the one they became. Cleaned layers never change. Answers the detections that changed, the ones another took in, and their page indices, `null` for a chapter that is not there. Rejects with `mask_padding_invalid`, or the chapter-busy code a run holds
 * @property {(spec: {maskId: string}) => Promise<{region: null, pageStatus: string}|null>} deleteMask - deleting a mask deletes the row: the text under it comes back and the region goes off the page, so there is no region to answer with. `null` (rather than `{region: null}`) means the mask was not found
 * @property {(spec: {regionId: string, region: ApiRegion|null, pageStatus?: string}) => Promise<ApiRegion|null>} restoreRegion - put a region back as it was; the undo half of every region-level edit. `region: null` means it was not there
 * @property {(spec: {regionId: string, layer: {opacity: number, offsetX: number, offsetY: number, rotation: number, locked: boolean}}) => Promise<ApiRegion|null>} setLayerStyle - change one layer's opacity, placement or lock; rejects with a `masks.refused.*` key when the layer's `mask.capabilities` do not allow it (a fixed redraw moved, a locked layer moved, a detection styled)
 * @property {(spec: {regionId: string}) => Promise<ApiRegion|null>} keepDependencyResult - accept a later committed result after its earlier input changed
 * @property {(spec: {regionId: string, inside: boolean}) => Promise<ApiRegion>} setDetectionType - set a stored detection's text type, inside a speech bubble or outside one, before it is cleaned; answers the region as it now is. The balloon colour Detect measured is cleared, and a Fill or Solid pick moved outside a balloon becomes LaMa; any other pick stays. The page's status does not move. No undo, as with `editDetectionMask`. Rejects with `not_a_detection` for an id that is not a stored detection (a cleaned layer, a held row, a region gone), or with the chapter-busy code a run holds
 * @property {(spec: {maskId: string, kind: 'stronger'|'simpler'|'cycleFill'|'reopenInTool'|'retry'|'retryWider'|'engine', engine?: string, params?: CloudRunParams}) => Promise<{region: ApiRegion, mask: import('../model/types.js').Mask, reopenTool: string|null, pageStatus: string}|null>} rerunMask - `engine` names the rung `kind: 'engine'` runs at; `'retry'` re-runs the rung the mask already used with the same hole, `'retryWider'` with the stored mask as the hole (wider on each press)
 * @property {(spec: {regionId: string, engine?: string, params?: CloudRunParams}) => Promise<{region: ApiRegion, mask: import('../model/types.js').Mask, pageStatus: string}|null>} cleanAnyway - `engine` is the starting rung: the user's `fill`/`redraw` pick for this kind of text, or a rung named outright. With `params.exact`, a model picked from Clean with: run as named, not scored against. The automatic pass never cleans an out-of-balloon region; this is where the pick for one bites
 * @property {(opts: Object) => Promise<Object|null>} planExportChapter - read-only validated output plan, declarations and source/patch revision
 * @property {(opts: {planRevision?: string, chapterId: string, format?: string, destination?: 'new-folder'|'source-folder'|string, masks?: 'flattened'|'separate-layer', layout?: 'per-page'|'stitched'}) => Promise<{status: 'exported'|'refused', fileCount?: number, path?: string, reasonKey?: string, gutterPixels?: number}|null>} exportChapter - `format` is `'PNG' | 'TIFF' | 'PSD' | 'CBZ'`; `destination` also takes an absolute path; `layout: 'stitched'` is longstrip only and never PSD; `masks: 'separate-layer'` is a mask file beside each raster page or a layer per region in a PSD, and is refused for CBZ; `gutterPixels` comes back on a stitched export alone
 * @property {() => Promise<Object>} readSettings
 * @property {(patch: Object) => Promise<Object>} writeSettings
 * @property {() => Promise<InferenceConfig>} readInferenceConfig - read public inference configuration from `inference.json`
 * @property {(spec: { config: InferenceConfig }) => Promise<InferenceConfig>} writeInferenceConfig - validate and atomically persist public inference configuration to `inference.json`
 * @property {(spec: { provider: 'beam'|'modal', profileId: string }) => Promise<InferenceConfig>} selectCloudProfile - make a configured profile the default cloud target, read and written under the native writer's lock; work running on another profile goes on
 * @property {(spec: { provider: 'beam'|'modal', profileId: string, role: SecretRole, secret: string, tokenId?: string, sessionOnly?: boolean }) => Promise<SecretSummary>} storeCloudSecret - store cloud credential into OS keyring or session store (write-only)
 * @property {(spec: { provider: 'beam'|'modal', profileId: string, role: SecretRole }) => Promise<SecretSummary>} deleteCloudSecret - delete cloud credential from OS keyring and session store
 * @property {(spec: { provider: 'beam'|'modal', profileId: string, role: SecretRole }) => Promise<SecretSummary>} getCloudSecretSummary - query safe summary of cloud credential presence
 * @property {() => Promise<void>} forgetCloudSecretDenials - forget cancelled keychain prompts so the next credential read may ask again; only on an explicit action such as opening Settings > Cloud, never from a poll
 * @property {(spec: { provider: 'beam'|'modal', profileId: string }) => Promise<import('../model/types.js').CloudConnectionStatus>} checkCloudConnection - test control-plane reachability (ordinary check; never triggers GPU work)
 * @property {(spec: { provider: 'beam'|'modal', profileId: string }) => Promise<{expected: string, deployed: string|null, current: boolean}>} checkCloudRelease - whether a setup runs the cloud code this app's helper would deploy (`current`); starts no GPU
 * @property {(spec: { provider: 'beam'|'modal', profileId: string }) => Promise<import('../model/types.js').CloudModelInfo>} getCloudModelInfo - query wire metadata, model revision, and provisional service limits
 * @property {(spec: { provider: 'beam'|'modal', profileId: string }) => Promise<CloudGpuStatus>} getCloudGpuStatus - which GPU containers this configured deployment has up; never starts one
 * @property {(spec: { provider: 'beam'|'modal', profileId: string, role?: 'render'|'analysis' }) => Promise<CloudGpuStop>} stopCloudGpu - stop one GPU container of this configured deployment, or all without role; that container's work in flight ends cancelled
 * @property {(spec: { target: ExecutionTarget, recipe?: import('../model/types.js').RenderRecipe, intent: import('../model/types.js').OperationIntent, regionId?: string, chapterId?: string, pageIndex?: number, simulateBlocked?: boolean }) => Promise<import('../model/types.js').ConsentProposal>} prepareCloudConsent - prepare backend-authorized consent proposal with exact crop/hint digests
 * @property {(spec: { proposalId: string, intent: import('../model/types.js').OperationIntent, rightsAttested?: boolean, retentionAcknowledged?: boolean, simulateEpochMismatch?: boolean }) => Promise<import('../model/types.js').Grant>} confirmCloudConsent - validate proposal and mint scoped attempt-limited authorization grant. Both statements are required unless the project's standing consent covers the proposal (`rights_attestation_required`, `retention_acknowledgement_required`). A grant confirmed with both records the project's standing consent, as `confirmRunAnalysis` and `confirmCloudClean` do
 * @property {(spec: { attemptId: string, grantNonce: string, proposalId?: string, target?: ExecutionTarget, recipe?: import('../model/types.js').RenderRecipe, simulateMode?: 'blocked_authorization'|'ambiguous_acceptance', snapshot?: Object }) => Promise<import('../model/types.js').CloudAttemptSubmission>} submitCloudAttempt - persist durable intent and submit cloud attempt (never auto-retried if ambiguous)
 * @property {(spec: { attemptId: string, handle?: string }) => Promise<import('../model/types.js').CloudAttemptStatus>} getCloudAttemptStatus - poll authoritative remote attempt lifecycle status
 * @property {(spec: { attemptId: string, handle?: string }) => Promise<import('../model/types.js').CloudAttemptResult>} getCloudAttemptResult - retry-safe, idempotent retrieval of validated output crop
 * @property {(spec: { attemptId: string, handle?: string }) => Promise<import('../model/types.js').CloudCancelResult>} cancelCloudAttempt - request nonterminal attempt cancellation
 * @property {(spec?: { apply?: boolean, action?: 'abandon'|'acknowledge', acceptDuplicateRisk?: boolean, attemptId?: string, chapterId?: string, pageIndex?: number, regionId?: string, regionRevision?: number|string, sourceImageHash?: string, simulateStale?: boolean }) => Promise<import('../model/types.js').CloudRecoveryDecision|CloudRecoveryReport>} reconcileCloudRecovery - evaluate crash discovery against the local project. With `apply: true` it recovers - resumes accepted attempts, attaches cached results, never resubmits an unknown one - and answers a `CloudRecoveryReport`
 * @property {() => Promise<{available: boolean, reasonKey: string|null}>} sidecarAvailable - whether rung 3a (the FLUX sidecar) can be offered on this machine. `reasonKey` is null when there is nothing to say, which is the ordinary case of nothing installed
 * @property {() => Promise<Array<{id: string, label: string}>>} listSidecarModels - list available model directories discovered under the sidecar weights root
 * @property {(spec: {backend: 'auto'|'mflux'|'sdnq', accelerator: 'auto'|'cuda'|'xpu'|'mps'}) => Promise<{backend: string, accelerator: string, model: string, root: string}>} installFluxHelper - install the optional local FLUX process and pinned 4B weights
 * @property {() => Promise<{appVersion: string, facts: Array<{labelKey: string, value: string}>}>} about
 * @property {() => Promise<{appVersion: string, components: Array<{name: string, available: boolean, detail: string|null, reasonKey: string|null}>}>} diagnostics - whether each component is usable **now**, found by trying: the `onnxruntime` row loads the runtime the way a run does, so an installed runtime that will not load says so here and nowhere else. `reasonKey` is a `diagnostics.runtime.*` key when it is not usable; `detail` is the loader's own words, for a log and never for the screen
 * @property {() => Promise<LoadedModel[]>} listLoadedModels - what is in memory **right now**. A poll, not a subscription: the answer changes with the work rather than with an event, and an empty array is the ordinary state of an application that is not cleaning anything
 * @property {(spec: {id: number}) => Promise<boolean>} unloadModel - ask for one back. `true` means the request is recorded, **not** that the memory is free: a backend drops the session at its next safe point, and anything the work still needs loads again
 * @property {() => Promise<Accelerators>} listAccelerators - what this machine can run models on, which provider each model will land on under the current setting, and why. Not a poll: the answer moves when the runtime is downloaded or the `accelerator` setting is written
 * @property {(spec?: {retryStore?: boolean}) => Promise<ModelsView>} listModels - the model catalogue and what of it is on this machine. Cheap by construction - presence and size, never a digest. `retryStore` is "Settings › Models has just been opened" and nothing else: it lets the once-per-process token migration be attempted again, for a keychain unlocked since launch. A refresh or a poll must not pass it
 * @property {(spec: {id: string}) => Promise<DownloadStart>} downloadModel - start one. `'alreadyRunning'` and `'alreadyInstalled'` say why one did not start, which is what a second window's stale row needs to hear. Progress arrives as `model-progress`, never as this promise
 * @property {(spec: {id: 'scriptGate'|'mangaOcr'|'hayaiOcr'}) => Promise<DownloadStart>} downloadModelGroup - start a logical multi-file language gate or Japanese OCR group
 * @property {(spec: {id: string}) => Promise<boolean>} cancelDownload - ask one to stop; it ends with a `done` event carrying an error, the same way a failure does
 * @property {(spec: {id: string}) => Promise<DeleteOutcome>} deleteModel - remove it from the app-data models directory **only**. `'readOnlyElsewhere'` is a copy this app does not own and `'notFound'` a row that was already stale; a successful delete also drops any session that weight was loaded into
 * @property {(spec: {id: 'scriptGate'|'mangaOcr'|'hayaiOcr'}) => Promise<DeleteOutcome>} deleteModelGroup - remove a logical multi-file language gate or Japanese OCR group
 * @property {(spec: {id: string}) => Promise<boolean>} verifyModel - digest an installed weight against its pin. Deliberately explicit: it is seconds of work, which is why `listModels` does not do it
 * @property {(spec: {id: 'scriptGate'|'mangaOcr'|'hayaiOcr'}) => Promise<boolean>} verifyModelGroup - verify every installed file in a logical model group
 * @property {() => Promise<Object>} listWorkflowCapabilities - independent RT, SAM, COO and backend states
 * @property {(spec: {sourcePath: string}) => Promise<boolean>} importFullRt
 * @property {() => Promise<boolean>} removeFullRt
 * @property {(spec: {sourceDir: string}) => Promise<boolean>} importSamTs - install verified local ONNX graphs as a logical pair
 * @property {() => Promise<boolean>} installSamTs - download pinned upstream weights and export verified SAM-TS-L graphs locally
 * @property {() => Promise<boolean>} removeSamTs
 * @property {() => Promise<boolean>} verifySamTs
 * @property {(spec: {sourcePath: string, workflow: string, rtProfile: 'full-halves'|'small-whole', rtBackend: string, samBackend: string}) => Promise<Object>} analyzeCapabilities - read-only review evidence
 * @property {(spec: {chapterId: string, pageIndex: number, workflow: string, rtProfile: 'full-halves'|'small-whole', rtBackend: string, samBackend: string}) => Promise<Object>} analyzeChapterPage - read-only review evidence for a source page already in the chapter
 * @property {(spec: {analysisId: string, chapterId: string, pageIndex: number, componentId: string, allowOutsideBubbles?: boolean, paddingPx?: number, additions?: {bounds: {x: number, y: number, w: number, h: number}, bits: number[]}, removals?: {bounds: {x: number, y: number, w: number, h: number}, bits: number[]}, correctionRevision?: number}) => Promise<Object>} prepareComponentWrite - exact pinned component support preview, only when SAM writing is qualified
 * @property {(spec: {analysisId: string, chapterId: string, pageIndex: number, componentId: string}) => Promise<{regionId: string, additions: {bounds: {x: number, y: number, w: number, h: number}, bits: number[]}, removals: {bounds: {x: number, y: number, w: number, h: number}, bits: number[]}, paddingPx: number, correctionRevision: number, planRevision: number}|null>} loadComponentCorrection - load the persisted correction draft for a current SAM component analysis
 * @property {(spec: {planId: string, approvedSupportSha256: string}) => Promise<Object>} applyComponentWrite - commit one explicitly approved support raster
 * @property {(spec: {id: string}) => Promise<boolean>} discardPartial - throw away the unfinished download the row reports as `partialBytes`, and say whether there was one. `false` is also what a transfer in flight answers: its `.part` is a file being written and is not deleted out from under it
 * @property {() => Promise<DownloadStart>} downloadRuntime - fetch and unpack the ONNX Runtime build this platform is set to; reports under the id `runtime`
 * @property {() => Promise<DeleteOutcome>} deleteRuntime - remove it from the app-data runtimes directory only, with the same three answers `deleteModel` gives
 * @property {(spec: { op: string, provider: string, params?: Object }) => Promise<Object>} runCloudProvisioner - execute bounded cloud provisioner helper operation (inspect/plan/apply/resume/cleanup/probe) with secrets via stdin only. `inspect` also lists installations already in the account (`ProvisionInspectDiscovery`)
 * @property {() => Promise<unknown>} cancelCloudProvisioner - stop the helper that is running. Its journal makes a later `resume` safe, so a stop loses nothing already created
 * @property {(handler: (event: ProvisionProgress) => void) => Promise<() => void>} onProvisionProgress - `provision://progress`: one event per step the helper starts, finishes, skips or fails. Answers with a promise of the unlisten function
 * @property {(spec: {attemptId: string, choice: 'use'|'discard'|'retry'}) => Promise<void>} resolveQwenReview
 * @property {(handler: (preview: any) => void) => Promise<() => void>} onQwenReview
 * @property {(handler: (event: CloudAttemptEvent) => void) => Promise<() => void>} onCloudAttempt - `cloud://attempt`: one event per phase of an interactive cloud render. Answers with a promise of the unlisten function
 * @property {(spec: {provider: 'modal'|'beam', profileId: string}) => Promise<RemoteAnalysisCapabilities>} listRemoteAnalysisCapabilities - which analysis models the selected endpoint advertises. Reads the model list only; no page data is sent
 * @property {(spec: {chapterId: string, pageIndex: number, provider: 'modal'|'beam', profileId: string, capability: string, companion?: string|null, regions?: Array<{x: number, y: number, width: number, height: number}>}) => Promise<Object>} proposeRemoteAnalysis - state exactly which tiles of one page would be sent, to which models (`capability`, then `companion` for the same tiles). Sends nothing
 * @property {(spec: {proposalId: string, rightsAttested: boolean, retentionAcknowledged: boolean}) => Promise<Object>} confirmRemoteAnalysis - send the proposal's tiles one by one and answer with review-only evidence
 * @property {(spec: {proposalId: string}) => Promise<boolean>} cancelRemoteAnalysis - discard an open proposal or stop a running one after its tile in flight
 * @property {(spec: {proposalId: string}) => Promise<Object>} getRemoteAnalysisStatus - the proposal's journal record, recovered after a restart
 * @property {(handler: (record: Object) => void) => Promise<() => void>} onRemoteAnalysis - `cloud://analysis`: one snake_case journal record per phase of a review analysis
 * @property {(spec: {chapterId: string, scope?: 'page'|'chapter'|null, pageIndices?: number[]|null, capabilities: string[], provider: 'modal'|'beam', profileId: string}) => Promise<RunAnalysisProposal>} proposeRunAnalysis - state what an Auto clean run would send to the cloud for detection: the pages the run of `scope` would walk (`page` takes the one index in `pageIndices`), or `pageIndices` as given with no scope. Reads the model list and page sizes; sends nothing
 * @property {(spec: {proposalId: string, rightsAttested: boolean, retentionAcknowledged: boolean}) => Promise<RunAnalysisGrant>} confirmRunAnalysis - mint the single-use, short-lived run grant `runClean` spends as `cloudGrant`. A `standing` proposal is confirmed with `false` for both: the native side holds the statements the project's consent was given with
 * @property {(spec: {proposalId: string}) => Promise<boolean>} cancelRunAnalysis - discard an unconfirmed run proposal
 * @property {(spec: {chapterId: string, scope: 'page'|'chapter'|'regions', pageIndices?: number[]|null, regionIds?: string[]|null, localFirst?: boolean, bubbleEngine?: string|null, outsideEngine?: string|null, qwenEdit?: {target: 'auto'|'dialogue'|'sound_effect'|'other', description: string}}) => Promise<CloudCleanProposal>} prepareCloudClean - describe the plan for the stored detections in scope: the exact ordered regions, the batches they are sent in, and the estimate from each region's crop. Cleans and sends nothing. `bubbleEngine` and `outsideEngine` are Text cleanup's picks for this clean: each one given is saved onto the regions on its side of a balloon first (the only write), which decides what the mixed choice tries here first; a side not given keeps the pick each region was saved with. `localFirst` (default false) asks for mixed execution: once the run starts, fill and solid picks are tried on this computer first
 * @property {(spec: {proposalId: string, planDigest: string, rightsAttested: boolean, retentionAcknowledged: boolean}) => Promise<CloudCleanGrant>} confirmCloudClean - mint the single-use, short-lived grant `startCloudClean` spends, only for the plan the consent showed (`planDigest`, else `cloud_clean_plan_mismatch`). A `standing` proposal, or one that sends nothing, is confirmed with `false` for both
 * @property {(spec: {grantId: string}) => Promise<RunHandle>} startCloudClean - start the render run in the ordinary run slot. Ordinary run events; `cancelRun` stops it
 * @property {(spec: {proposalId: string}) => Promise<boolean>} cancelCloudClean - discard an unconfirmed cloud clean proposal
 * @property {() => Promise<Array<{id: string, targets: string[], recipe: Object}>>} denoisePresets - the page denoise presets the backend knows, from its own copy of `model/denoise-presets.json`
 * @property {(spec?: {presetId?: string}) => Promise<number|{secondsPerPage: number}>} benchmarkDenoiseLocal - denoise a reference page on this computer and answer the seconds one page took. Needs the local denoise model installed
 * @property {(spec: {runId: string, chapterId: string, pageIndices?: number[]|null, presetId: string, outDir: string}) => Promise<DenoiseReport>} denoiseChapterLocal - denoise the chapter's pages (every page when `pageIndices` is null) on this computer into `outDir`. `runId` is the caller's name for the run: `onDenoiseProgress` events carry it and `cancelDenoiseLocal` takes it. Answers when every page is done or the run was stopped
 * @property {(spec: {runId: string}) => Promise<boolean>} cancelDenoiseLocal - stop a denoise, local or cloud: a local one at its next tile, a cloud one after the pages in flight. Pages already saved stay. Answers whether that run was in flight
 * @property {(spec: {chapterId: string}) => Promise<ReplaceReport>} replaceWithDenoised - take the files the chapter's last denoise wrote as its pages, for every page denoised from its bare source and not cleaned since. Detections and their masks stay; the other pages are kept and listed, those with no denoised file to take (never denoised, failed, or gone) as `missing`. When any page fails nothing is replaced
 * @property {(spec: {chapterId: string}) => Promise<DenoiseRun[]>} denoiseHistory - every denoise run the chapter's manifest remembers (the newest ten), newest first, with what each page has left
 * @property {(spec: {chapterId: string, pageIndex: number, run: number, side: 'raw'|'denoised'}) => Promise<ArrayBuffer|Uint8Array>} denoiseCompareImage - one side of the compare view as the file's bytes: the file run `run` wrote for the page, or the raw page it was made from. Serves only files the manifest recorded; rejects with `denoise_run_missing`, `denoise_page_missing` or `denoise_file_missing`
 * @property {(handler: (progress: DenoiseProgress) => void) => Promise<() => void>} onDenoiseProgress - `denoise://progress`: a local denoise after each tile and each page
 * @property {(handler: (recorded: {runId: string}) => void) => Promise<() => void>} onDenoiseRecorded - `denoise://recorded`: a finished denoise's pages are in its chapter's manifest, after a run walking that chapter let it go. A denoise answers without waiting for that
 * @property {(target?: {provider: 'modal'|'beam', profileId: string}) => Promise<string[]>} cloudDenoisePresets - the preset ids the selected cloud deployment can run now, from its own advertisement. Sends no page and starts no GPU. Rejects with `capability_unavailable: ...` when the deployment has no page denoise
 * @property {(spec: {chapterId: string, pageIndices?: number[]|null, recipe: Object}) => Promise<CloudDenoiseProposal>} prepareCloudDenoise - state what a cloud denoise of the chapter would send and cost. Sends nothing
 * @property {(spec: {proposalId: string, planDigest: string, rightsAttested: boolean, retentionAcknowledged: boolean}) => Promise<{grantId: string, expiresAtMs: number}>} confirmCloudDenoise - mint the single-use grant `startCloudDenoise` spends, only for the plan the consent showed. A `standing` proposal is confirmed with `false` for both
 * @property {(spec: {grantId: string, outDir: string, runId?: string}) => Promise<DenoiseReport>} startCloudDenoise - spend the grant and denoise its pages into `outDir`. `runId` names the run as a local one's does: `onDenoiseProgress` carries it and `cancelDenoiseLocal` stops it. Answers when every page is done or the run was stopped
 * @property {(spec: {proposalId: string}) => Promise<boolean>} cancelCloudDenoise - discard an unconfirmed cloud denoise proposal
 * @property {() => Promise<RunningJob[]>} listJobs - the runs and denoises going now, for a reloaded window or a reopened chapter to pick up
 * @property {() => Promise<void>} confirmQuit - the answer to a held quit: stop every job, then quit
 * @property {() => Promise<boolean>} hideToTray - the other answer: hide the window and let the jobs go on. Only while a tray exists
 * @property {(handler: (request: QuitRequest) => void) => Promise<() => void>} onQuitRequested - `app://quit-requested`: a quit was held because jobs are running
 */

/**
 * A job the backend is running (`list_jobs`).
 *
 * @typedef {Object} RunningJob
 * @property {string} runId
 * @property {'detect'|'clean'|'cloudClean'|'denoise'|'cloudDenoise'} kind
 * @property {string} chapterId
 * @property {number} done - pages finished
 * @property {number} total
 */

/**
 * A quit held by the quit guard (`app://quit-requested`).
 *
 * @typedef {Object} QuitRequest
 * @property {number} jobs - how many are running
 * @property {boolean} canHide - a tray exists, so `hideToTray` can keep them going
 */

/**
 * What `replaceWithDenoised` did, by page index. When `failed` is not empty,
 * `replaced` is empty: nothing was saved.
 *
 * @typedef {Object} ReplaceReport
 * @property {number[]} replaced
 * @property {number[]} kept - cleaned, or changed since denoise
 * @property {number[]} missing - no denoised file to take: never denoised, failed, or gone; they stay raw
 * @property {Array<{pageIndex: number, code: string}>} failed
 */

/**
 * What the chapter list carries of a chapter's denoise history
 * (`denoise_history.rs#summary`), null until it was denoised.
 *
 * @typedef {Object} DenoiseSummary
 * @property {number} runs - runs remembered
 * @property {number} latest - the newest run's `created`, seconds since the epoch
 * @property {boolean} taken - a run's file was taken as a page
 */

/**
 * One denoise run of a chapter (`denoise_history.rs`). `created` names it.
 * `preset` and `target` are null on a run recorded before they were;
 * `fromCleaned` is null when the history cannot say.
 *
 * @typedef {Object} DenoiseRun
 * @property {number} created - seconds since the epoch
 * @property {string|null} preset
 * @property {'local'|'cloud'|null} target
 * @property {boolean|null} fromCleaned
 * @property {string} folder - where the files were written
 * @property {Array<{pageIndex: number, exists: boolean, taken: boolean, current: boolean, fromCleaned: boolean|null}>} pages - in reading order
 */

/**
 * What a denoise run wrote and what it could not. A failed page never stops
 * the others; `code` is a stable failure code (`cloud_denoise_page_too_large`).
 *
 * @typedef {Object} DenoiseReport
 * @property {Array<{pageIndex: number, path: string, gray: boolean}>} written
 * @property {Array<{pageIndex: number, code: string}>} failed
 * @property {boolean} [cancelled] - the run was stopped before its last page; pages not reached are in neither list
 */

/**
 * How far a denoise is, local or cloud (`denoise://progress`).
 *
 * @typedef {Object} DenoiseProgress
 * @property {string} runId
 * @property {number} done - pages finished, saved or failed
 * @property {number} total
 * @property {number} page - how far through the page in progress, 0 to 1
 */

/**
 * What a cloud denoise would send (`inference/cloud_denoise.rs`).
 *
 * @typedef {Object} CloudDenoiseProposal
 * @property {string} proposalId
 * @property {string} chapterId
 * @property {number[]} pageIndices - every page the consent covers, in run order
 * @property {number} pages
 * @property {number[]} tooLargePages - pages in scope left out: past the gateway's limits for this recipe
 * @property {number} totalPixels
 * @property {Object} recipe
 * @property {string[]} modelIds - the pinned model of each step
 * @property {{low: number, high: number}} estimatedGpuSeconds
 * @property {{low: number, high: number}|null} estimatedCostUsd - null when the gateway states no price
 * @property {string|null} gpu
 * @property {'modal'|'beam'} provider
 * @property {string} profileName
 * @property {string} profileId
 * @property {string} planDigest
 * @property {number} expiresAtMs
 * @property {boolean} standing - the project already consented to this endpoint
 */

/**
 * Which step a run takes (docs/detect-clean.md §1): find and store the text
 * regions, clean the stored ones, or both.
 *
 * @typedef {'auto'|'detect'|'clean'} RunMode
 */

/**
 * What a cloud clean would render, for its consent dialog. `regions: 0` and a
 * null `proposalId` mean nothing can be sent: there was nothing in scope, or
 * every region in it has an unresolved earlier request (`unresolvedIds`) or is
 * too large for the render service (`tooLargeIds`).
 *
 * @typedef {Object} CloudCleanProposal
 * @property {string|null} proposalId
 * @property {string} chapterId
 * @property {string[]} regionIds - every region the consent covers, in the order they run
 * @property {number} regions
 * @property {number} pages
 * @property {number[]} [pageIndices]
 * @property {number} totalCropPixels
 * @property {number} [totalWorkPixels] - the same crops at the size the worker edits them
 * @property {number} localCleaned - always 0: preparing cleans nothing
 * @property {'cloud'|'mixed'} [execution]
 * @property {number} [localCandidates] - mixed only: fill and solid picks tried here first, after start
 * @property {number} [chunkRegions] - the batch size the plan is cut into
 * @property {number} [chunks]
 * @property {string[]} [unresolvedIds] - regions left out: an earlier cloud request for each is unresolved
 * @property {string[]} [tooLargeIds] - regions left out: each needs a crop past the render service's limits
 * @property {{low: number, high: number}|null} [estimatedGpuSeconds]
 * @property {string|null} [planDigest] - what the consent is given for
 * @property {{low: number, high: number}|null} estimatedCostUsd - null when the price is unknown
 * @property {string|null} gpu
 * @property {'modal'|'beam'} provider
 * @property {string} [profileId]
 * @property {string} profileName
 * @property {number} expiresAtMs
 * @property {boolean} [standing] - the chapter's project already consented to this endpoint, with both statements ticked: skip the question, not the grant
 */

/**
 * @typedef {Object} CloudCleanGrant
 * @property {string} grantId
 * @property {number} expiresAtMs
 */

/**
 * @typedef {Object} RemoteAnalysisCapabilities
 * @property {string} protocol_version
 * @property {Array<{capability: string, graph_sha256s: string[], model_revision: string}>} capabilities
 * @property {Object} limits
 */

/**
 * What a cloud-detection run would send, for the consent dialog.
 *
 * @typedef {Object} RunAnalysisProposal
 * @property {string} proposalId
 * @property {string} chapterId
 * @property {number[]} pageIndices
 * @property {'modal'|'beam'} provider
 * @property {string} profileId
 * @property {string} profileName
 * @property {string[]} capabilities
 * @property {Array<{capability: string, graphSha256s: string[], modelRevision: string}>} models
 * @property {number} pages
 * @property {number} totalTiles
 * @property {number} totalTilePixels
 * @property {number} sourceBytes
 * @property {boolean} includesSurroundingArt
 * @property {number|null} costEstimateUsd
 * @property {number} issuedAtMs
 * @property {number} expiresAtMs
 * @property {boolean} [standing] - the chapter's project already consented to this endpoint, with both statements ticked: skip the question, not the grant
 */

/**
 * @typedef {Object} RunAnalysisGrant
 * @property {string} grantId
 * @property {string} chapterId
 * @property {number[]} pageIndices
 * @property {string[]} capabilities
 * @property {number} expiresAtMs
 */

/**
 * One step of a running provisioner, as the core relays it from the helper's
 * progress lines. Every field is from a fixed set; no free text crosses.
 *
 * @typedef {Object} ProvisionProgress
 * @property {string} op - the helper op the step belongs to (`apply`, `resume`, `cleanup_apply`, ...)
 * @property {'modal'|'beam'} provider
 * @property {'inspect'|'validate'|'volume'|'state'|'secret'|'image'|'deploy'|'weights'|'token'|'endpoint'|'health'|'cleanup'} step
 * @property {'start'|'done'|'fail'|'skip'} state
 * @property {number|null} pct - 0..100 where the step can tell, null where it cannot
 */

/**
 * One installation of this app that `inspect` found already in the account
 * (`data.existing_installations`, rebuilt field by field by `provision.rs`).
 * Its id is its app name; an `apply` with that id reuses its app, storage and
 * downloaded models. Modal only: Beam answers an empty list.
 *
 * @typedef {Object} ProvisionExistingInstallation
 * @property {string} installation_id - `mc-...`, equal to `app_name`
 * @property {string} app_name
 * @property {number|null} created_at - unix seconds its storage was made
 * @property {number|null} deployed_at - unix seconds of its last deploy
 * @property {boolean} weights_checked - whether its storage could be listed
 * @property {string[]} models_ready - model ids whose download finished (ready marker present)
 * @property {string[]} analysis_ready - analysis capabilities whose graphs are in place
 * @property {{gpu: string, idle_seconds: number, model_id: string, analysis_models: string[], denoise: boolean, routing_region?: string}|null} options - the choices it recorded at apply time, when it did (`denoise` false when an older helper recorded none)
 * @property {boolean} on_this_computer - this computer's journal can resume it (Resume, not apply)
 */

/**
 * What `inspect` answers besides the account fields.
 *
 * @typedef {Object} ProvisionInspectDiscovery
 * @property {ProvisionExistingInstallation[]} existing_installations - newest first, at most 20
 * @property {boolean} existing_installations_complete - false when the list was cut short or could not be read
 */

/**
 * One phase change of an interactive cloud render.
 *
 * `attemptId` is `att-` plus the first 24 hex digits of the SHA-256 of the
 * grant's nonce, so the first event of a render already names it and a Cancel
 * can be offered from the first moment.
 *
 * @typedef {Object} CloudAttemptEvent
 * @property {string} attemptId
 * @property {string} regionId
 * @property {string} chapterId
 * @property {number} pageIndex
 * @property {'preparing'|'submitting'|'queued'|'running'|'downloading'|'reviewing'|'compositing'|'committed'|'failed'|'cancelled'|'unknown'} phase
 * @property {number} elapsedMs - since the render started
 * @property {string|null} errorCode - a stable snake_case code on `failed`, null otherwise
 */

/**
 * What `reconcileCloudRecovery({apply: true})` did at startup. Each entry names
 * where the attempt belongs; `reason` says why one needs a person.
 *
 * @typedef {Object} CloudRecoveryReport
 * @property {Array<{attemptId: string, chapterId: string, pageIndex: number, regionId: string}>} attached
 * @property {Array<{attemptId: string, chapterId: string, pageIndex: number, regionId: string, canAbandon?: boolean}>} stillRunning
 * @property {Array<{attemptId: string, chapterId: string, pageIndex: number, regionId: string, canAbandon?: boolean, reason: 'ambiguous'|'stale'|'failed'|'repair_needed'|'load_error'}>} needsAttention
 */

/**
 * One GPU container a cloud deployment has up (`inference/gpu.rs`). Times are
 * relative to the gateway's clock when it answered, never absolute.
 *
 * @typedef {Object} CloudGpuContainer
 * @property {'render'|'analysis'} role
 * @property {string} gpu - the provider's GPU name, e.g. `L4`
 * @property {'starting'|'idle'|'busy'} state
 * @property {number} idleSeconds - the deployment's scale-down window
 * @property {number} upForMs
 * @property {number|null} scaledownInMs - idle only: the provider's scale-down estimate
 * @property {number|null} listPriceUsdPerHour - list price in USD, an estimate for display
 */

/**
 * `supported: false` says nothing about the GPU: `outdated` is a deployment set
 * up before the GPU routes, `provider` one whose provider cannot report it.
 *
 * @typedef {Object} CloudGpuStatus
 * @property {boolean} supported
 * @property {'outdated'|'provider'|null} unsupported
 * @property {CloudGpuContainer[]} containers
 */

/**
 * @typedef {Object} CloudGpuStop
 * @property {boolean} supported
 * @property {Array<'render'|'analysis'>} stopped
 * @property {number} cancelledJobs
 */

/**
 * The four fields a cloud run carries, on every command that can start one:
 * the grant `confirmCloudConsent` minted, and the target, recipe and intent it
 * was minted for. A command given a grant it cannot match refuses the run.
 *
 * @typedef {Object} CloudRunParams
 * @property {string} grantNonce
 * @property {ExecutionTarget} executionTarget
 * @property {import('../model/types.js').RenderRecipe} recipe
 * @property {import('../model/types.js').OperationIntent} intent
 */

/**
 * Where a rendering or inference operation should execute.
 *
 * @typedef {{ type: 'local' } | { type: 'beam', profile_id: string } | { type: 'modal', profile_id: string }} ExecutionTarget
 */

/**
 * A validated public configuration profile for a cloud deployment (Beam or Modal).
 * Plaintext secrets (API tokens, workspace keys) never reside in public configuration.
 *
 * @typedef {Object} CloudProfile
 * @property {string} id
 * @property {string} name
 * @property {string} endpointUrl
 * @property {string} canonicalOrigin
 * @property {string} canonicalOriginFingerprint
 * @property {number} createdAtMs
 * @property {number} updatedAtMs
 */

/**
 * Public inference configuration persisted in `inference.json`.
 *
 * @typedef {Object} InferenceConfig
 * @property {number} schemaVersion
 * @property {ExecutionTarget} selectedTarget
 * @property {Record<string, CloudProfile>} beamProfiles
 * @property {Record<string, CloudProfile>} modalProfiles
 */

/**
 * The distinct roles a cloud credential can fulfill.
 *
 * @typedef {'setup'|'runtime'|'model_download'} SecretRole
 */

/**
 * The storage backend where a secret is retained.
 *
 * @typedef {'keyring'|'session'|'unavailable'} StorageBackendKind
 */

/**
 * Safe public summary of credential presence and storage backend.
 *
 * @typedef {Object} SecretSummary
 * @property {'beam'|'modal'} provider
 * @property {string} profileId
 * @property {SecretRole} role
 * @property {boolean} present
 * @property {StorageBackendKind} backend
 */

/**
 * Why a Download press did not start a download, or that it did.
 *
 * Both refusals are another window's doing: a second Settings dialog draws its
 * rows from a `listModels` taken before this one pressed anything, so its
 * Download button can be offered for something already running or already
 * here. Neither is an error and neither is a rejection - the remedy is a
 * refreshed list, which the interface asks for on the answer.
 *
 * @typedef {'started'|'alreadyRunning'|'alreadyInstalled'} DownloadStart
 */

/**
 * What a Delete press found.
 *
 * `'notFound'` is a stale row - nothing of that name on any search path -
 * and `'readOnlyElsewhere'` is a copy outside the directory the app owns, or
 * one the filesystem will not let go of. Both are the row's `readOnly` and
 * `installed` seen a moment later than they were drawn, so both are sentences
 * rather than failures.
 *
 * `'busy'` is a delete refused because a download is writing into the same
 * directory - the runtime's row is the only one that can reach it, because
 * `deleteRuntime` removes the tree a transfer's `.part` lives in.
 *
 * @typedef {'deleted'|'notFound'|'readOnlyElsewhere'|'busy'} DeleteOutcome
 */

/**
 * The accelerator setting's whole subject.
 *
 * `preference` is what is stored - `auto`, `cpu`, or a provider `id` - and is
 * never absent: an unwritten or unreadable value reads as `auto`, which is what
 * the backend will actually do.
 *
 * Every string is a key or an id, never English: `labelKey` names the provider,
 * `reasonKey` says why one is unusable, `noteKey` says a choice was made without
 * a measurement on this hardware, and `declinedKey` says a chosen provider was
 * not used. `neededBytes` and `roomBytes` travel *beside* the key rather than
 * inside the sentence, because a byte count is not translatable.
 *
 * @typedef {Object} Accelerators
 * @property {string} preference - `auto`, `cpu`, or a provider id
 * @property {Array<{id: string, labelKey: string, available: boolean, reasonKey: string|null, measured: boolean, active: boolean, selected: boolean}>} providers
 * @property {Array<{id: string, modelName: string, modelKey: string, preference: string, supportedIds: string[], backendStatus: Array<{id: string, supported: boolean, installed: boolean, available: boolean, verified: boolean, reasonKey: string|null}>, acceleratorId: string, labelKey: string, noteKey: string|null, declinedKey: string|null, declinedId: string|null, neededBytes: number|null, roomBytes: number|null}>} models
 */

/**
 * The model catalogue and what of it this machine has.
 *
 * `requiredBy` names **engines and features** in the interface's own
 * vocabulary - `autoClean`, `lama` - so that `state/capabilities` can
 * decide which controls to draw from the rows themselves rather than from a
 * second table beside them. An engine named by no row needs no weights and is
 * therefore always available, which is `fill`.
 *
 * `installed` is presence **and size**, never a digest: hashing 200 MB every
 * time Settings opens is not affordable. `sha256Ok` is null until something
 * calls `verifyModel`, which is the explicit re-check.
 *
 * `readOnly` is a copy found outside the directory the app writes to - a
 * developer's checkout, `$MANGA_CLEANER_MODELS`, or the installer's own - and
 * the Delete button is not offered for one.
 *
 * `partialBytes` is the unfinished download beside a row - the `.part` a
 * cancelled or failed transfer left, which the next Download resumes from and
 * which `discardPartial` throws away. `null` when there is none, which is the
 * ordinary state; on the runtime row it is everything its download directory is
 * holding rather than one file - both artefacts' partial transfers and any
 * complete archive a failed unpack left, which is kept for a re-unpack and is
 * the same kind of remainder.
 *
 * `hasToken` says whether a Hugging Face token is stored. **The token itself
 * never crosses the seam in this direction.** `tokenStore` says *where* one is
 * kept - `'keychain'` for the operating system's credential store,
 * `'fileNoStore'` for a build that has none, `'fileStoreUnavailable'` for a
 * store that is there and would not answer. Both file cases fall back to
 * `settings.json` and they are separate because they are opposite news: one is
 * permanent, the other is usually a keychain the user can unlock. Ids, not
 * sentences, and the difference between three notes in Settings.
 *
 * `tokenStoreReason` says *why* an unreachable store was unreachable -
 * `'locked'`, `'unreachable'`, `'ambiguous'`, `'unknown'` - and is `null` for
 * the other two locations, which have nothing to explain. An id again: the
 * platform's own words for a locked keychain are prose, and a note that could
 * only say "something went wrong" sent the user looking.
 *
 * The runtime row is the ONNX Runtime rather than a weight, and it carries what
 * a *choice* needs: `flavour` is the build that would be downloaded - the
 * stored `runtimeFlavour` where this platform publishes it, the platform's
 * default otherwise, and **not** necessarily the flavour of whatever is already
 * on disk, which nothing short of loading the library can tell. `bytes` is that
 * build's whole download, both artefacts where there are two. `flavours` is
 * every build published for this platform, default first; one entry means there
 * is nothing to choose and the picker is not drawn, which is macOS and Linux
 * aarch64.
 *
 * `installedFlavour` and `installedVersion` are the other question, and they
 * are the build **that is actually here**, read from the record an install
 * writes beside the libraries. `null` is *unknown* - a runtime unpacked before
 * that record existed, or one found outside the folder the app owns - rather
 * than *none*, so the interface says nothing rather than claiming a build.
 *
 * @typedef {Object} ModelsView
 * @property {Array<{id: string, fileName: string, bytes: number, sha256: string, kindKey: string, requiredBy: string[], installed: boolean, path: string|null, readOnly: boolean, sha256Ok: boolean|null, downloading: boolean, partialBytes: number|null}>} models
 * @property {{installed: boolean, path: string|null, readOnly: boolean, downloading: boolean, version: string|null, flavour: string|null, bytes: number|null, flavours: Array<{id: string, ortVersion: string, bytes: number, isDefault: boolean, userInstalled: string[]}>, available: boolean, platform: string|null, installedFlavour: string|null, installedVersion: string|null, partialBytes: number|null}} runtime
 * @property {string|null} modelsDir
 * @property {string|null} runtimeDir
 * @property {boolean} hasToken
 * @property {'keychain'|'fileNoStore'|'fileStoreUnavailable'} tokenStore
 * @property {'locked'|'unreachable'|'ambiguous'|'unknown'|null} tokenStoreReason
 */

/**
 * One thing the backend has loaded, for the tab in the corner of the editor.
 *
 * `bytes` is an estimate and `basis` says which kind - `measured` from a
 * running process, `weights` from the size of the model file, `reported` by the
 * loaded thing itself. The interface uses `basis` to format the size: `measured`
 * renders as an exact size (`models.value.size`), while `weights` and
 * `reported` render as approximate (`models.value.sizeApprox`).
 *
 * `id` is **not stable across loads**. A model unloaded and opened again is a
 * new row, because it is a new session - which is exactly what lets the
 * interface tell "still loaded" from "gone and back".
 *
 * @typedef {Object} LoadedModel
 * @property {number} id
 * @property {string} kindKey - e.g. `models.kind.inpainter`
 * @property {number} bytes
 * @property {'measured'|'weights'|'reported'} basis
 * @property {string} deviceKey - e.g. `accel.coreml`
 * @property {boolean} gpu
 * @property {number} idleMs - how long since anything used it
 * @property {boolean} unloading - an unload was asked for and has not reached a safe point yet
 */

/* ------------------------------------------------------------------ */
/* The selector                                                        */
/* ------------------------------------------------------------------ */

/** @type {Backend|null} */
let instance = null

/**
 * Force the mock even inside a Tauri window. Set on `globalThis` before the
 * first `getBackend()`; the flag exists so the built application can be run
 * against fixture data - for a demo, for a screenshot, or to tell an interface
 * bug apart from a backend one.
 */
const FORCE_MOCK = '__MANGA_CLEANER_FORCE_MOCK__'

/**
 * The single backend instance.
 *
 * Inside a Tauri window this is the adapter, **with the mock behind it**: the
 * adapter answers what the core can answer and forwards the rest, so the
 * interface is whole at every stage of the backend being built rather than only
 * at the end. Outside one - a browser, a test - it is the mock alone, because
 * there is nothing to invoke.
 *
 * @returns {Backend}
 */
export function getBackend() {
  if (!instance) {
    const mock = createMockBackend()
    instance = isTauri() && !globalThis[FORCE_MOCK] ? createTauriBackend({ fallback: mock }) : mock
    // A browser tab never holds its own close, so the quit question is raised
    // by hand there: `__MANGA_CLEANER_MOCK_QUIT__({canHide: true})` in the
    // console of the Vite mock.
    if (instance === mock) globalThis.__MANGA_CLEANER_MOCK_QUIT__ = (options) => mock.simulateQuitRequest(options)
  }
  return instance
}

/**
 * Replaces the selected implementation. For tests and for a later Tauri
 * adapter; the interface never calls this.
 *
 * @param {Backend|null} backend - pass null to fall back to the default
 * @returns {Backend|null}
 */
export function setBackend(backend) {
  instance = backend
  return instance
}

/**
 * The cloud commands currently registered in `src-tauri/src/lib.rs` (P3 configuration & secrets).
 */
export const TAURI_REGISTERED_CLOUD_COMMANDS = Object.freeze([
  'read_inference_config',
  'write_inference_config',
  'select_cloud_profile',
  'store_cloud_secret',
  'delete_cloud_secret',
  'get_cloud_secret_summary',
  'forget_cloud_secret_denials',
  'check_cloud_connection',
  'get_cloud_model_info',
  'get_cloud_gpu_status',
  'stop_cloud_gpu',
  'prepare_cloud_consent',
  'confirm_cloud_consent',
  'submit_cloud_attempt',
  'get_cloud_attempt_status',
  'get_cloud_attempt_result',
  'resolve_qwen_review',
  'cancel_cloud_attempt',
  'reconcile_cloud_recovery',
])

/**
 * The cloud lifecycle commands defined by backend wire/consent/journal contracts
 * awaiting registration in `src-tauri/src/lib.rs` (P3b consent IPC, P4 durable lifecycle & recovery).
 */
export const TAURI_PENDING_CLOUD_COMMANDS = Object.freeze([])

/**
 * The cloud provisioner helper IPC commands registered in `src-tauri/src/lib.rs`.
 */
export const TAURI_PROVISIONER_COMMANDS = Object.freeze([
  'run_cloud_provisioner',
  'cancel_cloud_provisioner',
  'provision_inspect',
  'provision_plan',
  'provision_apply',
  'provision_resume',
  'provision_cleanup',
  'provision_probe',
])

/**
 * Truthfully reports whether all required remote execution lifecycle commands are registered in Tauri IPC.
 * Registration indicates that genuine backend handlers exist in `src-tauri/src/lib.rs` and are mapped in the adapter.
 *
 * @returns {boolean}
 */
export function isCloudExecutionRegistered() {
  return TAURI_PENDING_CLOUD_COMMANDS.length === 0 && TAURI_REGISTERED_CLOUD_COMMANDS.length > 0
}

/**
 * Whether a cloud render can be offered right now, and when it cannot, why.
 *
 * Composed from three commands that already exist rather than asked of a
 * fourth, because each answers a different question and each is the
 * authority for its own: the permission is a setting, the default target is
 * the inference configuration, and whether a runtime secret is stored for it
 * is the credential store's summary - which says *present* and never says
 * the secret.
 *
 * `reason` is the first thing missing, in the order a person fixes them:
 * `off` (the permission switch), `noTarget` (no cloud endpoint is the
 * default, or the default names a profile that is gone), `noSecret` (the
 * endpoint has no runtime token), `secretLocked` (the system password store
 * would not answer, so whether a token is stored is unknown: the setup is not
 * lost and must not be redone), `unknown` (a read failed; not ready, and
 * nothing more can honestly be said).
 *
 * The target, the profile and `configured` (a default cloud endpoint whose
 * profile exists and has a runtime token stored) are read whether or not the
 * permission is on, so a status line can say which endpoint would be used and
 * whether switching cloud on is all that is left. `ready` is `allowed` and
 * `configured` together, and is the only field that may enable a cloud action.
 *
 * @typedef {Object} CloudReadiness
 * @property {boolean} allowed
 * @property {boolean} configured
 * @property {boolean} ready
 * @property {'off'|'noTarget'|'noSecret'|'secretLocked'|'unknown'|null} reason - null when ready
 * @property {ExecutionTarget|null} target
 * @property {CloudProfile|null} profile
 * @property {CloudEndpoint[]} endpoints - every saved endpoint, read in the same pass
 */

/**
 * A saved cloud endpoint as the interface names it: never a secret.
 *
 * @typedef {Object} CloudEndpoint
 * @property {'modal'|'beam'} provider
 * @property {string} id
 * @property {string} name
 * @property {string} endpointUrl
 */

/**
 * @param {any} config - an inference config
 * @returns {CloudEndpoint[]}
 */
function cloudEndpointsOf(config) {
  /** @type {CloudEndpoint[]} */
  const list = []
  for (const provider of /** @type {const} */ (['modal', 'beam'])) {
    const profiles = config?.[provider === 'modal' ? 'modalProfiles' : 'beamProfiles']
    if (!profiles || typeof profiles !== 'object') continue
    for (const [id, profile] of Object.entries(profiles)) {
      if (!profile || typeof profile !== 'object') continue
      list.push({
        provider,
        id,
        name: typeof profile.name === 'string' ? profile.name.trim() : '',
        endpointUrl: typeof profile.endpointUrl === 'string' ? profile.endpointUrl : '',
      })
    }
  }
  return list
}

/**
 * @param {Backend} [backend]
 * @returns {Promise<CloudReadiness>}
 */
export async function readCloudReadiness(backend = getBackend()) {
  /** @type {CloudReadiness} */
  const verdict = {
    allowed: false,
    configured: false,
    ready: false,
    reason: 'unknown',
    target: null,
    profile: null,
    endpoints: [],
  }
  try {
    const settings = await backend.readSettings()
    verdict.allowed = settings?.cloudEngines === 'allowed'
    const config = await backend.readInferenceConfig()
    verdict.endpoints = cloudEndpointsOf(config)
    const selected = config?.selectedTarget
    const profiles =
      selected?.type === 'modal'
        ? config.modalProfiles
        : selected?.type === 'beam'
          ? config.beamProfiles
          : null
    const profile = profiles && selected.profile_id ? (profiles[selected.profile_id] ?? null) : null
    let secret = false
    let locked = false
    if (profile) {
      verdict.target = { type: selected.type, profile_id: selected.profile_id }
      verdict.profile = profile
      const summary = await backend.getCloudSecretSummary({
        provider: verdict.target.type,
        profileId: verdict.target.profile_id,
        role: 'runtime',
      })
      secret = summary?.present === true
      locked = !secret && summary?.backend === 'unavailable'
    }
    verdict.configured = verdict.target !== null && secret
    verdict.reason = !verdict.allowed
      ? 'off'
      : !verdict.target
        ? 'noTarget'
        : !verdict.configured
          ? locked ? 'secretLocked' : 'noSecret'
          : null
    verdict.ready = verdict.allowed && verdict.configured
    return verdict
  } catch {
    verdict.configured = false
    verdict.ready = false
    verdict.reason = 'unknown'
    return verdict
  }
}

/**
 * Whether cloud execution is ready: the permission is on, the default target
 * is a cloud endpoint whose profile exists, and a runtime secret is stored for
 * it. Anything it cannot read counts as not ready.
 *
 * @param {Backend} [backend]
 * @returns {Promise<boolean>}
 */
export async function isCloudExecutionReady(backend = getBackend()) {
  return (await readCloudReadiness(backend)).ready
}
