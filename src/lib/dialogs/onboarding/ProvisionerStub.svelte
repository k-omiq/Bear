<script>
  /**
   * A stand-in for `CloudProvisioner.svelte`, for `FirstLaunchDialog.dom.test.js`
   * and nothing else: the application never imports it.
   *
   * The setup's cloud step is tested against the provisioner's props (IC-5),
   * not against its flow, which is tested beside it. So this shows the props
   * it was given as data attributes and offers the ways a provisioner ends as
   * buttons: finished with an endpoint that answered its first check,
   * finished with one that did not, each with the payload IC-5 names, or
   * closed. Two more say that it started and stopped working in the user's
   * account.
   */

  /** What a finished Beam setup hands back. */
  const RESULT = Object.freeze({
    provider: 'beam',
    profileId: 'mc-ab12cd',
    endpointUrl: 'https://example.test/mc/v1',
    name: 'Beam (mc-ab12cd)',
    healthy: true,
  })

  /** @type {{inline?: boolean, initialProvider?: string, wantDenoise?: boolean, onclose?: () => void, onconfigured?: (info: typeof RESULT) => unknown, onbusychange?: (busy: boolean) => void}} */
  let { inline = false, initialProvider = 'modal', wantDenoise = false, onclose, onconfigured, onbusychange } = $props()
</script>

<div data-testid="provisioner" data-inline={String(inline)} data-provider={initialProvider} data-want-denoise={String(wantDenoise)}>
  <button type="button" data-testid="provisioner-finish" onclick={() => onconfigured?.(RESULT)}>finish</button>
  <button type="button" data-testid="provisioner-finish-unchecked" onclick={() => onconfigured?.({ ...RESULT, healthy: false })}>
    finish unchecked
  </button>
  <button type="button" data-testid="provisioner-close" onclick={() => onclose?.()}>close</button>
  <button type="button" data-testid="provisioner-busy" onclick={() => onbusychange?.(true)}>busy</button>
  <button type="button" data-testid="provisioner-idle" onclick={() => onbusychange?.(false)}>idle</button>
</div>
