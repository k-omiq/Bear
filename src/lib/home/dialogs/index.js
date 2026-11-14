/**
 * Home's own dialogs, keyed by the modal `kind` that raises them.
 *
 * They are on the global modal stack rather than mounted inside `HomeScreen`
 * because the stack is already how they are opened: `src/lib/shortcuts.js`
 * binds `N` at home to `pushModal({kind: 'newProject'})`. The host
 * (`src/lib/shell/ModalHost.svelte`) looks a spec's kind up here and falls back
 * to its generic dialog for everything else - Task 11's Settings, Export and
 * About, and the confirmations that need nothing but a line of copy and two
 * buttons.
 */

export { default as newProject } from './NewProjectDialog.svelte'
export { default as newChapter } from './NewChapterDialog.svelte'
export { default as openProject } from './OpenProjectDialog.svelte'
export { default as renameProject } from './RenameProjectDialog.svelte'
export { default as chapterPages } from './ChapterPagesDialog.svelte'
