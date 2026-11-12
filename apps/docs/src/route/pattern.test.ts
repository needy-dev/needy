// The cases of crates/core/src/pattern_test.rs, so the port answers as needy does.
// Source: https://github.com/needy-dev/needy/blob/1efa6c8d1ab6b2bde0db2a8cf808167758601aa7/crates/core/src/pattern_test.rs

import { describe, expect, it } from 'vitest'

import { basename, compile, matches } from './pattern'

const argv = (s: string) => s.split(/\s+/).filter(Boolean)
const match = (pattern: string, command: string) => matches(compile(pattern), argv(command))

describe('pattern', () => {
  it('matches words like the command', () => {
    expect(match('git **', 'git')).toBeDefined()
    expect(match('git **', '/usr/bin/git status -s')).toBeDefined()
    expect(match('git commit *', 'git commit -a')).toBeDefined()
    expect(match('git commit *', 'git commit -a -m')).toBeUndefined()
    expect(match('pnpm dlx prettier **', 'pnpm dlx prettier --write x')).toBeDefined()
    expect(match('pnpm dlx prettier **', 'pnpm dlx pi')).toBeUndefined()
    expect(match('**', 'anything at all')).toBeDefined()
    expect(match('*.sh', './build.sh')).toBeDefined()
  })

  it('matches a path against the resolved executable and a word against the basename', () => {
    expect(match('/usr/local/bin/python3 **', '/usr/local/bin/python3 -V')).toBeDefined()
    expect(match('/usr/local/bin/python3 **', 'python3 -V')).toBeUndefined()
    expect(match('curl *', '/usr/bin/curl https://x/y')).toBeDefined()
  })

  it('takes alternatives, captures and the rest', () => {
    const m = match('{node,bun} {script} **', 'bun /opt/pi/dist/cli.js --x')!
    expect(m.captures.script).toBe('/opt/pi/dist/cli.js')
    expect(m.rest).toEqual(argv('--x'))
    expect(match('{git,hg} push **', 'git push origin main')).toBeDefined()
    expect(match('{git,hg} push **', 'svn push')).toBeUndefined()
  })

  it('takes the last path component as the basename', () => {
    expect(basename('/usr/bin/git')).toBe('git')
    expect(basename('git')).toBe('git')
    expect(basename('./build.sh')).toBe('build.sh')
  })
})
