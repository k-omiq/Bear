// Minimal Chrome DevTools Protocol client over the global WebSocket (Node 22+).
import { spawn } from 'node:child_process'

export const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'

export function launchChrome(port, profile, { detached = false } = {}) {
  const child = spawn(CHROME, [
    '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
    '--window-size=1440,900', '--hide-scrollbars', '--no-first-run', '--no-default-browser-check',
    '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows',
    '--force-color-profile=srgb', '--font-render-hinting=none', 'about:blank',
  ], { stdio: 'ignore', detached })
  if (detached) child.unref()
  return child
}

export async function connect(port) {
  let url
  for (let i = 0; i < 300 && !url; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()
      url = list.find((t) => t.type === 'page')?.webSocketDebuggerUrl
    } catch {}
    if (!url) await new Promise((r) => setTimeout(r, 100))
  }
  if (!url) throw new Error('Chrome did not start')
  const ws = new WebSocket(url)
  await new Promise((r) => ws.addEventListener('open', r, { once: true }))
  let id = 0
  const pending = new Map()
  const listeners = new Map()
  ws.addEventListener('message', (e) => {
    const m = JSON.parse(e.data)
    if (m.id && pending.has(m.id)) {
      const { res, rej } = pending.get(m.id)
      pending.delete(m.id)
      m.error ? rej(new Error(`${m.error.message} ${m.error.data ?? ''}`)) : res(m.result)
    } else if (m.method) {
      for (const fn of listeners.get(m.method) ?? []) fn(m.params)
    }
  })
  const send = (method, params = {}) =>
    new Promise((res, rej) => {
      const i = ++id
      pending.set(i, { res, rej })
      ws.send(JSON.stringify({ id: i, method, params }))
    })
  const on = (method, fn) => {
    if (!listeners.has(method)) listeners.set(method, new Set())
    listeners.get(method).add(fn)
    return () => listeners.get(method).delete(fn)
  }
  const evaluate = async (expr) => {
    const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true })
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
    return r.result.value
  }
  return { ws, send, on, evaluate, close: () => ws.close() }
}
