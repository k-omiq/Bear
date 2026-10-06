import { describe, expect, it } from 'vitest'
import { parseModalTokenCommand } from './modal-token-command.js'

describe('Modal token command import', () => {
  it('reads the command Modal shows without needing to activate a CLI profile', () => {
    expect(
      parseModalTokenCommand('modal token set --token-id ak-example123 --token-secret as-example456 --profile=k-omiq'),
    ).toEqual({ tokenId: 'ak-example123', tokenSecret: 'as-example456', profile: 'k-omiq' })
    expect(
      parseModalTokenCommand("modal token set --profile 'k-omiq' --token-secret 'as-example456' --token-id 'ak-example123'"),
    ).toEqual({ tokenId: 'ak-example123', tokenSecret: 'as-example456', profile: 'k-omiq' })
  })

  it('rejects incomplete commands, proxy tokens, duplicates, and shell additions', () => {
    for (const command of [
      'modal token set --token-id ak-example123 --token-secret as-example456',
      'modal token set --token-id wk-example123 --token-secret ws-example456 --profile=k-omiq',
      'modal token set --token-id ak-one --token-id ak-two --token-secret as-secret --profile=k-omiq',
      'modal token set --token-id ak-example123 --token-secret as-example456 --profile=k-omiq; echo bad',
      'modal token set --token-id ak-example123 --token-secret as-example456 --profile="k-omiq',
    ]) {
      expect(parseModalTokenCommand(command)).toBeNull()
    }
  })
})
