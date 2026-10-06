/**
 * Read the command Modal offers when it creates an API token. This is data,
 * never a command to execute: the helper receives the two credentials on
 * stdin and resolves their workspace directly with the Modal SDK.
 *
 * Only Modal's `token set` spelling and its three expected flags are accepted.
 * A malformed or extended shell command must stay in the field for correction.
 *
 * @param {string} input
 * @returns {{tokenId: string, tokenSecret: string, profile: string}|null}
 */
export function parseModalTokenCommand(input) {
  if (typeof input !== 'string' || input.length > 1024 || /[\r\n;|&`$<>\\]/.test(input)) return null
  /** @type {string[]} */
  const args = []
  let offset = 0
  while (offset < input.length) {
    while (/\s/.test(input[offset] ?? '') && offset < input.length) offset += 1
    if (offset >= input.length) break
    const quote = input[offset] === '"' || input[offset] === "'" ? input[offset++] : null
    const start = offset
    if (quote) {
      while (offset < input.length && input[offset] !== quote) offset += 1
      if (offset >= input.length) return null
      args.push(input.slice(start, offset++))
      if (offset < input.length && !/\s/.test(input[offset])) return null
    } else {
      while (offset < input.length && !/\s/.test(input[offset])) {
        if (input[offset] === '"' || input[offset] === "'") return null
        offset += 1
      }
      args.push(input.slice(start, offset))
    }
  }
  if (args.length < 6 || args.length > 9) return null
  if (args[0] !== 'modal' || args[1] !== 'token' || args[2] !== 'set') return null

  /** @type {Record<string, string>} */
  const values = {}
  for (let index = 3; index < args.length; index += 1) {
    const word = args[index]
    const equals = word.indexOf('=')
    const flag = equals < 0 ? word : word.slice(0, equals)
    if (!['--token-id', '--token-secret', '--profile'].includes(flag) || flag in values) return null
    const value = equals < 0 ? args[++index] : word.slice(equals + 1)
    if (!value || value.startsWith('--')) return null
    values[flag] = value
  }

  const tokenId = values['--token-id']
  const tokenSecret = values['--token-secret']
  const profile = values['--profile']
  if (!/^ak-[A-Za-z0-9_-]{1,253}$/.test(tokenId ?? '')) return null
  if (!/^as-[A-Za-z0-9_-]{1,253}$/.test(tokenSecret ?? '')) return null
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(profile ?? '')) return null
  return { tokenId, tokenSecret, profile }
}
