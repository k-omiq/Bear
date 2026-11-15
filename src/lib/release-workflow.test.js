import { spawnSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

/**
 * The release workflow's publish step builds latest.json from every artifact
 * folder of the run. A re-run of the workflow downloads the step's own
 * `update-manifest` artifact with the rest; that folder has no signature and
 * must be passed over, not stop the release with "no signature".
 *
 * The Python is taken from `release.yml` itself and run on a folder shaped
 * like a re-run's download.
 */

const workflow = readFileSync(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8')

/** The heredoc the publish step runs as `python3 - "$VERSION"`. */
function publishScript() {
  const match = /python3 - "\$VERSION" <<'PY'\n([\s\S]*?)\n\s*PY\n/.exec(workflow)
  if (!match) throw new Error('the publish step heredoc was not found')
  const lines = match[1].split('\n')
  const indent = Math.min(...lines.filter((l) => l.trim()).map((l) => l.match(/^ */)[0].length))
  return lines.map((l) => l.slice(indent)).join('\n')
}

const python = spawnSync('python3', ['--version']).status === 0

describe('the release publish step', () => {
  it.skipIf(!python)('passes over its own update-manifest artifact on a re-run', () => {
    const dir = mkdtempSync(join(tmpdir(), 'release-publish-'))
    try {
      const put = (path, text) => {
        mkdirSync(join(dir, path, '..'), { recursive: true })
        writeFileSync(join(dir, path), text)
      }
      put('artifacts/darwin-aarch64/MangaCleaner_1.0.0_darwin-aarch64.app.tar.gz', 'app')
      put('artifacts/darwin-aarch64/MangaCleaner_1.0.0_darwin-aarch64.app.tar.gz.sig', 'sig-mac\n')
      put('artifacts/launch-smoke-ubuntu/report.json', '{}')
      put('artifacts/update-manifest/latest.json', '{"version": "1.0.0"}')
      writeFileSync(join(dir, 'publish.py'), publishScript())

      const run = spawnSync('python3', ['-I', 'publish.py', '1.0.0'], {
        cwd: dir,
        env: { ...process.env, RELEASE_NOTES: 'Notes.' },
        encoding: 'utf8',
      })
      expect(run.stderr).not.toContain('no signature')
      expect(run.status).toBe(0)
      const latest = JSON.parse(readFileSync(join(dir, 'latest.json'), 'utf8'))
      expect(Object.keys(latest.platforms)).toEqual(['darwin-aarch64'])
      expect(latest.platforms['darwin-aarch64'].signature).toBe('sig-mac')
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  it('uploads the manifest artifact with overwrite, so a re-run can upload it again', () => {
    const step = workflow.slice(workflow.indexOf('name: update-manifest'))
    expect(step.slice(0, 300)).toMatch(/overwrite: true/)
  })
})
