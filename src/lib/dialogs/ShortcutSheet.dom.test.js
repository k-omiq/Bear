/**
 * The shortcut sheet's tabs and its search: one tab open at a time, a search
 * across all of them, and the entries that ship with no key reading Not set
 * until the user records one.
 */
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte'
import { afterEach, describe, expect, it } from 'vitest'
import { t } from '../i18n/index.js'
import { resetAllShortcutBindings, session, setShortcutBinding } from '../state/session.svelte.js'
import ShortcutSheet from './ShortcutSheet.svelte'

afterEach(() => {
  cleanup()
  resetAllShortcutBindings()
})

/** @param {string} labelKey */
function tab(labelKey) {
  return screen.getByRole('tab', { name: new RegExp(`^${t(labelKey)}`) })
}

/** @param {string} labelKey */
function row(labelKey) {
  return /** @type {HTMLElement} */ (screen.getByText(t(labelKey)).closest('li'))
}

function search() {
  return /** @type {HTMLInputElement} */ (screen.getByRole('textbox', { name: t('shortcuts.search.label') }))
}

describe('the shortcut sheet', () => {
  it('opens one tab at a time, and moves along the tabs on the arrows', async () => {
    render(ShortcutSheet)
    await fireEvent.click(tab('shortcuts.tab.tools'))
    expect(tab('shortcuts.tab.tools').getAttribute('aria-selected')).toBe('true')
    expect(screen.getByText(t('tools.name.eraser'))).toBeTruthy()
    expect(screen.queryByText(t('shortcuts.edit.undo'))).toBeNull()

    await fireEvent.click(tab('shortcuts.tab.edit'))
    expect(screen.getByText(t('shortcuts.edit.undo'))).toBeTruthy()
    expect(screen.queryByText(t('tools.name.eraser'))).toBeNull()

    await fireEvent.keyDown(tab('shortcuts.tab.edit'), { key: 'ArrowRight' })
    expect(tab('shortcuts.tab.view').getAttribute('aria-selected')).toBe('true')
    // A tab of several groups names each of them.
    expect(screen.getByText(t('shortcuts.group.zoom'))).toBeTruthy()
    expect(screen.getByText(t('shortcuts.zoom.fit'))).toBeTruthy()
  })

  it('searches every tab at once, a single character as a key, and says when nothing matches', async () => {
    render(ShortcutSheet)
    await fireEvent.click(tab('shortcuts.tab.tools'))
    await fireEvent.input(search(), { target: { value: 'undo' } })
    expect(screen.getByText(t('shortcuts.edit.undo'))).toBeTruthy()
    expect(screen.queryByText(t('tools.name.eraser'))).toBeNull()
    expect(within(tab('shortcuts.tab.edit')).getByText('1')).toBeTruthy()

    // `m` is the mask overlay's key, not every name with an m in it.
    await fireEvent.input(search(), { target: { value: 'm' } })
    expect(screen.getByText(t('shortcuts.view.maskOverlay'))).toBeTruthy()
    expect(screen.queryByText(t('shortcuts.edit.undo'))).toBeNull()

    // The mouse's rows are searched too.
    await fireEvent.input(search(), { target: { value: t('shortcuts.navigation.wheel') } })
    expect(screen.getByText(t('shortcuts.navigation.wheel'))).toBeTruthy()

    await fireEvent.input(search(), { target: { value: 'zzzz' } })
    expect(screen.getByText(t('shortcuts.search.empty', { query: 'zzzz' }))).toBeTruthy()

    // Escape empties the search before it closes anything.
    await fireEvent.keyDown(search(), { key: 'Escape' })
    expect(search().value).toBe('')
    expect(screen.getByText(t('tools.name.eraser'))).toBeTruthy()
  })

  it('reads Not set for the run keys that ship with none, and shows a key the user gives one', async () => {
    render(ShortcutSheet)
    await fireEvent.click(tab('shortcuts.tab.run'))
    const solid = row('shortcuts.run.solidBubbles')
    expect(within(solid).getByText(t('shortcuts.rebind.unbound'))).toBeTruthy()
    expect(within(row('shortcuts.run.fullClean')).getByText(t('shortcuts.rebind.unbound'))).toBeTruthy()
    expect(within(solid).queryByRole('button', { name: t('shortcuts.rebind.reset', { name: t('shortcuts.run.solidBubbles') }) })).toBeNull()

    // Recorded from the sheet itself, as a user would.
    await fireEvent.click(within(solid).getByRole('button', { name: t('shortcuts.rebind.change', { name: t('shortcuts.run.solidBubbles') }) }))
    await fireEvent.keyDown(window, { key: 'F9' })
    expect(session.shortcuts).toEqual({ 'run.solidBubbles': { key: 'F9', shift: false, accel: false } })
    expect(within(row('shortcuts.run.solidBubbles')).getByText('F9')).toBeTruthy()

    // A reset gives it no key again.
    await fireEvent.click(within(row('shortcuts.run.solidBubbles')).getByRole('button', { name: t('shortcuts.rebind.reset', { name: t('shortcuts.run.solidBubbles') }) }))
    expect(session.shortcuts).toEqual({})
    expect(within(row('shortcuts.run.solidBubbles')).getByText(t('shortcuts.rebind.unbound'))).toBeTruthy()
  })

  it('says which shortcut holds a key the user tries to give another', async () => {
    setShortcutBinding('run.solidBubbles', { key: 'F8' })
    render(ShortcutSheet)
    await fireEvent.click(tab('shortcuts.tab.run'))
    const full = row('shortcuts.run.fullClean')
    await fireEvent.click(within(full).getByRole('button', { name: t('shortcuts.rebind.change', { name: t('shortcuts.run.fullClean') }) }))
    await fireEvent.keyDown(window, { key: 'F8' })
    expect(screen.getByRole('status').textContent).toContain(t('shortcuts.run.solidBubbles'))
    expect(Object.keys(session.shortcuts)).toEqual(['run.solidBubbles'])

    // Still listening, so "choose another" is the next key.
    expect(within(row('shortcuts.run.fullClean')).getByText(t('shortcuts.rebind.listening'))).toBeTruthy()
    await fireEvent.keyDown(window, { key: 'F10' })
    expect(session.shortcuts['run.fullClean']).toEqual({ key: 'F10', shift: false, accel: false })
    expect(within(row('shortcuts.run.fullClean')).getByText('F10')).toBeTruthy()
    expect(screen.getByRole('status').textContent).toBe('')
  })

  it('cancels on Escape after a conflict, and drops the message', async () => {
    setShortcutBinding('run.solidBubbles', { key: 'F8' })
    render(ShortcutSheet)
    await fireEvent.click(tab('shortcuts.tab.run'))
    await fireEvent.click(within(row('shortcuts.run.fullClean')).getByRole('button', { name: t('shortcuts.rebind.change', { name: t('shortcuts.run.fullClean') }) }))
    await fireEvent.keyDown(window, { key: 'F8' })
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(within(row('shortcuts.run.fullClean')).getByText(t('shortcuts.rebind.unbound'))).toBeTruthy()
    expect(screen.getByRole('status').textContent).toBe('')
    await fireEvent.keyDown(window, { key: 'F10' })
    expect(Object.keys(session.shortcuts)).toEqual(['run.solidBubbles'])
  })

  it('stops listening on a click elsewhere, which WebKit does not turn into a blur', async () => {
    render(ShortcutSheet)
    await fireEvent.click(tab('shortcuts.tab.run'))
    const solid = row('shortcuts.run.solidBubbles')
    await fireEvent.click(within(solid).getByRole('button', { name: t('shortcuts.rebind.change', { name: t('shortcuts.run.solidBubbles') }) }))
    expect(within(row('shortcuts.run.solidBubbles')).getByText(t('shortcuts.rebind.listening'))).toBeTruthy()
    await fireEvent.pointerDown(search(), { button: 0 })
    expect(within(row('shortcuts.run.solidBubbles')).getByText(t('shortcuts.rebind.unbound'))).toBeTruthy()
    await fireEvent.keyDown(window, { key: 'u' })
    expect(session.shortcuts).toEqual({})
  })

  it('keeps a row it just rebound in the results, though it no longer matches', async () => {
    render(ShortcutSheet)
    await fireEvent.input(search(), { target: { value: 'm' } })
    const overlay = row('shortcuts.view.maskOverlay')
    await fireEvent.click(within(overlay).getByRole('button', { name: t('shortcuts.rebind.change', { name: t('shortcuts.view.maskOverlay') }) }))
    await fireEvent.keyDown(window, { key: 'F7' })
    expect(session.shortcuts).toEqual({ 'view.maskOverlay': { key: 'F7', shift: false, accel: false } })
    expect(within(row('shortcuts.view.maskOverlay')).getByText('F7')).toBeTruthy()
    // A new search lets it go.
    await fireEvent.input(search(), { target: { value: 'z' } })
    expect(screen.queryByText(t('shortcuts.view.maskOverlay'))).toBeNull()
  })
})
