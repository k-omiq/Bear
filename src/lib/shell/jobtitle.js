/**
 * A job's name, as the Jobs list and the quit question show it: the project
 * and the chapter, or as much of that as is known. A job read back from
 * `listJobs` knows only its chapter id until the library has been read
 * (`state/jobs.svelte.js#nameJobs`), and says so rather than showing an id.
 */

import { t } from '../i18n/index.js'

/** @param {import('../state/jobs.svelte.js').Job} job */
export function jobTitle(job) {
  const chapter = job.chapterName
    || (job.chapterNumber != null && job.chapterNumber !== '' ? t('jobs.item.chapter', { number: job.chapterNumber }) : t('jobs.item.unknown'))
  return job.projectName ? t('jobs.item.title', { project: job.projectName, chapter }) : chapter
}
