<script>
  /**
   * Setup step: Normal or Advanced (`DetectionModeChoice`), and for Advanced,
   * where it runs once a cloud GPU can be used.
   *
   * Nothing else is asked here. The languages, the text reader and the
   * review stay at their defaults and live under More options in Settings >
   * Models; the Downloads step fetches what the chosen mode needs.
   */
  import { t } from '../../i18n/index.js'
  import { session } from '../../state/session.svelte.js'
  import { cloudDetectOffered, firstLaunch, modeBytes, pickDetectionMode } from '../firstlaunch.svelte.js'
  import DetectionModeChoice from '../DetectionModeChoice.svelte'
  import DetectOnChoice from './DetectOnChoice.svelte'

  const sizes = $derived({ normal: modeBytes('normal'), advanced: modeBytes('advanced') })
</script>

<p class="lead">{t('onboarding.detection.body')}</p>
<DetectionModeChoice
  value={session.detectionMode}
  onchange={(mode) => void pickDetectionMode(mode)}
  {sizes}
  memory={firstLaunch.memory}
  label={t('pipelines.detection')}
/>
{#if session.detectionMode === 'advanced' && cloudDetectOffered()}
  <div class="where"><DetectOnChoice /></div>
{/if}

<style>
  .where { margin-top: var(--s-6) }
</style>
