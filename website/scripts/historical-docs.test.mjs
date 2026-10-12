import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { generateHistoricalDocs, historicalRenderer, historyRoute, HistorySource, pinnedDoc, releaseTagCommits } from './historical-docs.mjs'
import { historyFixture } from './fixtures/history.mjs'

const websiteRoot = fileURLToPath(new URL('..', import.meta.url))

function output(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'website-history-output.'))
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
  return dir
}

async function generate(t) {
  const fixture = historyFixture()
  t.after(fixture.cleanup)
  const websiteDir = output(t)
  const result = await generateHistoricalDocs({ ...fixture, repoRoot: fixture.root, websiteDir, acquire: false })
  return { ...fixture, ...result, websiteDir }
}

function body(websiteDir, route) {
  const file = path.join(websiteDir, route.slice(1).replace(/\.html$/, '.md'))
  const markdown = fs.readFileSync(file, 'utf8')
  const target = JSON.parse(markdown.match(/import articleHtml from (".*")/)[1])
  return { markdown, html: JSON.parse(fs.readFileSync(path.resolve(path.dirname(file), target), 'utf8')) }
}

test('only this repository and full commit-pinned documentation articles become historical routes', () => {
  const revision = 'a'.repeat(40)
  for (const kind of ['blob', 'tree']) assert.deepEqual(pinnedDoc(`https://github.com/jdevuyst/kio/${kind}/${revision}/docs/guides/guide.md#part`), { revision, source: 'docs/guides/guide.md', hash: '#part' })
  for (const url of [`https://github.com/jdevuyst/kio/blob/main/docs/example.md`, `https://github.com/another/kio/blob/${revision}/docs/example.md`, `https://github.com/jdevuyst/kio/blob/${revision}/specs/language.md`, `https://github.com/jdevuyst/kio/tree/${revision}/docs/`, `https://github.com/jdevuyst/kio/blob/${revision}/docs/%2e%2e/specs/language.md`]) assert.equal(pinnedDoc(url), null)
  assert.throws(() => historyRoute(revision, '../escape.md'))
})

test('exact remote tag metadata uses peeled commits, not nearest or annotated tag object IDs', () => {
  const commit = 'a'.repeat(40)
  const tagObject = 'b'.repeat(40)
  const unrelated = 'c'.repeat(40)
  const tags = releaseTagCommits(`${commit}\trefs/tags/releases/v0.1.0\n${tagObject}\trefs/tags/releases/v0.2.0\n${commit}\trefs/tags/releases/v0.2.0^{}\n${unrelated}\trefs/tags/releases/v0.3.0\n${commit}\trefs/tags/unrelated\n${commit}\trefs/tags/releases/v0.4.0^{}\n`)
  assert.deepEqual([...tags], [['releases/v0.1.0', commit], ['releases/v0.2.0', commit], ['releases/v0.3.0', unrelated]])
  assert.deepEqual([...releaseTagCommits('')], [])
})

test('actual different commits retain historical content, link closure, current counterpart and content notices', async t => {
  const result = await generate(t)
  const { first, second, websiteDir, pages, rewrittenBlogs } = result
  assert.notEqual(first, second)
  assert.match(rewrittenBlogs.get('blog/fixture.html'), new RegExp(`/kio/history/${first}/docs/guides/changed.html#stable-anchor`))
  assert.match(rewrittenBlogs.get('blog/fixture.html'), new RegExp(`https://github.com/jdevuyst/kio/blob/${first}/specs/language.md`))
  const old = pages.get(`${first}/docs/guides/changed.md`)
  assert.equal(old.differs, true)
  assert.equal(old.currentRoute, '/docs/guides/changed.html')
  assert.deepEqual(old.releases.map(release => [release.version, release.tag]), [['0.1.0', 'releases/v0.1.0']])
  assert.equal(old.releases[0].url, 'https://github.com/jdevuyst/kio/tree/releases%2Fv0.1.0')
  assert.deepEqual(pages.get(`${second}/docs/guides/related.md`).releases.map(release => release.version), ['0.1.1'])
  assert.equal(pages.get(`${first}/docs/guides/equal.md`).differs, false)
  assert.equal(pages.get(`${first}/docs/guides/image-only.md`).differs, true)
  assert.equal(pages.get(`${first}/docs/guides/code-space.md`).differs, true)
  assert.equal(pages.get(`${first}/docs/guides/deleted.md`).currentAvailable, false)
  assert.equal(pages.get(`${first}/docs/guides/deleted.md`).differs, false)
  assert.equal(pages.get(`${first}/docs/guides/deleted.md`).currentRoute, null)
  const historical = body(websiteDir, old.historicalRoute)
  assert.match(historical.html, /Historical documentation text/)
  assert.match(historical.html, /id="stable-anchor"/)
  assert.match(historical.html, new RegExp(`/kio/history/${first}/docs/guides/related.html#related-heading`))
  assert.match(historical.html, new RegExp(`/tree/${first}/docs/hosts/`))
  assert.match(historical.html, new RegExp(`/blob/${first}/specs/language.md#types`))
  assert.match(body(websiteDir, historyRoute(first, 'docs/guides/related.md')).html, /Original neighboring/)
  assert.match(body(websiteDir, historyRoute(second, 'docs/guides/related.md')).html, /Second neighboring/)
  assert.match(historical.markdown, /search: false/)
  assert.doesNotMatch(historical.markdown, /version=/)
  assert.equal(fs.existsSync(path.join(websiteDir, 'history', first, 'current')), false)
  assert.equal(pages.size, 9)
  const nav = await import(pathToFileURL(path.join(websiteDir, '.vitepress/generated/history-nav.mjs')))
  assert.equal(nav.historicalSidebars[`/history/${second}/docs/`][0].items.length, 1)
  assert.ok(nav.historicalSidebars[`/history/${first}/docs/`][0].items.every(item => item.link.startsWith(`/history/${first}/docs/`)))
  assert.deepEqual(nav.historicalSidebars[`/history/${first}/docs/`][1], {
    text: 'Reference', items: [{ text: 'Language spec', link: `https://github.com/jdevuyst/kio/blob/${first}/specs/language.md` }]
  })
  assert.doesNotMatch(JSON.stringify(nav.historicalSidebars), /Source documentation|\/main\//)
})

test('historical blog counterparts link to the ordinary current blog route', async t => {
  const fixture = historyFixture()
  t.after(fixture.cleanup)
  fixture.write('docs/blog/article.md', '# Blog article\n\nArticle body.\n')
  fixture.git('add', '.')
  fixture.git('commit', '--quiet', '-m', 'Blog counterpart')
  const revision = fixture.git('rev-parse', 'HEAD')
  const websiteDir = output(t)
  const blogPages = new Map([['blog/fixture.html', `<a href="https://github.com/jdevuyst/kio/blob/${revision}/docs/blog/article.md">article</a>`]])
  const currentPages = new Map([['blog/article', '<h1>Blog article</h1><p>Article body.</p>']])
  const { pages } = await generateHistoricalDocs({ repoRoot: fixture.root, websiteDir, blogPages, currentPages, acquire: false })
  assert.equal(pages.get(`${revision}/docs/blog/article.md`).currentRoute, '/blog/article.html')
})

test('historical assets remain pinned and raw HTML, SVG, includes, frontmatter and interpolation remain inert', async t => {
  const { websiteDir, first } = await generate(t)
  const { html } = body(websiteDir, historyRoute(first, 'docs/guides/changed.md'))
  assert.match(html, /src="\/kio\/history-assets\/[a-f0-9]{64}.png"/)
  assert.equal(fs.readdirSync(path.join(websiteDir, '.vitepress/public/history-assets')).length, 1)
  assert.match(html, /src="data:image\/svg\+xml;base64,/)
  assert.doesNotMatch(html, /<script|<iframe|href="javascript:|THIS_INCLUDE_MUST_NOT_APPEAR/)
  assert.match(html, /&lt;script&gt;/)
  assert.match(html, /&lt;&lt;&lt; ..\/..\/secret.txt/)
  assert.match(html, /\{\{ globalThis.historyInterpolationExecuted = true \}\}/)
  assert.match(html, /old_spelling\(1,  2\)/)
  const frontmatter = body(websiteDir, historyRoute(first, 'docs/guides/frontmatter.md')).html
  assert.doesNotMatch(frontmatter, /historyFrontmatterExecuted/)
  assert.equal(globalThis.historyFrontmatterExecuted, undefined)
  const equal = body(websiteDir, historyRoute(first, 'docs/guides/equal.md')).html
  assert.doesNotMatch(equal, /hidden_old|check_exit_code|harness=|@old/)
  assert.match(equal, /old_spelling/)
})

test('literal examples preserve comments and code whitespace; harmless prose reflow compares equally', async () => {
  const renderer = await historicalRenderer(websiteRoot)
  const html = renderer.render(renderer.tokens('# Example\n\n```text\n<!-- visible code comment -->\na  b\n```\n'))
  assert.match(html, /&lt;!-- visible code comment --&gt;/)
  assert.match(html, /a  b/)
})

test('comparison uses deployed rendered content and ignores only presentation wrappers', async t => {
  const fixture = historyFixture()
  t.after(fixture.cleanup)
  const websiteDir = output(t)
  fixture.currentPages.set('guides/equal', '<div class="kiodoc-page theme-changed"><h1 id="equal-article">Equal article</h1><p>One  ordinary\nsentence.</p><pre><code class="language-kio"><span class="kio-fn">old_spelling</span>()\n</code></pre></div>')
  const input = { ...fixture, repoRoot: fixture.root, websiteDir, acquire: false }
  const decorated = await generateHistoricalDocs(input)
  assert.equal(decorated.pages.get(`${fixture.first}/docs/guides/equal.md`).differs, false)
  fixture.currentPages.set('guides/equal', fixture.currentPages.get('guides/equal').replace('old_spelling', 'expanded_signature'))
  const expanded = await generateHistoricalDocs(input)
  assert.equal(expanded.pages.get(`${fixture.first}/docs/guides/equal.md`).differs, true, 'unchanged source with different generated reader content needs a notice')
  fixture.write('docs/guides/inline.md', '# Inline article\n\nOne `code` word with [related](related.md#related-heading) and [spec](../../specs/language.md#types).\n')
  fixture.git('add', '.')
  fixture.git('commit', '--quiet', '-m', 'Inline spaces and link targets')
  const revision = fixture.git('rev-parse', 'HEAD')
  const blogPages = new Map([['blog/inline.html', `<a href="https://github.com/jdevuyst/kio/blob/${revision}/docs/guides/inline.md">inline</a>`]])
  const rendered = '<h1>Inline article</h1><p>One <code>code</code> word with <a href="./related.html#related-heading">related</a> and <a href="https://github.com/jdevuyst/kio/blob/main/specs/language.md#types">spec</a>.</p>'
  const currentPages = new Map([...fixture.currentPages, ['guides/inline', rendered]])
  const inlineInput = { ...input, blogPages, currentPages }
  const equal = await generateHistoricalDocs(inlineInput)
  assert.equal(equal.pages.get(`${revision}/docs/guides/inline.md`).differs, false)
  currentPages.set('guides/inline', rendered.replace('One <code>code</code> word', 'One<code>code</code>word'))
  const joined = await generateHistoricalDocs(inlineInput)
  assert.equal(joined.pages.get(`${revision}/docs/guides/inline.md`).differs, true, 'spaces beside inline code remain reader-visible')
  currentPages.set('guides/inline', rendered.replace('related.html#related-heading', 'related.html#different-heading'))
  const differentTarget = await generateHistoricalDocs(inlineInput)
  assert.equal(differentTarget.pages.get(`${revision}/docs/guides/inline.md`).differs, true)
})

test('pinned absolute and repository-wide raster images are local; title entities are decoded', async t => {
  const fixture = historyFixture()
  t.after(fixture.cleanup)
  const websiteDir = output(t)
  const absolute = `https://raw.githubusercontent.com/jdevuyst/kio/${fixture.first}/docs/assets/diagram.png`
  fixture.write('shared.png', fs.readFileSync(path.join(fixture.root, 'docs/assets/diagram.png')))
  fixture.write('docs/guides/images.md', `# A & B\n\n![Local](../../shared.png)\n\n![Pinned](${absolute})\n\n![Raw](${absolute}?raw=true)\n`)
  fixture.git('add', '.')
  fixture.git('commit', '--quiet', '-m', 'Images outside documentation directory')
  const revision = fixture.git('rev-parse', 'HEAD')
  const blogPages = new Map([['blog/images.html', `<a href="https://github.com/jdevuyst/kio/blob/${revision}/docs/guides/images.md">images</a>`]])
  const currentPages = new Map([['guides/images', `<h1>A &amp; B</h1><p><img src="../../shared.png" alt="Local"></p><p><img src="${absolute}" alt="Pinned"></p><p><img src="${absolute}?raw=true" alt="Raw"></p>`]])
  const { pages } = await generateHistoricalDocs({ repoRoot: fixture.root, websiteDir, blogPages, currentPages, acquire: false })
  const article = pages.get(`${revision}/docs/guides/images.md`)
  assert.equal(article.title, 'A & B')
  assert.equal(article.differs, false)
  assert.deepEqual(article.releases, [])
  const { html } = body(websiteDir, article.historicalRoute)
  assert.equal([...html.matchAll(/src="\/kio\/history-assets\/[a-f0-9]{64}\.png"/g)].length, 3)
  assert.doesNotMatch(html, /undefined|src="https:/)
  assert.equal(fs.readdirSync(path.join(websiteDir, '.vitepress/public/history-assets')).length, 2)
  fixture.write('renamed.png', fs.readFileSync(path.join(fixture.root, 'shared.png')))
  currentPages.set('guides/images', currentPages.get('guides/images').replace('../../shared.png', '../../renamed.png'))
  const renamed = await generateHistoricalDocs({ repoRoot: fixture.root, websiteDir, blogPages, currentPages, acquire: false })
  assert.equal(renamed.pages.get(`${revision}/docs/guides/images.md`).differs, false, 'renamed identical image bytes are not newer reader content')
  fixture.write('docs/guides/images.md', `# Unsafe remote image\n\n${[`${absolute}?unsupported=1`, `${absolute.replace('raw.githubusercontent.com', 'RAW.GITHUBUSERCONTENT.COM')}?unsupported=1`, `${absolute.replace('https:', '')}?unsupported=1`, `${absolute.replace('https://', 'https://credential@')}?unsupported=1`].map(href => `![Unknown](${href})`).join('\n\n')}\n`)
  fixture.git('add', '.')
  fixture.git('commit', '--quiet', '-m', 'Unsupported image query')
  const unsupported = fixture.git('rev-parse', 'HEAD')
  const unsupportedBlogs = new Map([['blog/images.html', `<a href="https://github.com/jdevuyst/kio/blob/${unsupported}/docs/guides/images.md">images</a>`]])
  await generateHistoricalDocs({ repoRoot: fixture.root, websiteDir, blogPages: unsupportedBlogs, currentPages, acquire: false })
  const unsupportedHtml = body(websiteDir, historyRoute(unsupported, 'docs/guides/images.md')).html
  assert.match(unsupportedHtml, /Image unavailable: Unknown/)
  assert.equal([...unsupportedHtml.matchAll(/Image unavailable: Unknown/g)].length, 4)
  assert.doesNotMatch(unsupportedHtml, /<img|src="https:/)
})

test('historical extraction and reachable inventory have explicit cumulative bounds', async t => {
  const fixture = historyFixture()
  t.after(fixture.cleanup)
  const websiteDir = output(t)
  const source = new HistorySource(fixture.root, path.join(websiteDir, 'objects'), { acquire: false })
  source.bytes = 128 * 1024 * 1024
  assert.throws(() => source.read(fixture.first, 'docs/guides/changed.md'), /128 MiB.*inventory/)
  const revisionSource = new HistorySource(fixture.root, path.join(websiteDir, 'revisions'), { acquire: false })
  for (let index = 0; index < 64; index++) revisionSource.revisions.set(String(index), {})
  assert.throws(() => revisionSource.revision(fixture.first), /64 referenced revisions.*inventory/)
  const blogPages = new Map([['blog/too-many.html', Array.from({ length: 4097 }, (_, index) => `<a href="https://github.com/jdevuyst/kio/blob/${fixture.first}/docs/${index}.md">article</a>`).join('')]])
  await assert.rejects(generateHistoricalDocs({ repoRoot: fixture.root, websiteDir, blogPages, currentPages: fixture.currentPages, acquire: false }), /4096 reachable articles.*inventory/)
})

test('object acquisition uses an exact commit in a private cache and reports missing offline history', async t => {
  const fixture = historyFixture()
  t.after(fixture.cleanup)
  const isolated = output(t)
  const source = new HistorySource(isolated, path.join(isolated, 'objects'), { repository: fixture.root })
  assert.match(source.read(fixture.first, 'docs/guides/changed.md').toString(), /Historical documentation text/)
  assert.equal(source.revisions.size, 1)
  assert.equal(fs.existsSync(path.join(isolated, 'objects/FETCH_HEAD')), false)
  assert.equal(fs.readdirSync(path.join(isolated, 'objects/refs/heads')).length, 0)
  assert.equal(fs.readFileSync(path.join(isolated, 'objects/refs/history', fixture.first), 'utf8').trim(), fixture.first)
  assert.deepEqual(source.revision(fixture.first).releases.map(tag => tag.version), ['0.1.0'])
  const metadata = source.remoteTags
  assert.deepEqual(source.revision(fixture.second).releases.map(tag => tag.version), ['0.1.1'])
  assert.equal(source.remoteTags, metadata, 'tag discovery is cached once per build')
  const blobs = source.blobs.size
  source.read(fixture.first, 'docs/guides/changed.md')
  assert.equal(source.blobs.size, blobs)
  const local = new HistorySource(fixture.root, path.join(isolated, 'local'), { acquire: false })
  assert.deepEqual(local.revision(fixture.between).releases, [])
  const offline = new HistorySource(isolated, path.join(isolated, 'offline'), { acquire: false })
  assert.throws(() => offline.read('f'.repeat(40), 'docs/guides/changed.md'), /unavailable locally.*offline/)
  const noMetadata = new HistorySource(fixture.root, path.join(isolated, 'missing-tags'), { repository: path.join(isolated, 'unavailable-remote') })
  const warning = console.warn
  const warnings = []
  console.warn = message => warnings.push(message)
  try {
    assert.deepEqual(noMetadata.revision(fixture.between).releases, [])
    assert.equal(noMetadata.remoteTags, null)
    assert.deepEqual(noMetadata.releases(fixture.between), [])
    assert.equal(warnings.length, 1)
  } finally { console.warn = warning }
})

test('missing historical article and symlink source cannot masquerade as a rendered snapshot', async t => {
  const fixture = historyFixture()
  t.after(fixture.cleanup)
  const websiteDir = output(t)
  const blogPages = new Map([['blog/missing.html', `<a href="https://github.com/jdevuyst/kio/blob/${fixture.first}/docs/missing.md">missing</a>`]])
  await assert.rejects(generateHistoricalDocs({ repoRoot: fixture.root, websiteDir, blogPages, currentPages: fixture.currentPages, acquire: false }), /source is missing or is not a regular file/)
  if (process.platform !== 'win32') {
    fs.symlinkSync('../../secret.txt', path.join(fixture.root, 'docs/guides/symlink.md'))
    fixture.git('add', '.')
    fixture.git('commit', '--quiet', '-m', 'Symlink documentation source')
    const revision = fixture.git('rev-parse', 'HEAD')
    const linked = new Map([['blog/symlink.html', `<a href="https://github.com/jdevuyst/kio/blob/${revision}/docs/guides/symlink.md">symlink</a>`]])
    await assert.rejects(generateHistoricalDocs({ repoRoot: fixture.root, websiteDir, blogPages: linked, currentPages: fixture.currentPages, acquire: false }), /source is missing or is not a regular file/)
  }
})
