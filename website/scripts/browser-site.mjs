import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { firefox } from 'playwright-core'
import { preview } from 'vite'

const repoRoot = fileURLToPath(new URL('../..', import.meta.url))
export const defaultDist = fileURLToPath(new URL('../.vitepress/dist', import.meta.url))
export const siteBase = '/kio/'
const timeout = 90_000

async function artifactIdentity(dist) {
  const hash = createHash('sha256')
  let files = 0
  let bytes = 0
  async function walk(directory, relative = '') {
    const entries = (await fs.readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name, 'en'))
    for (const entry of entries) {
      const name = relative + entry.name
      if (entry.isDirectory()) await walk(path.join(directory, entry.name), `${name}/`)
      else if (entry.isFile()) {
        const contents = await fs.readFile(path.join(directory, entry.name))
        hash.update(`${name}\0${contents.length}\0`).update(contents)
        files += 1
        bytes += contents.length
      } else throw new Error(`dist contains a non-regular artifact: ${name}`)
    }
  }
  await walk(dist)
  assert.ok(files > 0, 'dist is empty; build the website first')
  return { sha256: hash.digest('hex'), files, bytes }
}

export function browserToolchain(platform = process.platform, arch = process.arch) {
  if (platform === 'linux' && ['x64', 'arm64'].includes(arch)) return { tool: 'http:kio-firefox', executable: 'firefox' }
  if (platform === 'darwin' && ['x64', 'arm64'].includes(arch)) return { tool: 'http:kio-firefox', executable: 'Nightly.app/Contents/MacOS/firefox' }
  if (platform === 'win32' && arch === 'x64') return { tool: 'http:kio-firefox', executable: 'firefox.exe' }
  throw new Error(`website browser tasks have no managed browser route for ${platform}-${arch}`)
}

export function isCancelledPrefetch(request) {
  return request.failure()?.errorText === 'NS_BINDING_ABORTED' &&
    request.resourceType() === 'other' && !request.isNavigationRequest() &&
    request.headers()['sec-purpose'] === 'prefetch'
}

function managedBrowser() {
  const toolchain = browserToolchain()
  for (const [name, value] of Object.entries(process.env)) {
    if ((/^MOZ_DISABLE_.*SANDBOX$/.test(name) || name === 'MOZ_FORCE_DISABLE_E10S' || name === 'PLAYWRIGHT_FIREFOX_POLICIES_JSON') && value) {
      throw new Error(`refusing sandbox/policy override ${name}; browser tasks require the normal browser sandbox`)
    }
  }
  try {
    const env = { ...process.env }
    if (env.KIO_GENERAL_MISE_DATA_DIR) env.MISE_DATA_DIR = env.KIO_GENERAL_MISE_DATA_DIR
    const install = execFileSync('mise', ['where', toolchain.tool], {
      cwd: repoRoot, env, encoding: 'utf8', timeout, stdio: ['ignore', 'pipe', 'pipe']
    }).trim()
    return { ...toolchain, executablePath: path.join(install, toolchain.executable) }
  } catch (error) {
    throw new Error(`mise-managed browser is unavailable; run mise install --locked ${toolchain.tool} from the repository root (Linux runtime libraries: sh ci/impl-toolchain.sh install-browser-system-packages)`, { cause: error })
  }
}

// Smoke assertions and change-scoped visual captures share this launch path.
export async function withBuiltSite({ dist = defaultDist, output } = {}, visit) {
  const started = performance.now()
  const toolchain = managedBrowser()
  const artifact = await artifactIdentity(path.resolve(dist))
  const runDir = output ? path.resolve(output) : await fs.mkdtemp(path.join(os.tmpdir(), 'website-browser.'))
  if (output) await fs.mkdir(runDir, { recursive: false })
  const profile = path.join(runDir, 'profile')
  const diagnostics = []
  const failures = []
  const note = message => {
    diagnostics.push(String(message).slice(0, 2_000))
    if (diagnostics.length > 80) diagnostics.shift()
  }
  const report = { artifact, tool: toolchain.tool, executablePath: toolchain.executablePath, platform: `${process.platform}-${process.arch}` }
  let server
  let context
  let page
  let failed = false
  let phase = 'preview setup'
  try {
    server = await preview({
      configFile: false, base: siteBase, build: { outDir: path.resolve(dist) },
      preview: { host: '127.0.0.1', port: 0, open: false }
    })
    const origin = `http://127.0.0.1:${server.httpServer.address().port}`
    phase = 'normal sandboxed browser launch'
    context = await firefox.launchPersistentContext(profile, {
      executablePath: toolchain.executablePath, headless: true, timeout,
      viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce'
    })
    const browser = context.browser()
    report.browser = browser.version()
    report.launchMs = Math.round(performance.now() - started)
    phase = 'browser page setup'
    page = context.pages()[0]
    page.setDefaultTimeout(timeout)
    page.setDefaultNavigationTimeout(timeout)
    page.on('console', message => {
      note(`console ${message.type()}: ${message.text()}`)
      if (message.type() === 'error') failures.push(`console: ${message.text()}`)
    })
    page.on('pageerror', error => { note(`pageerror: ${error.message}`); failures.push(error.message) })
    page.on('requestfailed', request => {
      const message = `request failed: ${request.url()} ${request.failure()?.errorText}`
      note(message)
      // Firefox cancels speculative VitePress prefetches when a document leaves.
      if (isCancelledPrefetch(request)) note(`cancelled speculative prefetch: ${request.url()}`)
      else if (request.url().startsWith(origin)) failures.push(message)
    })
    page.on('response', response => {
      if (response.url().startsWith(origin) && response.status() >= 400) {
        const message = `HTTP ${response.status()}: ${response.url()}`
        note(message)
        failures.push(message)
      }
    })
    phase = 'rendered website checks'
    await visit({ browser, context, page, origin, report, runDir })
    assert.deepEqual(failures, [], 'browser reported website errors')
    report.elapsedMs = Math.round(performance.now() - started)
    report.diagnostics = diagnostics
    if (output) await fs.writeFile(path.join(runDir, 'browser-report.json'), JSON.stringify(report, null, 2) + '\n')
    console.log(`website-browser: Firefox ${report.browser}; dist sha256=${artifact.sha256}; ${artifact.files} files/${artifact.bytes} bytes; ${report.elapsedMs}ms`)
    return report
  } catch (error) {
    failed = true
    report.phase = phase
    report.error = error.stack ?? String(error)
    report.elapsedMs = Math.round(performance.now() - started)
    report.diagnostics = diagnostics
    if (page && !page.isClosed()) {
      try {
        report.page = await page.evaluate(() => ({
          url: location.href, status: document.querySelector('.repl-status')?.textContent,
          fallback: document.querySelector('.repl-fallback')?.textContent,
          terminal: [...document.querySelectorAll('.xterm-rows > div')].map(row => row.textContent)
        }))
        await page.screenshot({ path: path.join(runDir, 'failure.png') })
      } catch (captureError) { report.captureError = String(captureError) }
    }
    await fs.writeFile(path.join(runDir, 'browser-report.json'), JSON.stringify(report, null, 2) + '\n')
    console.error(`website-browser: ${phase} failed; diagnostics retained in ${runDir}\n${diagnostics.join('\n')}`)
    throw error
  } finally {
    try { await context?.close() } finally {
      try {
        if (server) await new Promise((resolve, reject) => server.httpServer.close(error => error ? reject(error) : resolve()))
      } finally {
        await fs.rm(profile, { recursive: true, force: true })
        if (!output && !failed) await fs.rm(runDir, { recursive: true, force: true })
      }
    }
  }
}

export async function openBuiltPage(page, url) {
  const response = await page.goto(url, { waitUntil: 'domcontentloaded' })
  assert.ok(response?.ok() || response?.status() === 304 || (response === null && page.url() === url),
    `page navigation failed (${response?.status() ?? 'no response'}): ${url}`)
  // SSR contains controls before Vue installs their handlers.
  await page.waitForFunction(() => Boolean(document.querySelector('#app')?.__vue_app__))
}

export async function smokeRepl(page, origin) {
  await openBuiltPage(page, origin + siteBase)
  // Scrolling near the lazy REPL replaces its launch button; this link stays mounted.
  await page.locator('a.secondary-link[href="#repl"]').click()
  await page.waitForFunction(() => document.querySelector('.repl-fallback') || document.querySelector('.repl-status[data-state="ready"]'))
  assert.equal(await page.locator('.repl-fallback').count(), 0, 'worker/WASM startup showed the REPL fallback')
  await page.waitForFunction(() => [...document.querySelectorAll('.xterm-rows > div')].some(row => row.textContent.trim().startsWith('kio>')))
  const initialInput = await page.evaluate(() => [...document.querySelectorAll('.xterm-rows > div')].map(row => row.textContent.trim()).findLast(row => row.startsWith('kio>')).slice(4).trimStart())
  await page.locator('.xterm-helper-textarea').focus()
  for (const _ of initialInput) await page.keyboard.press('Backspace')
  await page.keyboard.type(':normalize ()')
  await page.keyboard.press('Enter')
  await page.waitForFunction(() => {
    const rows = [...document.querySelectorAll('.xterm-rows > div')].map(row => row.textContent.trim()).filter(Boolean)
    return rows.includes('kio> :normalize ()') && rows.at(-1) === 'kio>'
  })
  const rows = (await page.locator('.xterm-rows > div').allTextContents()).map(row => row.trim()).filter(Boolean)
  const echo = rows.indexOf('kio> :normalize ()')
  assert.deepEqual(rows.slice(echo + 1), ['()', 'kio>'], 'normalization must render its standalone result and return the prompt')
  assert.equal(await page.locator('.repl-status[data-state="ready"]').count(), 1, 'REPL did not return to ready')
}

export async function capturePage(page, url, directory, { fullPage = false } = {}) {
  for (const [name, viewport] of Object.entries({ desktop: { width: 1440, height: 1000 }, narrow: { width: 390, height: 844 } })) {
    await page.setViewportSize(viewport)
    await openBuiltPage(page, url)
    await page.waitForFunction(() => document.fonts.status === 'loaded')
    await page.waitForFunction(() => [...document.images].every(image => image.complete))
    const broken = await page.evaluate(() => [...document.images].filter(image => !image.naturalWidth).map(image => image.src))
    assert.deepEqual(broken, [], 'page contains broken rendered images')
    await page.screenshot({ path: path.join(directory, `${name}.png`), fullPage })
  }
}
