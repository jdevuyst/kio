import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { build } from 'vitepress'
import { generateHistoricalDocs, historyRoute } from '../historical-docs.mjs'
import { historyFixture } from './history.mjs'
import { generateKiodocHtml } from '../prepare-docs.mjs'

const websiteRoot = fileURLToPath(new URL('../..', import.meta.url))

export function assertHistoryFixture(fixture) {
  const read = route => fs.readFileSync(path.join(fixture.dist, route.replace(/^\//, '')), 'utf8')
  const historical = read(historyRoute(fixture.first, 'docs/guides/changed.md'))
  const current = read('/docs/guides/changed.html')
  assert.match(historical, /Historical documentation text/)
  assert.match(historical, /You’re reading documentation for Kio 0.1.0. This page has changed since then\./)
  assert.match(historical, /href="\/kio\/docs\/guides\/changed.html"[^>]*>View current page/)
  assert.match(historical, /Docs at /)
  assert.doesNotMatch(historical, /Current changed article/)
  assert.doesNotMatch(historical, /<script>globalThis.historyScriptExecuted/)
  assert.match(current, /Current documentation text/)
  assert.doesNotMatch(current, /history-banner|history-version-tabs|history-provenance|historical-doc/)
  assert.match(current, /Current changed article/)
  assert.match(current, /href="\.\/related.html#related-heading"/)
  assert.match(current, /href="https:\/\/github.com\/jdevuyst\/kio\/blob\/main\/docs\/guides\/changed.md"[^>]*>View source on GitHub/)
  assert.match(read(historyRoute(fixture.first, 'docs/guides/equal.md')), /You’re reading documentation for Kio 0.1.0. This page matches the current documentation\./)
  const missing = read(historyRoute(fixture.first, 'docs/guides/deleted.md'))
  assert.match(missing, /You’re reading documentation for Kio 0.1.0. This page has no counterpart in the current documentation\./)
  assert.match(missing, /href="\/kio\/docs\/"[^>]*>Browse current docs/)
  assert.doesNotMatch(missing, /View current page|history-version-tabs/)
  assert.equal(fs.existsSync(path.join(fixture.dist, 'history', fixture.first, 'current')), false)
  assert.doesNotMatch(historical, /history-version-tabs/)
  assert.doesNotMatch(historical, /Tagged Kio|Documentation at |2020-01-01|history-context/)
  assert.match(historical, new RegExp(`href="https://github.com/jdevuyst/kio/blob/${fixture.first}/docs/guides/changed.md"[^>]*>View source on GitHub</a></footer>`))
  assert.doesNotMatch(historical, /Source documentation/)
  assert.match(historical, new RegExp(`https://github.com/jdevuyst/kio/blob/${fixture.first}/specs/language.md`))
  assert.doesNotMatch(historical, /specs\/cli.md|specs\/package.md|specs\/backends/)
  assert.match(historical, /<footer class="source-footer"/)
  const untagged = read(historyRoute(fixture.between, 'docs/guides/related.md'))
  assert.match(untagged, new RegExp(`You’re reading documentation at commit ${fixture.between.slice(0, 8)}\\.`))
  assert.doesNotMatch(untagged, /Tagged Kio|documentation for Kio/)
  assert.match(read('blog/fixture.html'), new RegExp(`/kio/history/${fixture.first}/docs/guides/changed.html#stable-anchor`))
  assert.match(read('blog/fixture.html'), /href="https:\/\/github.com\/jdevuyst\/kio\/blob\/main\/docs\/blog\/fixture.md"[^>]*>View source on GitHub/)
  assert.doesNotMatch(read('docs/guides/index.html'), /source-footer|View source on GitHub/)
  assert.doesNotMatch(read('blog/index.html'), /source-footer|View source on GitHub/)
  const searchDir = path.join(fixture.dist, 'assets/chunks')
  const index = fs.readdirSync(searchDir).filter(name => name.includes('localSearchIndex'))
  assert.ok(index.length > 0)
  for (const name of index) {
    const indexed = fs.readFileSync(path.join(searchDir, name), 'utf8')
    assert.doesNotMatch(indexed, /Historical-only body|Original neighboring article/)
  }
}

export async function buildHistoryFixture() {
  const fixture = historyFixture()
  fixture.blogPages.set('blog/fixture.html', fixture.blogPages.get('blog/fixture.html')
    + `<a href="https://github.com/jdevuyst/kio/blob/${fixture.between}/docs/guides/related.md">untagged revision</a>`)
  const websiteDir = fs.mkdtempSync(path.join(os.tmpdir(), 'website-history-site.'))
  const cleanup = () => {
    fixture.cleanup()
    fs.rmSync(websiteDir, { recursive: true, force: true })
  }
  try {
    const write = (relative, contents) => {
      const file = path.join(websiteDir, relative)
      fs.mkdirSync(path.dirname(file), { recursive: true })
      fs.writeFileSync(file, contents)
    }
    const copy = relative => {
      const target = path.join(websiteDir, relative)
      fs.mkdirSync(path.dirname(target), { recursive: true })
      fs.cpSync(path.join(websiteRoot, relative), target, { recursive: true })
    }
    for (const file of ['.vitepress/config.ts', '.vitepress/site.mjs', '.vitepress/search-content.mjs', '.vitepress/theme']) copy(file)
    for (const file of ['kio-favicon.png', 'kio-nav-icon.png']) copy(`.vitepress/public/${file}`)
    fs.symlinkSync(path.join(websiteRoot, 'node_modules'), path.join(websiteDir, 'node_modules'), process.platform === 'win32' ? 'junction' : 'dir')
    write('index.md', '# Historical documentation integration\n')
    write('package.json', '{"type":"module","private":true}\n')
    write('.vitepress/generated/nav.mjs', 'export const docsSidebar = [{text:"Current documentation",items:[{text:"Current changed article",link:"/docs/guides/changed"}]}]\n')
    write('.vitepress/generated/languages.mjs', 'export const supportedLanguages = []; export const languageCount = 0;\n')
    write('.vitepress/generated/helloExample.mjs', 'export const helloExampleFiles = [];\n')
    write('.vitepress/public/docs/assets/diagram.png', fs.readFileSync(path.join(fixture.root, 'docs/assets/diagram.png')))
    const history = await generateHistoricalDocs({ repoRoot: fixture.root, websiteDir, blogPages: fixture.blogPages, currentPages: fixture.currentPages, acquire: false })
    const kiodocHtml = new Map()
    const sources = new Map()
    for (const [key, html] of fixture.currentPages) {
      kiodocHtml.set(key, html)
      sources.set(key, key === 'index' ? 'docs/README.md' : `docs/${key}.md`)
      write(`docs/${key}.md`, `---\ntitle: ${JSON.stringify(key)}\n---\n\n<KiodocPage page=${JSON.stringify(key)} />\n`)
    }
    write('docs/hosts/index.md', '# Current host documentation\n')
    kiodocHtml.set('guides/index', '<h1>Guide catalogue</h1>')
    write('docs/guides/index.md', '<KiodocPage page="guides/index" />\n')
    kiodocHtml.set('blog/fixture', '<div class="kiodoc-page">' + history.rewrittenBlogs.get('blog/fixture.html') + '<footer class="blog-post-footer">Fixture blog disclosure.</footer></div>')
    sources.set('blog/fixture', 'docs/blog/fixture.md')
    kiodocHtml.set('blog/index', '<h1>Fixture blog</h1><a href="./fixture.html">Fixture post</a>')
    write('blog/index.md', '<KiodocPage page="blog/index" />\n')
    write('blog/fixture.md', '---\nsidebar: false\n---\n\n<KiodocPage page="blog/fixture" />\n')
    await generateKiodocHtml({ websiteDir, pages: kiodocHtml, sources })
    await build(websiteDir)
    return { ...fixture, ...history, websiteDir, dist: path.join(websiteDir, '.vitepress/dist'), cleanup }
  } catch (error) {
    cleanup()
    throw error
  }
}
import assert from 'node:assert/strict'
