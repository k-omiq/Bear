<script>
  import { t } from '../i18n/index.js'
  import { openExternal } from '../api/external.js'

  let { compact = false } = $props()

  const INVITE_URL = 'https://discord.gg/3ueKc9PaX7'

  /** @param {MouseEvent} event */
  function openInvite(event) {
    event.preventDefault()
    openExternal(INVITE_URL)
  }
</script>

<section class="invite" class:compact>
  <div class="intro">
    <span class="discord-mark" aria-hidden="true">
      <svg viewBox="0 0 127.14 96.36" fill="currentColor" focusable="false">
        <path d="M104.4 8.7A102.7 102.7 0 0 0 79.1.9a71.6 71.6 0 0 0-3.2 6.6 95 95 0 0 0-28.7 0A71.6 71.6 0 0 0 44 .9 102.7 102.7 0 0 0 18.7 8.7C2.7 32.4-1.6 55.6.6 78.5a103 103 0 0 0 31 15.7 75 75 0 0 0 6.6-10.7 66.4 66.4 0 0 1-10.4-5c.9-.6 1.7-1.3 2.5-2a73.2 73.2 0 0 0 66.5 0l2.5 2a66.4 66.4 0 0 1-10.4 5 75 75 0 0 0 6.6 10.7 103 103 0 0 0 31-15.7c2.7-26.5-4.5-49.5-22.1-69.8ZM42.5 64.4c-6.3 0-11.5-5.7-11.5-12.7s5.1-12.7 11.5-12.7c6.4 0 11.6 5.7 11.5 12.7 0 7-5.1 12.7-11.5 12.7Zm42.1 0c-6.3 0-11.5-5.7-11.5-12.7s5.1-12.7 11.5-12.7c6.4 0 11.6 5.7 11.5 12.7 0 7-5.1 12.7-11.5 12.7Z" />
      </svg>
    </span>
    <div>
      {#if compact}<h3>{t('onboarding.community.heading')}</h3>{/if}
      <p>{t('onboarding.community.intro')}</p>
    </div>
  </div>

  <ul class="reasons">
    <li><strong>{t('onboarding.community.bugs')}</strong><span>{t('onboarding.community.bugsDetail')}</span></li>
    <li><strong>{t('onboarding.community.updates')}</strong><span>{t('onboarding.community.updatesDetail')}</span></li>
    <li><strong>{t('onboarding.community.ideas')}</strong><span>{t('onboarding.community.ideasDetail')}</span></li>
  </ul>

  <div class="action">
    <a class="join" href={INVITE_URL} target="_blank" rel="noopener noreferrer" onclick={openInvite}>
      <svg viewBox="0 0 127.14 96.36" fill="currentColor" aria-hidden="true" focusable="false">
        <path d="M104.4 8.7A102.7 102.7 0 0 0 79.1.9a71.6 71.6 0 0 0-3.2 6.6 95 95 0 0 0-28.7 0A71.6 71.6 0 0 0 44 .9 102.7 102.7 0 0 0 18.7 8.7C2.7 32.4-1.6 55.6.6 78.5a103 103 0 0 0 31 15.7 75 75 0 0 0 6.6-10.7 66.4 66.4 0 0 1-10.4-5c.9-.6 1.7-1.3 2.5-2a73.2 73.2 0 0 0 66.5 0l2.5 2a66.4 66.4 0 0 1-10.4 5 75 75 0 0 0 6.6 10.7 103 103 0 0 0 31-15.7c2.7-26.5-4.5-49.5-22.1-69.8ZM42.5 64.4c-6.3 0-11.5-5.7-11.5-12.7s5.1-12.7 11.5-12.7c6.4 0 11.6 5.7 11.5 12.7 0 7-5.1 12.7-11.5 12.7Zm42.1 0c-6.3 0-11.5-5.7-11.5-12.7s5.1-12.7 11.5-12.7c6.4 0 11.6 5.7 11.5 12.7 0 7-5.1 12.7-11.5 12.7Z" />
      </svg>
      {t('onboarding.community.join')}
    </a>
    <span class="external">{t('onboarding.community.external')}</span>
  </div>
</section>

<style>
  .invite {
    padding: var(--s-7);
    border: 1px solid var(--line2);
    border-radius: var(--r-xl);
    background: var(--card);
    box-shadow: var(--edge-soft);
  }
  .compact { margin-top: var(--s-8) }
  .intro { display: flex; align-items: center; gap: var(--s-5) }
  .discord-mark {
    display: grid;
    place-items: center;
    flex: none;
    width: 48px;
    height: 48px;
    border-radius: var(--r-lg);
    background: var(--discord-soft);
    color: var(--discord-blurple);
  }
  .discord-mark svg { width: 26px; height: 26px }
  h3 { margin: 0 0 var(--s-1); font-size: 16px; line-height: 1.25; color: var(--text) }
  .intro p { margin: 0; color: var(--t2); font-size: 13px; line-height: 1.5 }
  .reasons {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: var(--s-5);
    margin: var(--s-7) 0;
    padding: var(--s-6) 0;
    list-style: none;
    border-top: 1px solid var(--line);
    border-bottom: 1px solid var(--line);
  }
  .reasons li { display: flex; flex-direction: column; gap: var(--s-2); min-width: 0 }
  .reasons strong { font-size: 12px; font-weight: 600; color: var(--text) }
  .reasons span { color: var(--t2); font-size: 11px; line-height: 1.5 }
  .action { display: flex; align-items: center; flex-wrap: wrap; gap: var(--s-4) }
  .join {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--s-3);
    min-height: 38px;
    padding: 0 var(--s-5);
    border-radius: var(--r-sm);
    background: var(--discord-blurple);
    color: var(--discord-white);
    font-size: 12px;
    font-weight: 600;
    text-decoration: none;
    transition: background var(--dur-fast) var(--ease), transform var(--dur-fast) var(--ease);
  }
  .join:hover { background: var(--discord-hover); transform: translateY(-1px) }
  .join:active { transform: translateY(0) }
  .join:focus-visible { outline: 2px solid var(--discord-blurple); outline-offset: 3px }
  .join svg { width: 17px; height: 17px }
  .external { color: var(--t3); font-size: 11px }
  @media (max-width: 560px) {
    .invite { padding: var(--s-6) }
    .reasons { grid-template-columns: 1fr; gap: var(--s-4) }
  }
</style>
