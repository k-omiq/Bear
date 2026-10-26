<script>
  /**
   * The Runtime screen's "This computer": what the graphics paths need, what
   * this machine has, and which build it is ready for.
   *
   * Two halves, because they are known at different times. The **NVIDIA
   * half** (`runtimeReport`) reads the disk and the driver, never the runtime,
   * so it is there before a runtime is downloaded and is what lets a build be
   * picked before 455 MB of it arrive. It is judged for `flavour`, the build
   * being chosen, which is the question a user with two CUDA toolkits needs
   * answered: which one would this build use, and is the cuDNN beside it the
   * right one. The **paths half** is `listAccelerators`, the loaded runtime's
   * own answer, and waits for the runtime (`accelerators` null until then).
   *
   * Shown only where it says something: the NVIDIA half on Windows and Linux,
   * where the CUDA builds are offered; the paths half wherever a runtime is
   * loaded. Paths a build does not carry at all are left out - nine rows of
   * "not in this build" teach nothing - and every path that is in the build
   * and cannot run says why, which is where an install or a driver update is
   * named.
   *
   * @type {{
   *   flavour: string|null,
   *   flavours?: Array<{id: string}>,
   *   platform: string|null,
   *   accelerators: import('../../api/backend.js').Accelerators|null,
   *   pathsPending?: boolean,
   *   onchooseflavour?: (id: string) => void,
   *   chooseDisabled?: boolean,
   *   onrecheck?: () => (void|Promise<void>),
   *   found?: string|null,
   * }}
   *
   * `found` is bound out: the CUDA build this machine is ready for, so a
   * build choice beside this can mark it without asking the driver twice.
   * `chooseDisabled` holds Use build still while the build cannot change
   * (its download is running), as the build choice beside it is held.
   */
  import { t } from '../../i18n/index.js'
  import { Button } from '../../ui/index.js'
  import { getBackend } from '../../api/backend.js'
  import { backendName, cudaMajorOf, flavourName } from './runtime.js'

  let {
    flavour,
    flavours = [],
    platform,
    accelerators,
    pathsPending = false,
    onchooseflavour,
    chooseDisabled = false,
    onrecheck,
    found = $bindable(null),
  } = $props()

  const cudaPlatform = $derived(typeof platform === 'string' && /^(windows|linux)-x64$/.test(platform))
  const wanted = $derived(cudaMajorOf(flavour))

  /** @type {import('../../api/backend.js').RuntimeReport|null} */
  let report = $state(null)
  let failed = $state(false)
  let checking = $state(false)
  let asked = 0

  async function read(rescan = false) {
    const ask = ++asked
    checking = true
    try {
      const answer = await getBackend().runtimeReport({ flavour, rescan })
      if (ask !== asked) return
      report = answer
      failed = false
    } catch {
      if (ask !== asked) return
      report = null
      failed = true
    } finally {
      if (ask === asked) checking = false
    }
  }

  $effect(() => {
    void flavour
    if (cudaPlatform) read()
  })

  $effect(() => {
    found = report?.recommended ?? null
  })

  async function recheck() {
    if (checking) return
    await Promise.all([cudaPlatform ? read(true) : null, onrecheck?.()])
  }

  const paths = $derived(
    (accelerators?.providers ?? []).filter(
      (provider) => provider.available || (provider.reasonKey && provider.reasonKey !== 'accel.declined.unavailable'),
    ),
  )

  const recommended = $derived(
    report?.recommended && report.recommended !== flavour && flavours.some((row) => row.id === report?.recommended)
      ? report.recommended
      : null,
  )

  const verdictParams = $derived({ major: wanted ?? '', have: report?.verdictHave ?? '', need: report?.verdictNeed ?? '' })
</script>

<div class="machine">
  {#if cudaPlatform}
    <dl class="facts">
      <div class="fact">
        <dt>{t('settings.runtime.machine.gpu')}</dt>
        <dd>
          {#if report?.gpus.length}
            {#each report.gpus as gpu, index (index)}
              <span class="line">{t('settings.runtime.machine.gpuLine', { name: gpu.name, driver: gpu.driver })}</span>
            {/each}
          {:else if report}
            <span class="line quiet">{t('settings.runtime.machine.noNvidia')}</span>
          {:else if failed}
            <span class="line quiet">{t('settings.runtime.machine.unreadable')}</span>
          {:else}
            <span class="line quiet">{t('settings.runtime.machine.checking')}</span>
          {/if}
        </dd>
      </div>
      {#if report && (report.gpus.length || report.toolkits.length || report.cudnn.length)}
        <div class="fact">
          <dt>{t('settings.runtime.machine.toolkits')}</dt>
          <dd>
            {#each report.toolkits as toolkit (`${toolkit.major}:${toolkit.dir}`)}
              <span class="line" class:off={wanted !== null && toolkit.major !== wanted}>
                <span class="tag">CUDA {toolkit.minor === null ? toolkit.major : `${toolkit.major}.${toolkit.minor}`}</span>
                <span class="path" title={toolkit.dir}><bdi dir="ltr">{toolkit.dir}</bdi></span>
                {#if toolkit.missing.length}<span class="warn">{t('settings.runtime.machine.missing', { files: toolkit.missing.join(', ') })}</span>{/if}
              </span>
            {:else}
              <span class="line quiet">{t('settings.runtime.machine.none')}</span>
            {/each}
          </dd>
        </div>
        <div class="fact">
          <dt>{t('settings.runtime.machine.cudnn')}</dt>
          <dd>
            {#each report.cudnn as install (install.dir)}
              <span class="line" class:off={wanted !== null && install.cudaMajor !== null && install.cudaMajor !== wanted}>
                <span class="tag">
                  {install.cudaMajor === null ? t('settings.runtime.machine.cudnnAny') : t('settings.runtime.machine.cudnnFor', { major: install.cudaMajor })}
                </span>
                <span class="path" title={install.dir}><bdi dir="ltr">{install.dir}</bdi></span>
                {#if install.missing.length}<span class="warn">{t('settings.runtime.machine.missing', { files: install.missing.join(', ') })}</span>{/if}
              </span>
            {:else}
              <span class="line quiet">{t('settings.runtime.machine.none')}</span>
            {/each}
          </dd>
        </div>
      {/if}
    </dl>

    <!-- Always in the page, so a screen reader announces a verdict that
         arrives; drawn only for the build it was judged for, never the last
         build's answer under the new build's name. -->
    <div role="status">
      {#if report?.verdictKey && wanted !== null && report.wanted === wanted}
        <p class="verdict" class:ok={report.verdictKey === 'accel.cudaCheck.ready'}>
          <span class="verdict-build">{flavourName(flavour)}</span>
          {t(report.verdictKey, verdictParams)}
        </p>
      {/if}
    </div>

    {#if recommended}
      <div class="recommend">
        <p>{t('settings.runtime.machine.recommend', { build: flavourName(recommended) })}</p>
        {#if onchooseflavour}
          <Button size="sm" variant="soft" disabled={chooseDisabled} onclick={() => onchooseflavour(recommended)}>
            {t('settings.runtime.machine.useBuild', { build: flavourName(recommended) })}
          </Button>
        {/if}
      </div>
    {/if}
  {/if}

  <div class="paths">
    <span class="paths-label">{t('settings.runtime.machine.paths')}</span>
    {#if paths.length}
      <ul class="path-list">
        {#each paths as provider (provider.id)}
          <li class="path-row" class:down={!provider.available}>
            <span class="dot" aria-hidden="true"></span>
            <span class="path-name">{backendName(provider.id)}</span>
            <span class="path-state">{provider.available ? t('settings.runtime.machine.pathReady') : t(provider.reasonKey ?? '')}</span>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="quiet">{pathsPending ? t('settings.runtime.machine.pathsLater') : t('settings.accel.unreadable')}</p>
    {/if}
  </div>

  <div class="recheck">
    <Button size="sm" aria-disabled={checking} onclick={recheck}>
      {checking ? t('settings.runtime.machine.checking') : t('settings.runtime.machine.recheck')}
    </Button>
  </div>
</div>

<style>
  .machine { display: grid; gap: var(--s-5) }
  .facts { display: grid; margin: 0 }
  .fact {
    display: grid;
    grid-template-columns: minmax(110px, 160px) minmax(0, 1fr);
    gap: var(--s-5);
    padding: var(--s-4) 0;
    border-top: 1px solid var(--line);
  }
  .fact:first-child { border-top: none; padding-top: 0 }
  dt { color: var(--t2); font-size: 12px }
  dd { margin: 0; display: grid; gap: var(--s-2); min-width: 0 }
  .line { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px var(--s-3); min-width: 0; font-size: 12.5px }
  .line.off { color: var(--t3) }
  .line.off .tag { opacity: .7 }
  .tag { flex: none; font-weight: 600; font-variant-numeric: tabular-nums }
  /* Cut from the left: the end of a path is the version folder that matters. */
  .path {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
    color: var(--t3);
    font-size: 11.5px;
  }
  .warn { color: var(--warn); font-size: 11.5px }
  .quiet { margin: 0; color: var(--t3); font-size: 12px }

  .verdict {
    margin: 0;
    padding: var(--s-4) var(--s-5);
    border-radius: var(--r-md);
    background: var(--panel2);
    color: var(--warn);
    font-size: 12.5px;
    line-height: 1.5;
    max-width: 72ch;
  }
  .verdict.ok { color: var(--text) }
  .verdict-build { font-weight: 600; margin-right: var(--s-2); color: var(--text) }

  .recommend {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-5);
    flex-wrap: wrap;
  }
  .recommend p { margin: 0; font-size: 12.5px; max-width: 60ch }

  .paths { display: grid; gap: var(--s-3) }
  .paths-label { color: var(--t2); font-size: 12px }
  .path-list { list-style: none; margin: 0; padding: 0; display: grid; gap: var(--s-2) }
  .path-row {
    display: grid;
    grid-template-columns: 8px minmax(84px, max-content) minmax(0, 1fr);
    align-items: baseline;
    gap: var(--s-4);
    font-size: 12.5px;
  }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--accent); transform: translateY(-1px) }
  .path-row.down .dot { background: none; box-shadow: inset 0 0 0 1.5px var(--warn) }
  .path-name { font-weight: 500 }
  .path-state { color: var(--t2); min-width: 0 }
  .path-row.down .path-state { color: var(--warn) }
  .recheck { display: flex }

  @media (max-width: 620px) {
    .fact { grid-template-columns: 1fr; gap: var(--s-2) }
  }
</style>
