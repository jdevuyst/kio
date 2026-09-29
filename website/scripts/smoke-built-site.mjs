#!/usr/bin/env node
import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'

const websiteDir = path.resolve(new URL('..', import.meta.url).pathname)
const repoRoot = path.resolve(websiteDir, '..')
const distDir = path.join(websiteDir, '.vitepress', 'dist')
const siteBase = '/kio/'

function fail(message) {
  console.error(`website-smoke: ${message}`)
  process.exit(1)
}

function read(file) {
  return fs.readFileSync(file, 'utf8')
}

function assertFile(relative) {
  const file = path.join(distDir, relative)
  if (!fs.existsSync(file) || fs.statSync(file).size === 0) {
    fail(`missing or empty dist artifact: ${relative}`)
  }
  return file
}

function walkHtml(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name)
    if (entry.isDirectory()) {
      walkHtml(full, out)
    } else if (entry.name.endsWith('.html')) {
      out.push(full)
    }
  }
  return out
}

function withoutQueryOrHash(href) {
  const query = href.indexOf('?')
  const hash = href.indexOf('#')
  const cut = [query, hash].filter((index) => index !== -1).sort((left, right) => left - right)[0]
  return cut === undefined ? href : href.slice(0, cut)
}

function safeDecode(value) {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}

function localHrefCandidates(fromFile, href) {
  if (/^(?:https?:|mailto:|tel:|javascript:)/i.test(href) || href.startsWith('#')) {
    return []
  }

  let raw = safeDecode(withoutQueryOrHash(href))
  if (raw === '') return []

  if (raw.startsWith(siteBase)) {
    raw = raw.slice(siteBase.length)
  } else if (raw.startsWith('/')) {
    return [raw.slice(1)]
  } else {
    raw = path.posix.join(path.posix.dirname(fromFile), raw)
  }

  const directoryLike = raw === '' || raw.endsWith('/')
  const normalized = path.posix.normalize(raw)
  if (normalized === '.') return ['index.html']
  if (directoryLike) return [path.posix.join(normalized, 'index.html')]
  if (path.posix.extname(normalized)) return [normalized]
  return [`${normalized}.html`, path.posix.join(normalized, 'index.html')]
}

function checkLocalHref(file, href) {
  const fromFile = path.relative(distDir, file).split(path.sep).join(path.posix.sep)
  const candidates = localHrefCandidates(fromFile, href)
  if (candidates.length === 0) return
  if (candidates.some((candidate) => fs.existsSync(path.join(distDir, candidate)))) return
  fail(`${fromFile} links to missing local target ${href} (tried ${candidates.join(', ')})`)
}

// A Kiodoc fence attribute ({ignore}, {variant=...}) must be consumed at
// render time, never shown to the reader. The match is scoped to the first
// text of each rendered code block — a leaked attribute leads the block —
// so prose and inline-code mentions of the attributes (the Kiodoc guide
// teaches them) and fence info-strings shown inside example blocks cannot
// false-positive.
const fenceAttrLeak = /^\{(?:ignore|variant=[a-z_]+)(?:[ \t}]|$)/

function leakedFenceAttribute(html) {
  for (const match of html.matchAll(/<pre[^>]*>(?:\s*<code[^>]*>)?/g)) {
    const from = match.index + match[0].length
    const start = html
      .slice(from, from + 400)
      .replace(/<[^>]+>/g, '')
      .replace(/^\s+/, '')
    if (fenceAttrLeak.test(start)) return start.slice(0, 40)
  }
  return null
}

const index = read(assertFile('index.html'))
const hostLanguageCount = fs.readdirSync(path.join(repoRoot, 'specs', 'backends'))
  .filter((name) => name.endsWith('.md') && name !== 'README.md')
  .length
if (!new RegExp(`${hostLanguageCount}\\s+host languages`, 'i').test(index)) {
  fail('landing page does not render the supported host-language count')
}

assertFile('docs/index.html')
assertFile('docs/hosts/swift.html')
assertFile('kio-favicon.png')
assertFile('kio-logo-home.webp')
assertFile('kio-nav-icon.png')

const blogIndex = read(assertFile('blog/index.html'))
if (!/written by hand/i.test(blogIndex)) {
  fail('blog index does not render the hand-written disclosure')
}
if (!blogIndex.includes('feed.xml')) {
  fail('blog index does not link to the RSS feed')
}
const feed = read(assertFile('blog/feed.xml'))
if (!/<rss\b/.test(feed) || !feed.includes('<item>')) {
  fail('blog RSS feed is missing its channel or items')
}
if (!feed.includes('https://jdevuyst.github.io/kio/blog/')) {
  fail('blog RSS feed does not use absolute canonical links')
}
if (!index.includes('application/rss+xml')) {
  fail('site head is missing the RSS feed discovery link')
}
const guide = read(assertFile('docs/guides/sums.html'))
if (!guide.includes('language-kio') || !guide.includes('kio-keyword')) {
  fail('representative docs page does not contain Kio token spans')
}
const kiodocGuide = read(assertFile('docs/guides/kiodoc.html'))
if (!kiodocGuide.includes('Call [`render`] after [`parse`].')) {
  fail('Kiodoc fenced references did not render with literal brackets')
}

const wasm = assertFile('wasm/kio_repl_wasm_bg.wasm')
const magic = fs.readFileSync(wasm).subarray(0, 4)
if (!(magic[0] === 0x00 && magic[1] === 0x61 && magic[2] === 0x73 && magic[3] === 0x6d)) {
  fail('wasm bundle does not start with wasm magic bytes')
}
assertFile('wasm/kio_repl_wasm.js')

const htmlFiles = walkHtml(distDir)
const leakPatterns = [
  [/href="\.\.\/(?:\.\.\/)*(?:specs|test-data)\//, 'unrewritten repo-relative link', false],
  [/\[`[^`\]]+`\]/, 'unresolved Kiodoc intra-doc reference', true],
  [/<p>\s*&lt;!--/, 'rendered HTML comment', false]
]

for (const file of htmlFiles) {
  const html = read(file)
  const proseHtml = html
    .replace(/<pre\b[^>]*>[\s\S]*?<\/pre>/gi, '')
    .replace(/<!--[\s\S]*?-->/g, '')
  for (const [pattern, label, proseOnly] of leakPatterns) {
    if (pattern.test(proseOnly ? proseHtml : html)) {
      fail(`${path.relative(distDir, file)} contains ${label}`)
    }
  }
  const leakedAttr = leakedFenceAttribute(html)
  if (leakedAttr !== null) {
    fail(`${path.relative(distDir, file)} renders a code block leaking a Kiodoc fence attribute: ${leakedAttr}`)
  }
  for (const match of html.matchAll(/\shref="([^"]+)"/g)) {
    checkLocalHref(file, match[1])
  }
}

console.log(`website-smoke: checked ${htmlFiles.length} HTML page(s)`)
