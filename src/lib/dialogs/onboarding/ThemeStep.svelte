<script>
  /**
   * Setup step: the theme, applied the moment it is picked, so the rest of
   * the setup is already seen in it.
   */
  import { ThemePicker } from '../../ui/index.js'
  import { t } from '../../i18n/index.js'
  import { THEMES, THEME_LABEL_KEYS, session, setTheme } from '../../state/session.svelte.js'
  import { saveFirstLaunchSetting } from '../firstlaunch.svelte.js'

  let saveFailed = $state(false)
  const themes = $derived(THEMES.map((value) => ({ value, label: t(THEME_LABEL_KEYS[value]) })))

  /** @param {string} value */
  async function pick(value) {
    saveFailed = !(await saveFirstLaunchSetting(setTheme, /** @type {any} */ (value), session.theme))
  }
</script>

<ThemePicker options={themes} value={session.theme} onchange={pick} label={t('onboarding.theme.heading')} />
{#if saveFailed}
  <p class="alert" role="alert">{t('onboarding.saveFailed')}</p>
{/if}
