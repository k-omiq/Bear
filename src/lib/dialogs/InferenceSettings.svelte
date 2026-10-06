<script module>
  /**
   * Validate human-readable display name (1..=128 chars, non-empty, trimmed, no control chars).
   *
   * @param {string} name
   * @returns {boolean}
   */
  export function isValidProfileName(name) {
    if (!name || typeof name !== 'string') return false
    if (name !== name.trim()) return false
    // Counted in UTF-8 bytes, as the native check (`config::validate_profile_name`) counts.
    if (name.length === 0 || new TextEncoder().encode(name).length > 128) return false
    if (/[\x00-\x1F\x7F]/.test(name)) return false
    return true
  }

  /**
   * Validate that an endpoint URL uses strict HTTPS, contains no credentials,
   * query strings, fragments, control characters, or prohibited hostnames (IPs, localhost).
   *
   * @param {string} rawUrl
   * @returns {boolean}
   */
  export function isValidEndpointUrl(rawUrl) {
    if (!rawUrl || typeof rawUrl !== 'string') return false
    if (rawUrl !== rawUrl.trim()) return false
    if (rawUrl.length === 0 || rawUrl.length > 2048) return false
    if (/[\x00-\x1F\x7F]/.test(rawUrl)) return false

    try {
      const parsed = new URL(rawUrl)
      if (parsed.protocol !== 'https:') return false
      if (parsed.username || parsed.password) return false
      if (parsed.search || parsed.hash) return false

      const host = parsed.hostname.toLowerCase().replace(/\.+$/, '')
      if (!host) return false
      if (
        host === 'localhost' ||
        host.endsWith('.localhost') ||
        host.endsWith('.local') ||
        host.endsWith('.internal') ||
        host.endsWith('.arpa')
      ) {
        return false
      }
      // Conservative rejection of IPv4 literals
      if (/^(\d{1,3}\.){3}\d{1,3}$/.test(host)) return false
      // Conservative rejection of IPv6 literals
      if (host.startsWith('[') || host.includes(':')) return false
      // WHATWG decimal notation
      if (/^\d+$/.test(host)) return false
      // Valid hostname characters
      if (!/^[a-z0-9_.-]+$/.test(host)) return false

      return true
    } catch {
      return false
    }
  }
</script>

<script>
  /**
   * Settings > Cloud: whether the cloud may be used, where it runs, and what
   * to do when something about it needs attention.
   *
   * From the top:
   *
   * 1. **Status**, one line: off, ready on the default endpoint, or the first
   *    thing that needs attention. It is `state/cloud.svelte.js`'s readiness,
   *    the same verdict the tool bar gates the Cloud engine on, so the two
   *    never disagree.
   * 2. **The permission switch.** This is its only place; General points here.
   * 3. **Needs attention**, only when something does: a setup that did not
   *    finish, a default endpoint with no access token, a render the last
   *    session could not settle.
   * 4. **Endpoints**: name, provider and Modal account, token, which one is
   *    the default, Rename, a connection test, and Remove. Each endpoint keeps
   *    its own access token, so choosing another default is the whole switch
   *    between accounts: nothing is asked for again. Remove can also delete
   *    what setup created in the account; that runs the provisioner's
   *    cleanup, which lists what it will delete and asks for the key again.
   * 5. **Set up with Modal or Beam**: the provisioner, inline.
   * 6. **Connect an existing endpoint**, collapsed: the manual form, for an
   *    endpoint deployed some other way.
   *
   * **Tokens.** A token typed here goes to the system keychain through
   * `storeCloudSecret`, and the field is wiped as soon as that call returns,
   * whatever it answered. None is ever read back: the list only asks whether
   * one is there.
   */
  import { onMount, tick, untrack } from 'svelte'
  import Icon from '../icons/Icon.svelte'
  import { Button, Disclosure, Field, Segmented, TextInput } from '../ui/index.js'
  import CloudProvisioner from './CloudProvisioner.svelte'
  import { getBackend } from '../api/backend.js'
  import { notify } from '../state/app.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { cloud, dismissRecovered, abandonRecovered, retryCloudRecovery, refreshCloudReadiness, recheckCloudConfiguration, setCloudPermission } from '../state/cloud.svelte.js'
  import { cloudGpu } from '../state/cloudgpu.svelte.js'
  import { endpointAccount, forgetUnfinished, healthKey, providerPaused, removeEndpoint, setup } from './provisioning.svelte.js'
  import { t } from '../i18n/index.js'
  import { recoveryActions } from '../model/cloudnotices.js'

  /** @type {{ backend?: import('../api/backend.js').Backend }} */
  let { backend: givenBackend } = $props()

  const backend = untrack(() => givenBackend ?? getBackend())

  /** The ids setup gives what it creates. Only those can be found again in the account and cleaned up. */
  const INSTALLATION_ID = /^[a-z0-9][a-z0-9-]{2,40}$/

  const uid = $props.id()
  const ids = {
    endpoints: `${uid}-endpoints`,
    attention: `${uid}-attention`,
    removeText: `${uid}-remove-text`,
    rename: `${uid}-rename`,
    tokenId: `${uid}-token-id`,
    tokenSecret: `${uid}-token-secret`,
    name: `${uid}-name`,
    url: `${uid}-url`,
    manualTokenId: `${uid}-manual-token-id`,
    manualSecret: `${uid}-manual-secret`,
  }

  const permissionOptions = [
    { value: 'off', label: t('settings.inference.permission.off') },
    { value: 'on', label: t('settings.inference.permission.on') },
  ]
  const providerOptions = [
    { value: 'modal', label: t('settings.inference.provider.modal') },
    { value: 'beam', label: t('settings.inference.provider.beamPaused'), disabled: providerPaused('beam') },
  ]

  /* ---------- what is saved ---------- */

  /** @type {import('../api/backend.js').InferenceConfig|null} */
  let config = $state(null)
  let loaded = $state(false)
  let loadFailed = $state(false)
  /**
   * Whether each endpoint has an access token, keyed `provider:id`: null when
   * it could not be read, absent while it is being read.
   *
   * @type {Record<string, boolean|null>}
   */
  let tokens = $state({})
  /**
   * Whether each setup-made Modal endpoint runs this app's cloud code, by
   * endpoint key; absent when it was not asked or could not answer.
   *
   * @type {Record<string, boolean>}
   */
  let current = $state({})
  /**
   * The last connection test of each endpoint.
   *
   * @type {Record<string, {state: 'testing'}|{state: 'done', ok: boolean, status: string, latency: number|null}>}
   */
  let tests = $state({})
  /** @type {string|null} an i18n key */
  let listError = $state(null)
  let saving = $state(false)
  let permissionSaving = false

  /* ---------- one endpoint's token or name form, one Remove confirmation ---------- */

  /** @type {string|null} */
  let renameFor = $state(null)
  let renameValue = $state('')
  let renameSaving = $state(false)
  /** @type {string|null} */
  let renameError = $state(null)

  /** @type {string|null} */
  let tokenFor = $state(null)
  let tokenId = $state('')
  let tokenSecret = $state('')
  let tokenSaving = $state(false)
  /** @type {string|null} */
  let tokenError = $state(null)

  /** @type {string|null} */
  let removingKey = $state(null)
  let alsoDelete = $state(false)
  let removeBusy = $state(false)
  /** @type {string|null} */
  let removeError = $state(null)

  /* ---------- the provisioner ---------- */

  /**
   * What the inline provisioner is open for: a new setup, or Resume or Clean
   * up of one installation. Null while it is closed.
   *
   * @type {{action: 'setup'}|{action: 'resume'|'cleanup', provider: 'modal'|'beam', installationId: string}|null}
   */
  let task = $state(null)

  /**
   * The control the task was opened from, as the data attribute that finds it
   * again: it is drawn anew rather than kept while the task is open (the Set
   * up button, Resume and Get a new token all make way for the provisioner),
   * so the element itself would be gone by the time the task closes.
   *
   * @type {[string, string]|null}
   */
  let taskOpener = null

  /* ---------- the manual form ---------- */

  let manualOpen = $state(false)
  /** @type {'modal'|'beam'} */
  let manualProvider = $state('modal')
  let manualName = $state('')
  let manualUrl = $state('')
  let manualTokenId = $state('')
  let manualSecret = $state('')
  let manualSaving = $state(false)
  /** @type {string|null} */
  let manualError = $state(null)
  /** @type {{key: string, params: Record<string, unknown>, ok: boolean}|null} */
  let manualNote = $state(null)

  /** @type {HTMLElement|undefined} */
  let root = $state()
  /** @type {HTMLElement|undefined} */
  let slot = $state()
  let alive = true

  /* ---------- derived ---------- */

  /**
   * @param {unknown} url
   * @returns {string}
   */
  function hostOf(url) {
    try {
      return new URL(String(url)).host
    } catch {
      return String(url ?? '')
    }
  }

  const endpoints = $derived.by(() => {
    if (!config) return []
    /** @type {Array<{provider: 'modal'|'beam', id: string, key: string, name: string, host: string, account: string|null, createdAtMs: number}>} */
    const list = []
    for (const provider of /** @type {const} */ (['modal', 'beam'])) {
      const profiles = (provider === 'modal' ? config.modalProfiles : config.beamProfiles) ?? {}
      for (const [id, profile] of Object.entries(profiles)) {
        if (!profile || typeof profile !== 'object') continue
        list.push({
          provider,
          id,
          key: `${provider}:${id}`,
          name: typeof profile.name === 'string' && profile.name.trim() ? profile.name : id,
          host: hostOf(profile.endpointUrl),
          account: endpointAccount(provider, profile.endpointUrl),
          createdAtMs: typeof profile.createdAtMs === 'number' ? profile.createdAtMs : 0,
        })
      }
    }
    return list.sort((a, b) => a.createdAtMs - b.createdAtMs || a.name.localeCompare(b.name))
  })

  const selectedKey = $derived.by(() => {
    const target = config?.selectedTarget
    return target && target.type !== 'local' ? `${target.type}:${target.profile_id}` : null
  })
  const selectedEndpoint = $derived(endpoints.find((ep) => ep.key === selectedKey) ?? null)

  const status = $derived.by(() => {
    if (!session.cloudAllowed) return { tone: 'off', text: t('settings.inference.status.off') }
    const readiness = cloud.readiness
    // Still the answer from before the switch was turned on.
    if (!cloud.checked || readiness.reason === 'off') {
      return { tone: 'checking', text: t('settings.inference.status.checking') }
    }
    if (readiness.ready && readiness.profile) {
      return { tone: 'ready', text: t('settings.inference.status.ready', { name: readiness.profile.name }) }
    }
    const reasonKey =
      readiness.reason === 'noTarget'
        ? loaded && endpoints.length === 0
          ? 'settings.inference.reason.none'
          : 'settings.inference.reason.noTarget'
        : readiness.reason === 'noSecret'
          ? 'settings.inference.reason.noSecret'
          : readiness.reason === 'secretLocked'
            ? 'settings.inference.reason.secretLocked'
            : 'settings.inference.reason.unknown'
    return { tone: 'attention', text: t('settings.inference.status.attention', { reasonKey }) }
  })

  const setupRunning = $derived(setup.run?.status === 'running')
  const unfinished = $derived(!task && !setupRunning ? setup.unfinished : null)
  const missingToken = $derived(selectedEndpoint && tokens[selectedEndpoint.key] === false ? selectedEndpoint : null)
  // A Modal installation deployed by an older release answers without the GPU
  // routes. Resume redeploys it with this release's code under the same plan.
  // A setup that answers with other cloud code than this app's helper would
  // deploy (`check_cloud_release`) runs older code: Resume redeploys it too.
  const outdated = $derived(
    !missingToken && selectedEndpoint?.provider === 'modal' && madeBySetup(selectedEndpoint) &&
      (cloudGpu.unsupported === 'outdated' || current[selectedEndpoint.key] === false)
      ? selectedEndpoint
      : null,
  )
  const attempts = $derived([...(cloud.recovery?.needsAttention ?? []), ...(cloud.recovery?.stillRunning ?? [])])
  /** @type {string|null} */
  let confirmingAbandon = $state(null)
  let recoveryBusy = $state(false)
  /** @param {() => Promise<void>} action */
  async function recoveryAction(action) {
    recoveryBusy = true
    try { await action(); confirmingAbandon = null }
    catch { notify({ key: 'cloud.recovery.actionFailed', tone: 'warn' }) }
    finally { recoveryBusy = false }
  }
  // Interrupted renders are a log, not a problem to fix: most are regions
  // edited after they were sent. They sit behind "Cloud logs", closed.
  let logsOpen = $state(false)
  async function dismissAllAttempts() {
    const attemptIds = (cloud.recovery?.needsAttention ?? []).map((entry) => entry.attemptId)
    await recoveryAction(async () => { for (const id of attemptIds) await dismissRecovered(id, backend) })
  }
  const attention = $derived(Boolean(unfinished || missingToken || outdated))

  /* ---------- labels ---------- */

  /** @param {'modal'|'beam'} which */
  function providerKey(which) {
    return which === 'beam' ? 'settings.inference.provider.beam' : 'settings.inference.provider.modal'
  }

  /** @param {string|undefined} reason */
  function attemptReasonKey(reason) {
    if (!reason) return 'cloud.recovery.running'
    if (reason === 'load_error') return 'cloud.recovery.loadError'
    if (reason === 'repair_needed') return 'cloud.recovery.repairNeeded'
    if (reason === 'ambiguous') return 'settings.inference.recovery.reason.ambiguous'
    if (reason === 'stale') return 'settings.inference.recovery.reason.stale'
    return 'settings.inference.recovery.reason.failed'
  }

  /** @param {string} key */
  function tokenText(key) {
    const present = tokens[key]
    if (present === true) return t('settings.inference.token.saved')
    if (present === false) return t('settings.inference.token.missing')
    if (present === null) return t('settings.inference.token.unknown')
    return ''
  }

  /** @param {{id: string}} ep */
  function madeBySetup(ep) {
    return INSTALLATION_ID.test(ep.id)
  }

  /* ---------- loading ---------- */

  let loadSeq = 0

  /** Read the endpoints, then whether each has a token. */
  async function load() {
    loadSeq += 1
    const mine = loadSeq
    /** @type {any} */
    let next
    try {
      next = await backend.readInferenceConfig()
    } catch {
      next = null
    }
    if (!alive || mine !== loadSeq) return
    loaded = true
    loadFailed = !next || typeof next !== 'object'
    if (loadFailed) return
    config = next
    /** @type {Record<string, boolean|null>} */
    const present = {}
    await Promise.all(
      endpoints.map(async (ep) => {
        try {
          const summary = await backend.getCloudSecretSummary({ provider: ep.provider, profileId: ep.id, role: 'runtime' })
          present[ep.key] = summary?.present === true
        } catch {
          present[ep.key] = null
        }
      }),
    )
    if (alive && mine === loadSeq) tokens = present
    // Which cloud code each setup-made Modal endpoint runs. Only an endpoint
    // with a token can be asked; one that cannot answer is left unsaid.
    /** @type {Record<string, boolean>} */
    const runs = {}
    await Promise.all(
      endpoints.filter((ep) => ep.provider === 'modal' && madeBySetup(ep) && present[ep.key] === true).map(async (ep) => {
        try {
          const answer = await backend.checkCloudRelease({ provider: ep.provider, profileId: ep.id })
          if (typeof answer?.current === 'boolean') runs[ep.key] = answer.current
        } catch {
          // Unreachable or refused: nothing to say about its code.
        }
      }),
    )
    if (alive && mine === loadSeq) current = runs
  }

  /** After anything that changes an endpoint or its token. */
  async function reload() {
    await load()
    void refreshCloudReadiness(backend)
  }

  onMount(() => {
    // Opening this panel is the person asking for their endpoints again, so a
    // keychain prompt they cancelled earlier may be shown once more. Polls
    // never do this, so a cancel still holds everywhere else.
    void Promise.resolve(backend.forgetCloudSecretDenials?.()).catch(() => {})
    void load()
    void refreshCloudReadiness(backend)
    // Settings was closed while a setup ran: show its checklist again.
    if (setup.run?.status === 'running') {
      task = { action: 'setup' }
      taskOpener = ['data-setup', 'open']
    }
    return () => {
      alive = false
      wipeTokenForm()
      wipeManualSecrets()
    }
  })

  /* ---------- focus ---------- */

  /**
   * Move focus once the DOM has caught up. Only from inside this panel: a
   * change that finishes after the person has moved on does not pull focus
   * back.
   *
   * @param {() => HTMLElement|null|undefined} find
   */
  async function focusSoon(find) {
    await tick()
    if (!alive || !root) return
    const active = document.activeElement
    if (active && active !== document.body && !root.contains(active)) return
    const element = find()
    element?.focus()
    element?.scrollIntoView?.({ block: 'nearest' })
  }

  /**
   * @param {string} attribute
   * @param {string} value
   */
  function byData(attribute, value) {
    for (const element of root?.querySelectorAll(`[${attribute}]`) ?? []) {
      if (element.getAttribute(attribute) === value) return /** @type {HTMLElement} */ (element)
    }
    return null
  }

  /* ---------- permission ---------- */

  /** @param {string} value */
  async function changePermission(value) {
    const allowed = value === 'on'
    if (permissionSaving || allowed === session.cloudAllowed) return
    permissionSaving = true
    try {
      await setCloudPermission(allowed, backend)
    } finally {
      permissionSaving = false
    }
  }

  /**
   * A setup that ends with a healthy endpoint turns the cloud on, as the
   * first-launch setup does: approving the plan was the choice to use it, and
   * every render still asks first. One whose first check failed leaves the
   * switch alone, and the endpoint is tested from its row.
   *
   * @param {{healthy?: boolean}} [info]
   */
  function configured(info) {
    void reload()
    if (info?.healthy === true) void changePermission('on')
  }

  /* ---------- the default endpoint ---------- */

  /**
   * The radios are the browser's until the write answers. When it fails,
   * put them back where the config says.
   */
  function syncRadios() {
    for (const input of root?.querySelectorAll(`input[name="${uid}-default"]`) ?? []) {
      const radio = /** @type {HTMLInputElement} */ (input)
      radio.checked = radio.value === selectedKey
    }
  }

  /** @param {{provider: 'modal'|'beam', id: string, key: string}} ep */
  async function selectDefault(ep) {
    if (saving || selectedKey === ep.key || providerPaused(ep.provider)) {
      syncRadios()
      return
    }
    saving = true
    listError = null
    try {
      // Read and written natively under one lock, so a rename or a pick in
      // the engine pickers cannot be lost to this one.
      const written = await backend.selectCloudProfile({ provider: ep.provider, profileId: ep.id })
      if (!alive) return
      config = written && typeof written === 'object' ? written : await backend.readInferenceConfig()
      void refreshCloudReadiness(backend)
    } catch {
      if (!alive) return
      listError = 'settings.inference.error.saveFailed'
      syncRadios()
    } finally {
      if (alive) saving = false
    }
  }

  /* ---------- test ---------- */

  /** @param {{provider: 'modal'|'beam', id: string, key: string}} ep */
  async function testEndpoint(ep) {
    if (tests[ep.key]?.state === 'testing') return
    tests = { ...tests, [ep.key]: { state: 'testing' } }
    /** @type {{state: 'done', ok: boolean, status: string, latency: number|null}} */
    let result
    try {
      const answer = await backend.checkCloudConnection({ provider: ep.provider, profileId: ep.id })
      if (answer?.ok === true) await recheckCloudConfiguration({ provider: ep.provider, profileId: ep.id }, backend)
      result = {
        state: 'done',
        ok: answer?.ok === true,
        status: typeof answer?.status === 'string' ? answer.status : 'unknown',
        latency: typeof answer?.latencyMs === 'number' && Number.isFinite(answer.latencyMs) ? Math.round(answer.latencyMs) : null,
      }
    } catch {
      result = { state: 'done', ok: false, status: 'unknown', latency: null }
    }
    if (alive) tests = { ...tests, [ep.key]: result }
  }

  /* ---------- token ---------- */

  function wipeTokenForm() {
    tokenId = ''
    tokenSecret = ''
  }

  /** @param {{provider: 'modal'|'beam', key: string}} ep */
  function openTokenForm(ep) {
    wipeTokenForm()
    tokenError = null
    removingKey = null
    renameFor = null
    tokenFor = ep.key
    void focusSoon(() => document.getElementById(ep.provider === 'modal' ? ids.tokenId : ids.tokenSecret))
  }

  /** @param {{key: string}} ep */
  function closeTokenForm(ep) {
    wipeTokenForm()
    tokenFor = null
    tokenError = null
    void focusSoon(() => byData('data-token', ep.key))
  }

  /** @param {{provider: 'modal'|'beam', id: string, key: string}} ep */
  async function saveToken(ep) {
    if (tokenSaving) return
    const secret = tokenSecret.trim()
    const id = tokenId.trim()
    if (!secret || (ep.provider === 'modal' && !id)) {
      tokenError = 'settings.inference.error.tokenRequired'
      return
    }
    tokenSaving = true
    tokenError = null
    let saved = false
    try {
      await backend.storeCloudSecret({
        provider: ep.provider,
        profileId: ep.id,
        role: 'runtime',
        secret,
        ...(ep.provider === 'modal' ? { tokenId: id } : {}),
      })
      saved = true
    } catch {
      saved = false
    } finally {
      wipeTokenForm()
    }
    if (!alive) return
    tokenSaving = false
    if (!saved) {
      tokenError = 'settings.inference.token.saveFailed'
      return
    }
    tokenFor = null
    const { [ep.key]: _stale, ...rest } = tests
    tests = rest
    await reload()
    void focusSoon(() => byData('data-token', ep.key))
  }

  /* ---------- rename ---------- */

  /** @param {{key: string, name: string}} ep */
  function openRename(ep) {
    wipeTokenForm()
    tokenFor = null
    removingKey = null
    renameValue = ep.name
    renameError = null
    renameFor = ep.key
    void focusSoon(() => document.getElementById(ids.rename))
  }

  /** @param {{key: string}} ep */
  function closeRename(ep) {
    renameFor = null
    renameError = null
    void focusSoon(() => byData('data-rename', ep.key))
  }

  /**
   * Only the name changes: the endpoint, its token and which one is the
   * default stay as they are.
   *
   * @param {{provider: 'modal'|'beam', id: string, key: string, name: string}} ep
   */
  async function saveRename(ep) {
    if (renameSaving) return
    const name = renameValue.trim()
    if (!isValidProfileName(name)) {
      renameError = 'settings.inference.error.invalidName'
      return
    }
    if (name === ep.name) {
      closeRename(ep)
      return
    }
    renameSaving = true
    renameError = null
    let saved = false
    try {
      const latest = await backend.readInferenceConfig()
      const listKey = ep.provider === 'beam' ? 'beamProfiles' : 'modalProfiles'
      const profile = latest?.[listKey]?.[ep.id]
      if (profile) {
        await backend.writeInferenceConfig({
          config: { ...latest, [listKey]: { ...latest[listKey], [ep.id]: { ...profile, name } } },
        })
        saved = true
      }
    } catch {
      saved = false
    }
    if (!alive) return
    renameSaving = false
    if (!saved) {
      renameError = 'settings.inference.rename.failed'
      return
    }
    renameFor = null
    await reload()
    void focusSoon(() => byData('data-rename', ep.key))
  }

  /* ---------- remove ---------- */

  /** @param {{key: string}} ep */
  function askRemove(ep) {
    wipeTokenForm()
    tokenFor = null
    renameFor = null
    removingKey = ep.key
    alsoDelete = false
    removeError = null
    void focusSoon(() => document.getElementById(ids.removeText))
  }

  /** @param {{key: string}} ep */
  function cancelRemove(ep) {
    removingKey = null
    removeError = null
    void focusSoon(() => byData('data-remove', ep.key))
  }

  /** @param {{provider: 'modal'|'beam', id: string, key: string, name: string}} ep */
  async function confirmRemove(ep) {
    if (removeBusy) return
    if (alsoDelete && madeBySetup(ep) && !setupRunning) {
      removingKey = null
      // Opened from the confirmation, which closes; the row's Remove is
      // what is left of that control once the task is done with.
      void openTask({ action: 'cleanup', provider: ep.provider, installationId: ep.id }, ['data-remove', ep.key])
      return
    }
    removeBusy = true
    removeError = null
    try {
      await removeEndpoint({ provider: ep.provider, profileId: ep.id }, backend)
    } catch {
      if (alive) {
        removeError = 'settings.inference.remove.failed'
        removeBusy = false
      }
      return
    }
    if (!alive) return
    removeBusy = false
    removingKey = null
    const { [ep.key]: _gone, ...rest } = tests
    tests = rest
    notify({ key: 'notice.cloud.endpointRemoved', params: { name: ep.name } })
    await load()
    void focusSoon(() => document.getElementById(ids.endpoints))
  }

  /* ---------- the provisioner ---------- */

  /**
   * @param {NonNullable<typeof task>} next
   * @param {[string, string]} opener - the data attribute and value of the control it is opened from
   */
  async function openTask(next, opener) {
    task = next
    taskOpener = opener
    await tick()
    if (!alive || !slot) return
    const heading = /** @type {HTMLElement|null} */ (slot.querySelector('[tabindex="-1"]'))
    heading?.focus()
    slot.scrollIntoView?.({ block: 'nearest' })
  }

  async function closeTask() {
    const opener = taskOpener
    task = null
    taskOpener = null
    // Whatever the provisioner did, the list shows what is saved now. Focus
    // waits for it, because the control the task was opened from may not
    // outlive what the task did: Resume goes once the setup has finished,
    // Get a new token once the token is there, Remove once the endpoint is
    // deleted. Then it goes back to that control if it is still there, and
    // to the heading of the section the task was shown in if it is not.
    await reload()
    void focusSoon(() => (opener && byData(opener[0], opener[1])) || document.getElementById(ids.endpoints))
  }

  /** @param {NonNullable<typeof unfinished>} marker */
  function resumeUnfinished(marker) {
    void openTask({ action: 'resume', provider: marker.provider, installationId: marker.installationId }, [
      'data-resume',
      marker.installationId,
    ])
  }

  /* ---------- manual connect ---------- */

  function wipeManualSecrets() {
    manualTokenId = ''
    manualSecret = ''
  }

  /** A profile id for an endpoint added by hand: never one setup would make. */
  function newProfileId() {
    const alphabet = 'abcdefghijklmnopqrstuvwxyz0123456789'
    const bytes = new Uint8Array(6)
    globalThis.crypto.getRandomValues(bytes)
    return `ep_${Array.from(bytes, (byte) => alphabet[byte % alphabet.length]).join('')}`
  }

  async function connectManual() {
    if (manualSaving) return
    manualError = null
    manualNote = null
    const which = manualProvider
    if (providerPaused(which)) return
    const name = manualName.trim()
    const endpointUrl = manualUrl.trim()
    const secret = manualSecret.trim()
    const id = manualTokenId.trim()
    if (!isValidProfileName(name)) {
      manualError = 'settings.inference.error.invalidName'
      return
    }
    if (!isValidEndpointUrl(endpointUrl)) {
      manualError = 'settings.inference.error.invalidUrl'
      return
    }
    if (!secret || (which === 'modal' && !id)) {
      manualError = 'settings.inference.error.tokenRequired'
      return
    }

    manualSaving = true
    const profileId = newProfileId()
    const listKey = which === 'beam' ? 'beamProfiles' : 'modalProfiles'
    /** @type {any} */
    let previous = null
    let written = false
    let stored = false
    try {
      previous = await backend.readInferenceConfig()
      const now = Date.now()
      const selected = previous?.selectedTarget
      await backend.writeInferenceConfig({
        config: {
          ...previous,
          [listKey]: {
            ...(previous?.[listKey] ?? {}),
            [profileId]: {
              id: profileId,
              name,
              endpointUrl,
              canonicalOrigin: '',
              canonicalOriginFingerprint: '',
              createdAtMs: now,
              updatedAtMs: now,
            },
          },
          selectedTarget: !selected || selected.type === 'local' ? { type: which, profile_id: profileId } : selected,
        },
      })
      written = true
      await backend.storeCloudSecret({
        provider: which,
        profileId,
        role: 'runtime',
        secret,
        ...(which === 'modal' ? { tokenId: id } : {}),
      })
      stored = true
    } catch {
      stored = false
    } finally {
      wipeManualSecrets()
    }

    if (!stored) {
      // An endpoint with no token is one nobody asked for: take it back out.
      if (written && previous) {
        try {
          await backend.writeInferenceConfig({ config: previous })
        } catch {
          // Still listed, with no token; Remove takes it away.
        }
      }
      if (!alive) return
      manualError = written ? 'settings.inference.error.tokenSaveFailed' : 'settings.inference.error.saveFailed'
      manualSaving = false
      void reload()
      return
    }

    await reload()
    /** @type {any} */
    let check = null
    try {
      check = await backend.checkCloudConnection({ provider: which, profileId })
    } catch {
      check = null
    }
    if (!alive) return
    const checkStatus = typeof check?.status === 'string' ? check.status : 'unknown'
    const ok = check?.ok === true
    tests = {
      ...tests,
      [`${which}:${profileId}`]: {
        state: 'done',
        ok,
        status: checkStatus,
        latency: typeof check?.latencyMs === 'number' && Number.isFinite(check.latencyMs) ? Math.round(check.latencyMs) : null,
      },
    }
    manualNote = ok
      ? { key: 'settings.inference.connect.connected', params: { name }, ok }
      : { key: 'settings.inference.connect.savedUnchecked', params: { name, reasonKey: (checkStatus === 'gateway_out_of_date' ? 'cloud.recovery.gatewayOutOfDate' : healthKey(checkStatus)) }, ok }
    manualName = ''
    manualUrl = ''
    manualSaving = false
  }
</script>

{#snippet secretField(id, label, value, onchange)}
  <div class="key">
    <label class="key-label" for={id}>{label}</label>
    <TextInput
      {id}
      {value}
      {onchange}
      type="password"
      autocomplete="off"
      autocapitalize="off"
      spellcheck="false"
    />
  </div>
{/snippet}

<div class="cloud" bind:this={root}>
  <p class="status {status.tone}" role="status">
    <span class="light" aria-hidden="true"></span>
    <span>{status.text}</span>
  </p>

  <Field
    label={t('settings.inference.permission.label')}
    description={t('settings.inference.permission.description')}
    layout="row"
  >
    {#snippet children({ labelId })}
      <Segmented
        options={permissionOptions}
        value={session.cloudAllowed ? 'on' : 'off'}
        labelledBy={labelId}
        onchange={changePermission}
      />
    {/snippet}
  </Field>

  {#if attention}
    <section class="attention" aria-labelledby={ids.attention}>
      <h3 class="title" id={ids.attention}>{t('settings.inference.recovery.title')}</h3>
      <ul class="issues">
        {#if unfinished}
          <li class="issue">
            <Icon name="warning-triangle" size={13} />
            <div class="issue-body">
              <p>
                {t('settings.inference.recovery.unfinished', {
                  providerKey: providerKey(unfinished.provider),
                  id: unfinished.installationId,
                })}
              </p>
              <p class="note">{t('settings.inference.recovery.forgetNote')}</p>
              <div class="actions">
                <Button
                  size="sm"
                  variant="soft"
                  data-resume={unfinished.installationId}
                  onclick={() => resumeUnfinished(unfinished)}
                >
                  {t('settings.inference.recovery.resume')}
                </Button>
                <Button size="sm" onclick={forgetUnfinished}>{t('settings.inference.recovery.forget')}</Button>
              </div>
            </div>
          </li>
        {/if}
        {#if missingToken}
          <li class="issue">
            <Icon name="warning-triangle" size={13} />
            <div class="issue-body">
              <p>{t('settings.inference.recovery.noToken', { name: missingToken.name })}</p>
              <div class="actions">
                {#if madeBySetup(missingToken) && !task && !setupRunning}
                  <Button
                    size="sm"
                    variant="soft"
                    data-newtoken={missingToken.key}
                    onclick={() =>
                      openTask({ action: 'resume', provider: missingToken.provider, installationId: missingToken.id }, [
                        'data-newtoken',
                        missingToken.key,
                      ])}
                  >
                    {t('settings.inference.recovery.newToken')}
                  </Button>
                {/if}
                <Button size="sm" onclick={() => openTokenForm(missingToken)}>{t('settings.inference.token.add')}</Button>
              </div>
            </div>
          </li>
        {/if}
        {#if outdated}
          <li class="issue">
            <Icon name="warning-triangle" size={13} />
            <div class="issue-body">
              <p>{t(cloudGpu.unsupported === 'outdated' ? 'settings.inference.recovery.outdated' : 'settings.inference.recovery.olderCode', { name: outdated.name })}</p>
              {#if !task && !setupRunning}
                <div class="actions">
                  <Button
                    size="sm"
                    variant="soft"
                    data-update={outdated.key}
                    onclick={() =>
                      openTask({ action: 'resume', provider: outdated.provider, installationId: outdated.id }, [
                        'data-update',
                        outdated.key,
                      ])}
                  >
                    {t('settings.inference.recovery.update')}
                  </Button>
                </div>
              {/if}
            </div>
          </li>
        {/if}
      </ul>
    </section>
  {/if}

  <section class="endpoints" aria-labelledby={ids.endpoints} data-settings-anchor="endpoints">
    <h3 class="title" id={ids.endpoints} tabindex="-1" data-anchor-focus>{t('settings.inference.endpoints.title')}</h3>

    {#if loadFailed}
      <div class="problem" role="alert">
        <Icon name="warning-triangle" size={13} />
        <p>{t('settings.inference.endpoints.loadFailed')}</p>
        <Button size="sm" onclick={() => void reload()}>{t('settings.inference.endpoints.retry')}</Button>
      </div>
    {:else if loaded && endpoints.length === 0}
      <p class="empty">{t('settings.inference.endpoints.empty')}</p>
    {:else if endpoints.length > 0}
      {#if endpoints.length > 1}<p class="note switch">{t('settings.inference.endpoints.switchNote')}</p>{/if}
      <ul class="list">
        {#each endpoints as ep, index (ep.key)}
          {@const test = tests[ep.key]}
          {@const chosen = selectedKey === ep.key}
          <li class="row" class:chosen>
            <div class="main">
              <input
                type="radio"
                class="pick"
                id="{uid}-pick-{index}"
                name="{uid}-default"
                value={ep.key}
                checked={chosen}
                disabled={providerPaused(ep.provider) && !chosen}
                aria-label={t('settings.inference.endpoints.useDefault', { name: ep.name })}
                onchange={() => selectDefault(ep)}
              />
              <div class="text">
                <div class="name-line">
                  <label class="name" for="{uid}-pick-{index}">{ep.name}</label>
                  {#if chosen}<span class="badge">{t('settings.inference.endpoints.default')}</span>{/if}
                  {#if providerPaused(ep.provider)}<span class="badge paused" data-paused>{t('settings.inference.endpoints.paused')}</span>{/if}
                  <button type="button" class="link" data-rename={ep.key} onclick={() => openRename(ep)}>
                    {t('settings.inference.rename.action')}
                  </button>
                </div>
                <div class="meta" title={ep.host}>
                  {ep.account
                    ? t('settings.inference.endpoints.account', { providerKey: providerKey(ep.provider), account: ep.account })
                    : t('settings.inference.endpoints.meta', { providerKey: providerKey(ep.provider), host: ep.host })}
                </div>
                <div class="facts">
                  <span class="token" class:missing={tokens[ep.key] === false}>{tokenText(ep.key)}</span>
                  <button type="button" class="link" data-token={ep.key} onclick={() => openTokenForm(ep)}>
                    {tokens[ep.key] === true ? t('settings.inference.token.replace') : t('settings.inference.token.add')}
                  </button>
                </div>
                <p class="check" class:bad={test?.state === 'done' && !test.ok} aria-live="polite">
                  {#if test?.state === 'testing'}
                    {t('settings.inference.endpoints.testing')}
                  {:else if test?.state === 'done'}
                    <span>{t(test.status === 'gateway_out_of_date' ? 'cloud.recovery.gatewayOutOfDate' : healthKey(test.status))}</span>
                    {#if test.ok && test.latency !== null}
                      <span class="latency">{t('settings.inference.health.latency', { latency: test.latency })}</span>
                    {/if}
                  {/if}
                </p>
              </div>
              <div class="row-actions">
                {#if ep.provider === 'modal' && madeBySetup(ep) && !task && !setupRunning}
                  <!-- Always here: redeploy this version's code, or change the
                       setup's options (page denoise, GPU, idle time). -->
                  <Button
                    size="sm"
                    data-update-row={ep.key}
                    onclick={() => openTask({ action: 'resume', provider: ep.provider, installationId: ep.id }, ['data-update-row', ep.key])}
                  >
                    {t('settings.inference.endpoints.update')}
                  </Button>
                {/if}
                <Button size="sm" disabled={providerPaused(ep.provider)} onclick={() => testEndpoint(ep)}>{t('settings.inference.endpoints.test')}</Button>
                <Button size="sm" data-remove={ep.key} onclick={() => askRemove(ep)}>
                  {t('settings.inference.endpoints.remove')}
                </Button>
              </div>
            </div>

            {#if renameFor === ep.key}
              <form
                class="inline"
                onsubmit={(event) => {
                  event.preventDefault()
                  void saveRename(ep)
                }}
              >
                <div class="key">
                  <label class="key-label" for={ids.rename}>{t('settings.inference.rename.label', { name: ep.name })}</label>
                  <TextInput
                    id={ids.rename}
                    value={renameValue}
                    maxlength="128"
                    autocomplete="off"
                    onchange={(/** @type {string} */ value) => (renameValue = value)}
                    onkeydown={(/** @type {KeyboardEvent} */ event) => {
                      if (event.key !== 'Escape') return
                      // Closes this form, not the Settings dialog around it.
                      event.stopPropagation()
                      closeRename(ep)
                    }}
                  />
                </div>
                {#if renameError}<p class="error" role="alert">{t(renameError)}</p>{/if}
                <div class="actions end">
                  <Button size="sm" onclick={() => closeRename(ep)}>{t('shell.action.cancel')}</Button>
                  <Button size="sm" variant="primary" type="submit">
                    {renameSaving ? t('settings.inference.rename.saving') : t('settings.inference.rename.save')}
                  </Button>
                </div>
              </form>
            {/if}

            {#if tokenFor === ep.key}
              <form
                class="inline"
                onsubmit={(event) => {
                  event.preventDefault()
                  void saveToken(ep)
                }}
              >
                {#if ep.provider === 'modal'}
                  {@render secretField(ids.tokenId, t('settings.inference.token.modalTokenId'), tokenId, (/** @type {string} */ value) => (tokenId = value))}
                  {@render secretField(ids.tokenSecret, t('settings.inference.token.modalTokenSecret'), tokenSecret, (/** @type {string} */ value) => (tokenSecret = value))}
                {:else}
                  {@render secretField(ids.tokenSecret, t('settings.inference.token.beamToken'), tokenSecret, (/** @type {string} */ value) => (tokenSecret = value))}
                {/if}
                {#if tokenError}<p class="error" role="alert">{t(tokenError)}</p>{/if}
                <div class="actions end">
                  <Button size="sm" onclick={() => closeTokenForm(ep)}>{t('shell.action.cancel')}</Button>
                  <Button size="sm" variant="primary" type="submit">
                    {tokenSaving ? t('settings.inference.token.saving') : t('settings.inference.token.save')}
                  </Button>
                </div>
              </form>
            {/if}

            {#if removingKey === ep.key}
              <div class="inline confirm" role="group" aria-labelledby={ids.removeText}>
                <p class="confirm-text" id={ids.removeText} tabindex="-1">
                  {t('settings.inference.remove.confirm', { name: ep.name })}
                </p>
                {#if madeBySetup(ep)}
                  <label class="check-row">
                    <input
                      type="checkbox"
                      checked={alsoDelete}
                      disabled={setupRunning}
                      onchange={(event) => (alsoDelete = event.currentTarget.checked)}
                    />
                    <span>{t('settings.inference.remove.alsoDelete')}</span>
                  </label>
                {/if}
                <p class="note">
                  {alsoDelete
                    ? t('settings.inference.remove.deleteNote', { providerKey: providerKey(ep.provider) })
                    : t('settings.inference.remove.keepNote', { providerKey: providerKey(ep.provider) })}
                </p>
                {#if removeError}<p class="error" role="alert">{t(removeError)}</p>{/if}
                <div class="actions end">
                  <Button size="sm" onclick={() => cancelRemove(ep)}>{t('shell.action.cancel')}</Button>
                  <Button size="sm" variant="primary" onclick={() => confirmRemove(ep)}>
                    {alsoDelete ? t('settings.inference.remove.review') : t('settings.inference.remove.confirmButton')}
                  </Button>
                </div>
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
    {#if listError}<p class="error" role="alert">{t(listError)}</p>{/if}

    <div class="slot" bind:this={slot}>
      {#if task}
        {#key task}
          <CloudProvisioner
            inline
            existing={task.action === 'setup' ? null : task}
            onclose={closeTask}
            onconfigured={configured}
            oncleaned={() => void reload()}
            {backend}
          />
        {/key}
      {:else}
        <div class="setup">
          <Button
            variant={loaded && endpoints.length === 0 ? 'primary' : 'soft'}
            data-setup="open"
            onclick={() => openTask({ action: 'setup' }, ['data-setup', 'open'])}
          >
            {t('settings.inference.setup.action')}
          </Button>
          <p class="note">{t('settings.inference.setup.description')}</p>
        </div>
      {/if}
    </div>
  </section>

  <Disclosure
    variant="plain"
    open={manualOpen}
    ontoggle={(/** @type {boolean} */ open) => {
      manualOpen = open
      if (!open) wipeManualSecrets()
    }}
  >
    {#snippet summary()}{t('settings.inference.connect.title')}{/snippet}
    <form
      class="manual"
      onsubmit={(event) => {
        event.preventDefault()
        void connectManual()
      }}
    >
      <p class="note">{t('settings.inference.connect.description')}</p>
      <Field label={t('settings.inference.provider.label')}>
        {#snippet children({ labelId })}
          <Segmented
            options={providerOptions}
            value={manualProvider}
            labelledBy={labelId}
            align="start"
            onchange={(value) => {
              manualProvider = value === 'beam' ? 'beam' : 'modal'
              wipeManualSecrets()
            }}
          />
        {/snippet}
      </Field>
      <Field label={t('settings.inference.connect.name')} controlId={ids.name}>
        {#snippet children()}
          <TextInput id={ids.name} value={manualName} onchange={(/** @type {string} */ value) => (manualName = value)} />
        {/snippet}
      </Field>
      <Field label={t('settings.inference.connect.endpoint')} controlId={ids.url}>
        {#snippet children()}
          <TextInput
            id={ids.url}
            value={manualUrl}
            placeholder={t('settings.inference.endpoint.placeholder')}
            autocapitalize="off"
            spellcheck="false"
            onchange={(/** @type {string} */ value) => (manualUrl = value)}
          />
        {/snippet}
      </Field>
      {#if manualProvider === 'modal'}
        {@render secretField(ids.manualTokenId, t('settings.inference.token.modalTokenId'), manualTokenId, (/** @type {string} */ value) => (manualTokenId = value))}
        {@render secretField(ids.manualSecret, t('settings.inference.token.modalTokenSecret'), manualSecret, (/** @type {string} */ value) => (manualSecret = value))}
      {:else}
        {@render secretField(ids.manualSecret, t('settings.inference.token.beamToken'), manualSecret, (/** @type {string} */ value) => (manualSecret = value))}
      {/if}
      {#if manualError}<p class="error" role="alert">{t(manualError)}</p>{/if}
      {#if manualNote}
        <p class="result" class:bad={!manualNote.ok} role="status">{t(manualNote.key, manualNote.params)}</p>
      {/if}
      <div class="actions end">
        <Button variant="primary" type="submit">
          {manualSaving ? t('settings.inference.connect.connecting') : t('settings.inference.connect.connect')}
        </Button>
      </div>
    </form>
  </Disclosure>

  <Disclosure variant="plain" open={logsOpen} ontoggle={(/** @type {boolean} */ open) => (logsOpen = open)}>
    {#snippet summary()}{t('settings.inference.logs.title', { count: attempts.length })}{/snippet}
    <div class="logs">
      <div class="actions">
        <Button size="sm" disabled={recoveryBusy} onclick={() => recoveryAction(() => retryCloudRecovery(backend))}>
          {t('cloud.recovery.check')}
        </Button>
        {#if (cloud.recovery?.needsAttention.length ?? 0) > 1}
          <Button size="sm" disabled={recoveryBusy} onclick={dismissAllAttempts}>{t('settings.inference.logs.dismissAll')}</Button>
        {/if}
      </div>
      {#if attempts.length === 0}
        <p class="empty">{t('settings.inference.logs.empty')}</p>
      {:else}
        <ul class="issues">
          {#each attempts as entry (entry.attemptId)}
            <li class="issue">
              <Icon name="warning-triangle" size={13} />
              <div class="issue-body">
                <p>
                  {typeof entry.pageIndex === 'number'
                    ? t('settings.inference.recovery.attempt', {
                        page: entry.pageIndex + 1,
                        reasonKey: attemptReasonKey(entry.reason),
                      })
                    : t('settings.inference.recovery.attemptNoPage', { reasonKey: attemptReasonKey(entry.reason) })}
                </p>
                <div class="actions">
                  {#if recoveryActions(entry.reason, entry.canAbandon).abandon}
                    <Button size="sm" disabled={recoveryBusy} onclick={() => { confirmingAbandon = entry.attemptId }}>
                      {t('cloud.recovery.abandon')}
                    </Button>
                  {/if}
                  <Button size="sm" disabled={recoveryBusy} onclick={() => recoveryAction(() => dismissRecovered(entry.attemptId, backend))}>
                    {t('settings.inference.recovery.dismiss')}
                  </Button>
                </div>
                {#if confirmingAbandon === entry.attemptId}
                  <p role="alert">{t('cloud.recovery.duplicateRisk')}</p>
                  <div class="actions">
                    <Button size="sm" disabled={recoveryBusy} onclick={() => recoveryAction(() => abandonRecovered(entry.attemptId, true, backend))}>
                      {t('cloud.recovery.confirmAbandon')}
                    </Button>
                    <Button size="sm" onclick={() => { confirmingAbandon = null }}>{t('cloud.recovery.keepAttempt')}</Button>
                  </div>
                {/if}
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </Disclosure>
</div>

<style>
  .cloud {
    display: grid;
    gap: var(--s-4);
    min-width: 0;
  }

  .title {
    margin: 0 0 var(--s-2);
    font-size: 11px;
    font-weight: 600;
    color: var(--t3);
    outline: none;
  }

  .note,
  .empty,
  .error,
  .result {
    margin: 0;
    font-size: 11.5px;
    line-height: 1.5;
  }
  .note,
  .empty { color: var(--t3) }
  .error { color: var(--warn) }
  .result { color: var(--t2) }
  .result.bad { color: var(--warn) }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-2);
  }
  .actions.end { justify-content: flex-end }

  /* ---------- status ---------- */

  .status {
    display: flex;
    gap: var(--s-3);
    align-items: center;
    margin: 0;
    padding: 9px 12px;
    border-radius: var(--r-md);
    background: var(--panel2);
    color: var(--text);
    line-height: 1.45;
  }
  .light {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--t3);
  }
  .status.ready .light { background: var(--accent) }
  .status.attention .light { background: var(--warn) }
  .status.checking .light { animation: mcBlink 1.2s var(--ease) infinite }
  .status.off { color: var(--t2) }

  /* ---------- needs attention ---------- */

  .issues {
    display: grid;
    gap: var(--s-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .issue {
    display: flex;
    gap: var(--s-3);
    align-items: flex-start;
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    color: var(--warn);
  }
  .issue :global(svg) { flex: none; margin-top: 2px }
  .issue-body {
    display: grid;
    gap: var(--s-2);
    min-width: 0;
  }
  .issue-body > p:first-child {
    margin: 0;
    line-height: 1.5;
    color: var(--text);
  }

  .logs {
    display: grid;
    gap: var(--s-2);
  }

  /* ---------- endpoints ---------- */

  .problem {
    display: flex;
    gap: var(--s-3);
    align-items: center;
    padding: 10px 12px;
    border-radius: var(--r-md);
    background: var(--panel2);
    color: var(--warn);
  }
  .problem p {
    flex: 1;
    margin: 0;
    line-height: 1.5;
    color: var(--text);
  }

  .list {
    display: grid;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--line);
    list-style: none;
  }
  .row {
    display: grid;
    gap: var(--s-3);
    padding: 10px 0;
    border-bottom: 1px solid var(--line);
  }
  .main {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    gap: var(--s-3);
    align-items: start;
  }
  .pick {
    margin: 3px 0 0;
    accent-color: var(--accent);
  }
  .text {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .name-line {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-2);
    align-items: baseline;
  }
  .name {
    min-width: 0;
    overflow-wrap: anywhere;
    color: var(--text);
    cursor: pointer;
  }
  .badge {
    padding: 0 6px;
    border-radius: var(--r-pill);
    background: var(--accent-soft);
    color: var(--text);
    font-size: 10.5px;
    line-height: 16px;
  }
  .badge.paused { background: var(--line); color: var(--t2) }
  .meta {
    font-size: 11.5px;
    color: var(--t3);
    overflow-wrap: anywhere;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--s-3);
    align-items: baseline;
    font-size: 11.5px;
  }
  .token { color: var(--t2) }
  .token.missing { color: var(--warn) }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--t2);
    font: inherit;
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .link:hover,
  .link:focus { color: var(--text) }
  .name-line .link { font-size: 11.5px }
  .switch { margin-bottom: var(--s-2) }
  .check {
    margin: 0;
    font-size: 11.5px;
    color: var(--t2);
  }
  .check.bad { color: var(--warn) }
  .latency {
    margin-left: var(--s-2);
    color: var(--t3);
    font-variant-numeric: tabular-nums;
  }
  .row-actions {
    display: flex;
    gap: var(--s-2);
  }

  .inline {
    display: grid;
    gap: var(--s-3);
    margin-left: 22px;
    padding: 10px 12px;
    border-radius: var(--r-md);
    background: var(--panel2);
  }
  .confirm-text {
    margin: 0;
    line-height: 1.5;
    color: var(--text);
    outline: none;
  }
  .check-row {
    display: flex;
    gap: var(--s-3);
    align-items: flex-start;
    line-height: 1.5;
    color: var(--text);
    cursor: pointer;
  }
  .check-row input {
    flex: none;
    margin: 3px 0 0;
    accent-color: var(--accent);
  }

  .key { display: grid; gap: var(--s-1) }
  .key-label { font-size: 11.5px; color: var(--t2) }

  .slot { margin-top: var(--s-3) }
  .setup {
    display: grid;
    gap: var(--s-2);
    justify-items: start;
  }

  /* ---------- manual ---------- */

  .manual {
    display: grid;
    gap: var(--s-3);
    padding-top: var(--s-2);
  }

  @media (max-width: 560px) {
    .main { grid-template-columns: auto minmax(0, 1fr) }
    .row-actions { grid-column: 2 }
  }
</style>
