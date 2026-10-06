import { describe, expect, it } from 'vitest'
import { RUNGS, currentRung, rungLabel, simpler, stronger } from './ladder.js'

describe('engine ladder clamping', () => {
  it('stronger stops at the top rung, flux', () => {
    expect(RUNGS.at(-1)).toBe('flux')
    expect(stronger('flux')).toBe('flux')
  })

  it('simpler stops at the bottom rung, fill', () => {
    expect(RUNGS[0]).toBe('fill')
    expect(simpler('fill')).toBe('fill')
  })

  it('steps exactly one rung at a time in between', () => {
    expect(RUNGS).toEqual(['fill', 'lama', 'flux'])
    expect(stronger('fill')).toBe('lama')
    expect(stronger('lama')).toBe('flux')
    expect(simpler('flux')).toBe('lama')
    expect(simpler('lama')).toBe('fill')
  })

  // Denoise fill was rung 1 and is gone. A patch, a pick or a ceiling saved
  // while it existed reads as a fill, and steps from there.
  it('reads the retired denoise rung as fill', () => {
    expect(RUNGS).not.toContain('denoise')
    expect(currentRung('denoise')).toBe('fill')
    expect(currentRung('lama')).toBe('lama')
    expect(currentRung('cloud')).toBe('cloud')
    expect(stronger('denoise')).toBe('lama')
    expect(simpler('denoise')).toBe('fill')
    expect(rungLabel('denoise')).toBe('ladder.rung.fill')
  })

  it('preserves legacy cloud stepping safely', () => {
    expect(stronger('cloud')).toBe('cloud')
    expect(simpler('cloud')).toBe('lama')
  })

  it('excludes paint and clone hand tools from ladder rungs', () => {
    expect(RUNGS).not.toContain('paint')
    expect(RUNGS).not.toContain('clone')
    expect(stronger('paint')).toBe('paint')
    expect(simpler('clone')).toBe('clone')
  })
})

describe('rungLabel', () => {
  it('returns the i18n key for known rungs including flux, legacy cloud, paint, and clone', () => {
    expect(rungLabel('fill')).toBe('ladder.rung.fill')
    expect(rungLabel('lama')).toBe('ladder.rung.lama')
    expect(rungLabel('flux')).toBe('ladder.rung.flux')
    expect(rungLabel('cloud')).toBe('ladder.rung.cloud')
    expect(rungLabel('paint')).toBe('ladder.rung.paint')
    expect(rungLabel('clone')).toBe('ladder.rung.clone')
  })

  it('falls back to unknown for an unrecognised rung', () => {
    expect(rungLabel('unknown_engine')).toBe('ladder.rung.unknown')
  })
})
