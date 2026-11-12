// Renders the README banners, in the dark and the light theme, as animated WebP in docs/assets/banners.
//
// Source: https://github.com/moeru-ai/injeca/blob/a361694ff67b4f2b2feb4b4cf0bf585e0f328a68/apps/docs/scripts/banners.ts
// As in the injeca Docs: it builds the page and serves it with vite preview. Each banner is ?capture=<scene> in
// headless Chrome, driven through the DevTools protocol. The page lists its steps in `__captureFrames` (how long each
// one shows), `__captureAt(i)` moves to step i, and the page plays each step in real time while Chrome sends each
// frame that it renders (a screencast). The frames keep the times that Chrome gives them, so the WebP plays at the
// speed of the page. img2webp joins them into a looping WebP.
//
//   pnpm -F @needy/docs banners
//
// BANNERS=hero,routing renders only those scenes. STILLS=1 writes a PNG of the end of each step to .tmp/banners/
// instead of the WebP, to look at a scene step by step. CHROME points to another Chrome binary. img2webp must be on PATH
// (brew install webp).

import process from 'node:process'

import { Buffer } from 'node:buffer'
import { execFile, spawn } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'

import { build, preview } from 'vite'

import { SIZES } from '../src/scenes/sizes.ts'

// Async, so the preview server in this process keeps answering while Chrome loads the page.
const run = promisify(execFile)

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const repo = join(root, '..', '..')
const out = join(repo, 'docs', 'assets', 'banners')
const stills = join(repo, '.tmp', 'banners')
const chrome = process.env.CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
const THEMES = ['dark', 'light'] as const

/** 1.5x: sharp enough on a Retina screen, and small enough for some hundred frames. */
const SCALE = 1.5
/** The most frames a second that a banner keeps. A frame that the page holds longer shows longer. */
const FPS = 15

// A built page, not the dev server: the dev server can reload the page while it optimizes dependencies,
// and headless Chrome then never finishes.
const configFile = join(root, 'vite.config.ts')
await build({ configFile, logLevel: 'error', root })
const server = await preview({ configFile, logLevel: 'error', preview: { host: '127.0.0.1', port: 0 }, root })
const address = server.httpServer.address()
if (!address || typeof address === 'string')
  throw new Error('The Vite server has no port.')
const origin = `http://127.0.0.1:${address.port}`

const work = mkdtempSync(join(tmpdir(), 'needy-banners-'))
mkdirSync(out, { recursive: true })
if (process.env.STILLS)
  mkdirSync(stills, { recursive: true })

/** A minimal client of the Chrome DevTools protocol over the WebSocket that Node has built in. */
class DevTools {
  private next = 0
  private readonly pending = new Map<number, { reject: (error: Error) => void, resolve: (value: any) => void }>()
  private readonly listeners = new Map<string, Set<(params: any) => void>>()
  private readonly socket: WebSocket

  private constructor(socket: WebSocket) {
    this.socket = socket
    socket.addEventListener('message', (event) => {
      const message = JSON.parse(String(event.data)) as { error?: { message: string }, id?: number, method?: string, params?: unknown, result?: unknown }
      if (message.method) {
        for (const listener of this.listeners.get(message.method) ?? [])
          listener(message.params)
        return
      }
      const waiting = message.id === undefined ? undefined : this.pending.get(message.id)
      if (!waiting)
        return
      this.pending.delete(message.id!)
      if (message.error)
        waiting.reject(new Error(message.error.message))
      else
        waiting.resolve(message.result)
    })
  }

  static async connect(url: string): Promise<DevTools> {
    const socket = new WebSocket(url)
    await new Promise((resolve, reject) => {
      socket.addEventListener('open', resolve, { once: true })
      socket.addEventListener('error', reject, { once: true })
    })
    return new DevTools(socket)
  }

  close(): void {
    this.socket.close()
  }

  /** Runs an expression in the page and waits for it when it is a promise. */
  async evaluate<T = unknown>(expression: string): Promise<T> {
    const { exceptionDetails, result } = await this.send('Runtime.evaluate', { awaitPromise: true, expression, returnByValue: true })
    if (exceptionDetails)
      throw new Error(`The page threw: ${JSON.stringify(exceptionDetails)}`)
    return result.value as T
  }

  /** Calls `listener` with the params of each event `method`. */
  on<T = any>(method: string, listener: (params: T) => void): void {
    if (!this.listeners.has(method))
      this.listeners.set(method, new Set())
    this.listeners.get(method)!.add(listener)
  }

  send<T = any>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    const id = ++this.next
    this.socket.send(JSON.stringify({ id, method, params }))
    return new Promise((resolve, reject) => this.pending.set(id, { reject, resolve }))
  }
}

/** Opens ?capture=<scene> in a headless Chrome at the size of the scene, and hands its DevTools to `use`. */
async function withPage(scene: string, theme: string, use: (devtools: DevTools) => Promise<void>): Promise<void> {
  const { height, width } = SIZES[scene]!
  const profile = join(work, `profile-${scene}-${theme}`)
  const browser = spawn(chrome, [
    '--headless=new',
    '--hide-scrollbars',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    `--window-size=${width},${height}`,
    'about:blank',
  ], { stdio: 'ignore' })

  try {
    // Chrome writes the port that it picked into the profile.
    let port: string | undefined
    for (let i = 0; i < 100 && !port; i++) {
      await sleep(100)
      try {
        port = readFileSync(join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]
      }
      catch {}
    }
    if (!port)
      throw new Error('Chrome did not open its DevTools port.')

    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json() as { type: string, webSocketDebuggerUrl: string }[]
    const page = targets.find(t => t.type === 'page')
    if (!page)
      throw new Error('Chrome has no page.')
    const devtools = await DevTools.connect(page.webSocketDebuggerUrl)

    await devtools.send('Emulation.setDeviceMetricsOverride', { deviceScaleFactor: SCALE, height, mobile: false, width })
    // Transparent around the rounded corners of the banner.
    await devtools.send('Emulation.setDefaultBackgroundColorOverride', { color: { a: 0, b: 0, g: 0, r: 0 } })
    await devtools.send('Page.enable')
    await devtools.send('Page.navigate', { url: `${origin}/?capture=${scene}&theme=${theme}` })
    // The page is ready once the fonts arrived and the scene listed its steps.
    await devtools.evaluate(`new Promise(async (resolve) => {
      while (!window.__ready || !window.__captureFrames) await new Promise(r => setTimeout(r, 50))
      await new Promise(r => setTimeout(r, 300))
      resolve(true)
    })`)
    await use(devtools)
    devtools.close()
  }
  finally {
    browser.kill()
  }
}

/** One scene: each step, and the transitions between steps, as Chrome renders them. */
async function animated(scene: string, theme: string): Promise<void> {
  await withPage(scene, theme, async (devtools) => {
    const steps = await devtools.evaluate<number[]>('window.__captureFrames')
    const { height, width } = SIZES[scene]!

    const frames: { file: string, time: number }[] = []
    let writing = Promise.resolve()
    devtools.on<{ data: string, metadata: { timestamp: number }, sessionId: number }>('Page.screencastFrame', ({ data, metadata, sessionId }) => {
      void devtools.send('Page.screencastFrameAck', { sessionId })
      const file = join(work, `${scene}-${theme}-${String(frames.length).padStart(5, '0')}.png`)
      frames.push({ file, time: metadata.timestamp * 1000 })
      writing = writing.then(() => writeFileSync(file, Buffer.from(data, 'base64')))
    })

    await devtools.evaluate('window.__captureAt(0)')
    await devtools.send('Page.startScreencast', { everyNthFrame: 1, format: 'png', maxHeight: height * SCALE, maxWidth: width * SCALE })
    const begin = Date.now()
    for (const [i, duration] of steps.entries()) {
      if (i > 0)
        await devtools.evaluate(`window.__captureAt(${i})`)
      await sleep(duration - 40)
      if (process.env.STILLS) {
        const { data } = await devtools.send<{ data: string }>('Page.captureScreenshot', { format: 'png' })
        writeFileSync(join(stills, `${scene}-${theme}-${i}.png`), Buffer.from(data, 'base64'))
      }
      else {
        await sleep(40)
      }
    }
    await devtools.send('Page.stopScreencast')
    await writing
    const total = Date.now() - begin
    // A screenshot in the middle of a screencast changes the size of the frames that follow, so a run for stills
    // writes no WebP.
    if (process.env.STILLS)
      return

    // Keep at most FPS frames a second. Each kept frame shows until the next one, and the last one until the end.
    const kept = frames.reduce<typeof frames>((list, frame) => (list.length === 0 || frame.time - list.at(-1)!.time >= 1000 / FPS ? [...list, frame] : list), [])
    const args: string[] = ['-loop', '0', '-lossless', '-q', '75', '-m', '4']
    for (const [i, frame] of kept.entries()) {
      const next = kept[i + 1]?.time ?? kept[0]!.time + total
      args.push('-d', String(Math.max(1, Math.round(next - frame.time))), frame.file)
    }

    const webp = join(out, `${scene}-${theme}.webp`)
    await run('img2webp', [...args, '-o', webp], { maxBuffer: 1 << 26 })
    console.info(`${webp} (${kept.length} frames)`)
  })
}

try {
  const only = process.env.BANNERS?.split(',')
  for (const theme of THEMES) {
    for (const scene of Object.keys(SIZES).filter(scene => !only || only.includes(scene)))
      await animated(scene, theme)
  }
}
finally {
  await server.close()
  // Chrome may still be writing its profile as it exits, so the removal tries again for a moment.
  rmSync(work, { force: true, maxRetries: 10, recursive: true, retryDelay: 200 })
}
