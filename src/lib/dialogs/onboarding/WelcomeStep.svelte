<script>
  /**
   * Setup's cover: one line on what the app is, the way in, where the
   * source lives, and the language, chosen before anything else has to be
   * read. The heading is the frame's, like every step's.
   */
  import { Button, Select } from '../../ui/index.js'
  import Icon from '../../icons/Icon.svelte'
  import { LOCALES, t } from '../../i18n/index.js'
  import { session, setLanguage } from '../../state/session.svelte.js'
  import { openExternal } from '../../api/external.js'
  import { nextFirstLaunchStep } from '../firstlaunch.svelte.js'

  const SOURCE_URL = 'https://github.com/k-omiq/manga-cleaner'

  /** Each language by its own name, so it can be found from any other. */
  const languages = LOCALES.map(({ tag, name }) => ({ value: tag, label: name }))

  /** @param {MouseEvent} event */
  function openSource(event) {
    event.preventDefault()
    openExternal(SOURCE_URL)
  }
</script>

<p class="body">{t('onboarding.welcome.body')}</p>
<div class="actions">
  <Button variant="primary" size="xl" onclick={nextFirstLaunchStep}>{t('onboarding.action.start')}</Button>
  <a class="source" href={SOURCE_URL} onclick={openSource}>
    <span class="mark" aria-hidden="true"></span>{t('onboarding.welcome.source')}
  </a>
</div>
<div class="language">
  <Icon name="globe" size={15} />
  <Select options={languages} value={session.language} label={t('settings.language.label')} fit onchange={setLanguage} />
</div>

<style>
  .body {
    margin: 0 0 var(--s-8);
    max-width: 44ch;
    font-size: 14px;
    line-height: 1.55;
    color: var(--t2);
  }
  .actions { display: flex; align-items: center; gap: var(--s-6) }
  .source {
    display: inline-flex;
    align-items: center;
    gap: var(--s-3);
    border-radius: var(--r-sm);
    color: var(--t2);
    font-size: 12px;
  }
  .source:hover { color: var(--text) }
  .language {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    margin-top: var(--s-8);
    color: var(--t2);
  }
  /* The GitHub mark, drawn in the text colour: the file is a black glyph used
     as a mask, so it follows every theme without a second copy. */
  .mark {
    width: 15px;
    height: 15px;
    background: currentColor;
    -webkit-mask: url('/github.svg') center / contain no-repeat;
    mask: url('/github.svg') center / contain no-repeat;
  }
</style>
