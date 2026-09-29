#!/usr/bin/env node
import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { siteUrl } from '../.vitepress/site.mjs'

const websiteDir = path.resolve(new URL('..', import.meta.url).pathname)
const repoRoot = path.resolve(websiteDir, '..')
const docsDir = path.join(repoRoot, 'docs')
const specsBackendsDir = path.join(repoRoot, 'specs', 'backends')
const generatedDir = path.join(websiteDir, '.vitepress', 'generated')
const generatedDocsDir = path.join(websiteDir, 'docs')
const blogDir = path.join(docsDir, 'blog')
const generatedBlogDir = path.join(websiteDir, 'blog')
const helloExampleDir = path.join(websiteDir, 'examples', 'hello-world')
const kiodocOutDir = path.join(docsDir, 'out', 'docs')
const githubBase = 'https://github.com/jdevuyst/kio'
const siteBase = '/kio/'
const siteOrigin = new URL(siteUrl).origin
const kiodocPages = new Map()
const escapedPipeSentinel = '\u0000KIO_ESCAPED_PIPE\u0000'

const blogLead = 'Design notes, milestones, and deep dives.'
const blogDisclosure = 'Blog posts are written by hand — unlike the rest of Kio’s documentation, which is generated with AI.'
const postDisclosure = 'Written by hand — the rest of Kio’s docs are AI-generated.'

const sectionOrder = [
  ['Tutorials', 'tutorials', 'tutorials'],
  ['Guides', 'guides', 'guides'],
  ['Case studies', 'caseStudies', 'poc'],
  ['Host integrations', 'hosts', 'hosts']
]

function fail(message) {
  console.error(`prepare-docs: ${message}`)
  process.exit(1)
}

function readText(file) {
  return fs.readFileSync(file, 'utf8')
}

function writeText(file, text) {
  fs.mkdirSync(path.dirname(file), { recursive: true })
  fs.writeFileSync(file, text)
}

function escapeHtml(value) {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
}

function posixPath(value) {
  return value.split(path.sep).join(path.posix.sep)
}

function stripMdExt(value) {
  return value.replace(/\.md$/i, '')
}

function stripHtmlExt(value) {
  return value.replace(/\.html$/i, '')
}

function parseDocsReadme() {
  const readmePath = path.join(docsDir, 'README.md')
  const lines = readText(readmePath).split(/\r?\n/)
  const sections = new Map(sectionOrder.map(([title, key, route]) => [key, { title, route, items: [] }]))
  let currentKey = null

  for (const line of lines) {
    const heading = line.match(/^## (.+)$/)
    if (heading) {
      const found = sectionOrder.find(([title]) => title === heading[1])
      currentKey = found ? found[1] : null
      continue
    }

    if (!currentKey) continue
    if (/^- \*\*\[/.test(line)) {
      fail(`docs/README.md section "${sections.get(currentKey).title}" uses a bold catalogue link; use a plain list link`)
    }
    const item = line.match(/^- \[([^\]]+)\]\(([^)]+)\)(?:\s+.\s+(.*))?$/)
    if (!item) continue
    const [, text, href, summary = ''] = item
    if (!href.endsWith('.md')) {
      fail(`docs/README.md section "${sections.get(currentKey).title}" links to non-Markdown target: ${href}`)
    }
    const target = path.join(docsDir, href)
    if (!fs.existsSync(target)) {
      fail(`docs/README.md links to missing file: ${href}`)
    }
    sections.get(currentKey).items.push({
      text,
      href: posixPath(path.posix.normalize(href)),
      summary: summary.trim()
    })
  }

  for (const [title, key] of sectionOrder) {
    if (sections.get(key).items.length === 0) {
      fail(`docs/README.md has no entries for "${title}"`)
    }
  }
  return sections
}

function listMarkdownFiles(relativeDir) {
  const dir = path.join(docsDir, relativeDir)
  return fs.readdirSync(dir)
    .filter((name) => name.endsWith('.md'))
    .map((name) => path.posix.join(relativeDir, name))
    .sort()
}

function validateDocsCatalogue(sections) {
  const catalogued = new Set()
  for (const section of sections.values()) {
    for (const item of section.items) {
      catalogued.add(item.href)
    }
  }

  for (const dir of ['tutorials', 'guides', 'poc', 'hosts']) {
    for (const file of listMarkdownFiles(dir)) {
      if (!catalogued.has(file)) {
        fail(`${file} is not listed in docs/README.md`)
      }
    }
  }
}

function backendTitle(file) {
  const first = readText(file).split(/\r?\n/, 1)[0]
  const heading = first.replace(/^#\s+/, '').replace(/\s+backend$/, '')
  return heading || path.basename(file, '.md')
}

function buildLanguages(hostItems) {
  const backendFiles = fs.readdirSync(specsBackendsDir)
    .filter((name) => name.endsWith('.md') && name !== 'README.md')
    .sort()
  const backends = new Map(backendFiles.map((file) => {
    const id = path.basename(file, '.md')
    return [id, {
      id,
      name: backendTitle(path.join(specsBackendsDir, file)),
      specPath: `specs/backends/${file}`,
      specUrl: `${githubBase}/blob/main/specs/backends/${file}`
    }]
  }))

  const hostById = new Map(hostItems.map((item) => [path.basename(item.href, '.md'), item]))
  const missingHosts = [...backends.keys()].filter((id) => !hostById.has(id))
  const extraHosts = [...hostById.keys()].filter((id) => !backends.has(id))
  if (missingHosts.length > 0) {
    fail(`docs/README.md Host integrations is missing host guide(s): ${missingHosts.join(', ')}`)
  }
  if (extraHosts.length > 0) {
    fail(`docs/README.md Host integrations lists guide(s) with no matching backend: ${extraHosts.join(', ')}`)
  }

  return hostItems.map((item) => {
    const id = path.basename(item.href, '.md')
    const backend = backends.get(id)
    return {
      id,
      name: item.text,
      docPath: item.href,
      docRoute: `/docs/${stripMdExt(item.href)}`,
      specPath: backend.specPath,
      specUrl: backend.specUrl
    }
  })
}

function runKiodocBuild() {
  const result = spawnSync('cargo', [
    'run',
    '--quiet',
    '--manifest-path',
    '../kio-rs/Cargo.toml',
    '--bin',
    'kio',
    '--',
    'doc',
    'build',
    '--html'
  ], {
    cwd: docsDir,
    stdio: 'inherit'
  })
  if (result.status !== 0) {
    fail('kio doc build --html failed')
  }
}

function runKio(args, options = {}) {
  const result = spawnSync('cargo', [
    'run',
    '--quiet',
    '--manifest-path',
    path.join(repoRoot, 'kio-rs', 'Cargo.toml'),
    '--bin',
    'kio',
    '--',
    ...args
  ], {
    cwd: options.cwd ?? repoRoot,
    encoding: 'utf8',
    stdio: options.stdio ?? 'pipe'
  })
  if (result.status !== 0) {
    if (result.stdout) process.stdout.write(result.stdout)
    if (result.stderr) process.stderr.write(result.stderr)
    fail(`kio ${args.join(' ')} failed`)
  }
  return result.stdout
}

function tokenCssClass(kind) {
  return `kio-${kind.replaceAll('.', '-')}`
}

function highlightKioFile(file) {
  const source = readText(file)
  const tokens = JSON.parse(runKio(['debug', 'tokens', file]))
  let html = ''
  let offset = 0

  for (const token of tokens) {
    html += escapeHtml(source.slice(offset, token.start))
    html += `<span class="${tokenCssClass(token.kind)}">${escapeHtml(source.slice(token.start, token.end))}</span>`
    offset = token.end
  }

  html += escapeHtml(source.slice(offset))
  return { source, highlightedHtml: html }
}

function extractMain(htmlFile) {
  const html = readText(htmlFile)
  const match = html.match(/<main>\n([\s\S]*?)<\/main>/)
  if (!match) {
    fail(`could not extract <main> from ${path.relative(repoRoot, htmlFile)}`)
  }
  return match[1].trim()
}

function stripRenderedHtmlComments(body) {
  const withoutLiteralComments = body.replace(/<!--[\s\S]*?-->/g, '')
  const startMarker = '<p>&lt;!--'
  const endMarker = '--&gt;</p>'
  let cleaned = ''
  let offset = 0

  for (;;) {
    const start = withoutLiteralComments.indexOf(startMarker, offset)
    if (start === -1) {
      cleaned += withoutLiteralComments.slice(offset)
      break
    }

    const end = withoutLiteralComments.indexOf(endMarker, start)
    if (end === -1) {
      fail('rendered HTML comment in generated Kiodoc output is not closed')
    }

    cleaned += withoutLiteralComments.slice(offset, start)
    offset = end + endMarker.length
    while (withoutLiteralComments[offset] === '\n') {
      offset += 1
    }
  }

  return cleaned.replace(/\n{3,}/g, '\n\n').trim()
}

function splitCollapsedTableRows(text) {
  const rows = []
  let start = 0

  for (let index = 0; index < text.length - 1; index += 1) {
    if (text[index] !== '|') continue
    if (index === start) continue

    let next = index + 1
    while (/\s/.test(text[next] ?? '')) {
      next += 1
    }

    if (text[next] !== '|') continue
    rows.push(text.slice(start, index + 1).trim())
    start = next
    index = next - 1
  }

  rows.push(text.slice(start).trim())
  return rows.filter(Boolean)
}

function splitTableCells(row) {
  const cells = []
  let cell = ''
  let escaped = false

  for (const char of row) {
    if (char === '|' && !escaped) {
      cells.push(cell.trim().replace(/\\\|/g, '|'))
      cell = ''
      continue
    }

    cell += char
    escaped = char === '\\' && !escaped
    if (char !== '\\') {
      escaped = false
    }
  }

  cells.push(cell.trim().replace(/\\\|/g, '|'))
  if (cells[0] === '') cells.shift()
  if (cells[cells.length - 1] === '') cells.pop()
  return cells
}

function protectEscapedPipes(value) {
  return value.replace(/\\\|/g, escapedPipeSentinel)
}

function restoreEscapedPipes(value) {
  return value.replaceAll(escapedPipeSentinel, '|')
}

function tableAlignment(separator) {
  const trimmed = separator.trim()
  if (trimmed.startsWith(':') && trimmed.endsWith(':')) return 'center'
  if (trimmed.endsWith(':')) return 'right'
  if (trimmed.startsWith(':')) return 'left'
  return null
}

function isSeparatorCells(cells) {
  return cells.length > 0 && cells.every((cell) => /^:?-+:?$/.test(cell.trim()))
}

function tableCell(tag, value, alignment) {
  const align = alignment ? ` style="text-align: ${alignment}"` : ''
  return `<${tag}${align}>${value}</${tag}>`
}

function renderTableParagraph(innerHtml) {
  const rows = splitCollapsedTableRows(protectEscapedPipes(innerHtml.trim()))
    .map((row) => splitTableCells(row).map(restoreEscapedPipes))
  if (rows.length < 2 || !isSeparatorCells(rows[1]) || rows[0].length !== rows[1].length) {
    return null
  }

  const alignments = rows[1].map(tableAlignment)
  const head = rows[0].map((cell, index) => tableCell('th', cell, alignments[index])).join('')
  const bodyRows = rows.slice(2)
  if (bodyRows.length === 0 || bodyRows.some((row) => row.length !== rows[0].length)) return null

  const body = bodyRows
    .map((row) => `<tr>${row.map((cell, index) => tableCell('td', cell, alignments[index])).join('')}</tr>`)
    .join('\n')

  return `<table>\n<thead><tr>${head}</tr></thead>\n<tbody>\n${body}\n</tbody>\n</table>`
}

function restoreRenderedMarkdownTables(body) {
  return body.replace(/<p>([\s\S]*?)<\/p>/g, (full, innerHtml) => {
    const table = renderTableParagraph(innerHtml)
    return table ?? full
  })
}

function extractTitle(body, fallback) {
  const match = body.match(/<h1[^>]*>(.*?)<\/h1>/)
  if (!match) return fallback
  return match[1].replace(/<[^>]+>/g, '').replace(/&#39;/g, "'").trim() || fallback
}

function repoUrlFor(repoRelative) {
  const normalized = repoRelative.replace(/^\/+/, '')
  if (normalized.endsWith('/')) {
    return `${githubBase}/tree/main/${normalized.replace(/\/+$/, '')}`
  }
  return `${githubBase}/blob/main/${normalized}`
}

function routeHref(fromHtmlRel, targetHtmlRel) {
  const fromDir = path.posix.dirname(fromHtmlRel)
  let targetRoute = targetHtmlRel === 'README.html' ? 'index.html' : targetHtmlRel
  let targetIsDirectoryIndex = false
  if (targetRoute.endsWith('/index.html')) {
    targetRoute = targetRoute.slice(0, -'index.html'.length)
    targetIsDirectoryIndex = true
  }
  let relative = path.posix.relative(fromDir, targetRoute)
  if (relative === '') {
    relative = targetIsDirectoryIndex ? './' : '.'
  } else if (targetIsDirectoryIndex && !relative.endsWith('/')) {
    relative = `${relative}/`
  }
  if (!relative.startsWith('.')) {
    relative = `./${relative}`
  }
  return relative
}

function rewriteLinks(body, htmlRel, importedHtml) {
  return body.replace(/href="([^"]+)"/g, (_full, href) => {
    if (/^(https?:|mailto:|tel:|#)/.test(href)) return `href="${href}"`
    const [rawPath, hash = ''] = href.split('#')
    const hashSuffix = hash ? `#${hash}` : ''
    const withoutLeadingParents = rawPath.replace(/^(\.\.\/)+/, '')

    if (withoutLeadingParents.startsWith('specs/') || withoutLeadingParents.startsWith('test-data/')) {
      return `href="${repoUrlFor(withoutLeadingParents)}${hashSuffix}"`
    }

    if (rawPath.endsWith('.md')) {
      const repoRelativeMd = path.posix.normalize(path.posix.join('docs', path.posix.dirname(htmlRel), rawPath))
      const sourceFile = path.join(repoRoot, repoRelativeMd)
      if (!fs.existsSync(sourceFile)) {
        fail(`${htmlRel} links to missing Markdown source ${rawPath}`)
      }
      if (!repoRelativeMd.startsWith('docs/')) {
        return `href="${repoUrlFor(repoRelativeMd)}${hashSuffix}"`
      }
      const resolvedMd = repoRelativeMd.slice('docs/'.length)
      const targetHtml = resolvedMd === 'README.md' ? 'README.html' : `${stripMdExt(resolvedMd)}.html`
      return `href="${routeHref(htmlRel, targetHtml)}${hashSuffix}"`
    }

    if (rawPath.endsWith('.html')) {
      const resolvedHtml = path.posix.normalize(path.posix.join(path.posix.dirname(htmlRel), rawPath))
      if (importedHtml.has(resolvedHtml)) {
        return `href="${routeHref(htmlRel, resolvedHtml)}${hashSuffix}"`
      }
    }

    if (rawPath.endsWith('/')) {
      const resolvedIndex = path.posix.normalize(path.posix.join(path.posix.dirname(htmlRel), rawPath, 'index.html'))
      if (importedHtml.has(resolvedIndex)) {
        return `href="${routeHref(htmlRel, resolvedIndex)}${hashSuffix}"`
      }
      fail(`${htmlRel} links to directory ${rawPath}, but the website has no generated index page for it`)
    }

    return `href="${href}"`
  })
}

function sectionIndexBody(section) {
  const htmlRel = `${section.route}/index.html`
  const items = section.items.map((item) => {
    const targetHtml = `${stripMdExt(item.href)}.html`
    const summary = item.summary ? ` — ${inlineSummaryHtml(item.summary)}` : ''
    return `  <li><a href="${routeHref(htmlRel, targetHtml)}">${escapeHtml(item.text)}</a>${summary}</li>`
  }).join('\n')
  return `<h1>${escapeHtml(section.title)}</h1>\n\n<ul>\n${items}\n</ul>`
}

function inlineSummaryHtml(summary) {
  return escapeHtml(summary).replace(/`([^`]+)`/g, '<code>$1</code>')
}

function writeVitePressPage(markdownRel, title, body) {
  const outFile = path.join(generatedDocsDir, markdownRel)
  const pageKey = stripMdExt(markdownRel)
  const html = `<div class="kiodoc-page">\n\n${body}\n\n</div>`
  kiodocPages.set(pageKey, html)
  writeText(outFile, `---\ntitle: ${JSON.stringify(title)}\neditLink: false\n---\n\n<KiodocPage page=${JSON.stringify(pageKey)} />\n`)
}

function generateDocsPages(sections) {
  fs.rmSync(generatedDocsDir, { recursive: true, force: true })
  const navItems = [...sections.values()].flatMap((section) => section.items)
  const importedHtml = new Set(navItems.map((item) => `${stripMdExt(item.href)}.html`))
  importedHtml.add('README.html')
  importedHtml.add('package-boundary.html')
  for (const section of sections.values()) {
    importedHtml.add(`${section.route}/index.html`)
  }

  const readmeHtml = path.join(kiodocOutDir, 'README.html')
  if (!fs.existsSync(readmeHtml)) {
    fail('kio doc build did not render README.html')
  }
  const readmeBody = rewriteLinks(restoreRenderedMarkdownTables(stripRenderedHtmlComments(extractMain(readmeHtml))), 'README.html', importedHtml)
  writeVitePressPage('index.md', extractTitle(readmeBody, 'Kio documentation'), readmeBody)

  for (const section of sections.values()) {
    writeVitePressPage(`${section.route}/index.md`, section.title, sectionIndexBody(section))
  }

  for (const item of navItems) {
    const htmlRel = `${stripMdExt(item.href)}.html`
    const htmlFile = path.join(kiodocOutDir, htmlRel)
    if (!fs.existsSync(htmlFile)) {
      fail(`kio doc build did not render ${htmlRel}`)
    }
    const body = rewriteLinks(restoreRenderedMarkdownTables(stripRenderedHtmlComments(extractMain(htmlFile))), htmlRel, importedHtml)
    const title = extractTitle(body, item.text)
    const outRel = item.href === 'README.md' ? 'index.md' : item.href
    writeVitePressPage(outRel, title, body)
  }

  const packageBoundary = path.join(kiodocOutDir, 'package-boundary.html')
  if (fs.existsSync(packageBoundary)) {
    const htmlRel = 'package-boundary.html'
    const body = rewriteLinks(restoreRenderedMarkdownTables(stripRenderedHtmlComments(extractMain(packageBoundary))), htmlRel, importedHtml)
    writeVitePressPage('package-boundary.md', 'Package Boundary', body)
  }
}

function listBlogPosts() {
  if (!fs.existsSync(blogDir)) return []
  return fs.readdirSync(blogDir)
    .filter((name) => /^\d{4}-\d{2}-\d{2}-.+\.md$/.test(name))
    .map((name) => ({
      name,
      date: name.slice(0, 10),
      slug: stripMdExt(name)
    }))
    .sort((left, right) => right.date.localeCompare(left.date) || right.slug.localeCompare(left.slug))
}

function parseByline(body) {
  const match = body.match(/<em>\s*By\s+<a href="([^"]+)"[^>]*>([^<]+)<\/a>\s*<\/em>/i)
  if (!match) return null
  return { url: match[1], name: match[2] }
}

function injectPostDate(body, date) {
  const time = `<time datetime="${date}">${date}</time>`
  const withByline = body.replace(
    /(<em>\s*By\s+<a href="[^"]+"[^>]*>[^<]+<\/a>)(\s*<\/em>)/i,
    `$1 · ${time}$2`
  )
  if (withByline !== body) return withByline
  return body.replace(/(<\/h1>)/i, `$1\n<p class="blog-post-date">${time}</p>`)
}

function blogIndexBody(entries) {
  const items = entries.map((entry) => {
    const author = entry.author
      ? ` <span class="blog-author">by <a href="${entry.author.url}">${entry.author.name}</a></span>`
      : ''
    return `  <li>
    <a class="blog-post-title" href="./${entry.slug}.html">${escapeHtml(entry.title)}</a>
    <div class="blog-post-meta"><time datetime="${entry.date}">${entry.date}</time>${author}</div>
  </li>`
  }).join('\n')
  const list = entries.length > 0
    ? `<ul class="blog-index">\n${items}\n</ul>`
    : '<p>No posts yet.</p>'
  return `<h1>Blog</h1>\n\n<p class="blog-lead">${escapeHtml(blogLead)}</p>\n\n<p class="blog-disclosure">${escapeHtml(blogDisclosure)}</p>\n\n${list}\n\n<p class="blog-rss"><a href="./feed.xml">Subscribe via RSS</a></p>`
}

function generateBlog() {
  fs.rmSync(generatedBlogDir, { recursive: true, force: true })
  const posts = listBlogPosts()
  const importedHtml = new Set(posts.map((post) => `blog/${stripMdExt(post.name)}.html`))
  const entries = []

  for (const post of posts) {
    const htmlRel = `blog/${stripMdExt(post.name)}.html`
    const htmlFile = path.join(kiodocOutDir, htmlRel)
    if (!fs.existsSync(htmlFile)) {
      fail(`kio doc build did not render ${htmlRel}`)
    }
    const body = rewriteLinks(restoreRenderedMarkdownTables(stripRenderedHtmlComments(extractMain(htmlFile))), htmlRel, importedHtml)
    const author = parseByline(body)
    const title = extractTitle(body, post.slug)
    const pageKey = `blog/${post.slug}`
    const postHtml = `${injectPostDate(body, post.date)}\n\n<footer class="blog-post-footer">${escapeHtml(postDisclosure)}</footer>`
    kiodocPages.set(pageKey, `<div class="kiodoc-page">\n\n${postHtml}\n\n</div>`)
    writeText(
      path.join(generatedBlogDir, `${post.slug}.md`),
      `---\ntitle: ${JSON.stringify(title)}\neditLink: false\nsidebar: false\nprev: false\nnext: false\n---\n\n<KiodocPage page=${JSON.stringify(pageKey)} />\n`
    )
    entries.push({ slug: post.slug, title, date: post.date, author, content: body })
  }

  const indexKey = 'blog/index'
  kiodocPages.set(indexKey, `<div class="kiodoc-page">\n\n${blogIndexBody(entries)}\n\n</div>`)
  writeText(
    path.join(generatedBlogDir, 'index.md'),
    `---\ntitle: "Blog"\neditLink: false\nsidebar: false\nprev: false\nnext: false\n---\n\n<KiodocPage page=${JSON.stringify(indexKey)} />\n`
  )
  generateBlogFeed(entries)
  return entries
}

function rfc822(date) {
  return new Date(`${date}T00:00:00Z`).toUTCString()
}

function absolutizeFeedUrls(html) {
  return html.replace(/((?:href|src)=")\//g, `$1${siteOrigin}/`)
}

function feedCdata(text) {
  return `<![CDATA[${text.replace(/]]>/g, ']]]]><![CDATA[>')}]]>`
}

function feedExcerpt(html) {
  const text = html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim()
  return text.length > 280 ? `${text.slice(0, 280).trimEnd()}…` : text
}

function generateBlogFeed(entries) {
  const built = entries.length > 0
    ? rfc822(entries[0].date)
    : new Date('1970-01-01T00:00:00Z').toUTCString()
  const items = entries.map((entry) => {
    const url = `${siteUrl}/blog/${entry.slug}.html`
    const creator = entry.author ? `\n      <dc:creator>${escapeHtml(entry.author.name)}</dc:creator>` : ''
    return `    <item>
      <title>${escapeHtml(entry.title)}</title>
      <link>${url}</link>
      <guid isPermaLink="true">${url}</guid>
      <pubDate>${rfc822(entry.date)}</pubDate>${creator}
      <description>${escapeHtml(feedExcerpt(entry.content))}</description>
      <content:encoded>${feedCdata(absolutizeFeedUrls(entry.content))}</content:encoded>
    </item>`
  }).join('\n')
  const xml = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>Kio Blog</title>
    <link>${siteUrl}/blog/</link>
    <atom:link href="${siteUrl}/blog/feed.xml" rel="self" type="application/rss+xml"/>
    <description>${escapeHtml(blogLead)}</description>
    <language>en</language>
    <lastBuildDate>${built}</lastBuildDate>
${items}
  </channel>
</rss>
`
  writeText(path.join(websiteDir, '.vitepress', 'public', 'blog', 'feed.xml'), xml)
}

function sidebarLink(item) {
  if (item.href === 'README.md') return '/docs/'
  return `/docs/${stripMdExt(item.href)}`
}

function generateNav(sections) {
  const sidebar = []
  for (const [title, key] of sectionOrder) {
    const section = sections.get(key)
    const items = section.items.map((item) => ({ text: item.text, link: sidebarLink(item) }))
    sidebar.push({ text: title, link: `/docs/${section.route}/`, items })
  }
  sidebar.push({
    text: 'Reference',
    items: [
      { text: 'Language spec', link: `${githubBase}/blob/main/specs/language.md` },
      { text: 'CLI spec', link: `${githubBase}/blob/main/specs/cli.md` },
      { text: 'Package spec', link: `${githubBase}/blob/main/specs/package.md` },
      { text: 'Backend contracts', link: `${githubBase}/tree/main/specs/backends` }
    ]
  })

  writeText(path.join(generatedDir, 'nav.mjs'), `export const docsSidebar = ${JSON.stringify(sidebar, null, 2)}\n`)
}

function generateLanguages(languages) {
  writeText(
    path.join(generatedDir, 'languages.mjs'),
    `export const supportedLanguages = ${JSON.stringify(languages, null, 2)}\n\nexport const languageCount = supportedLanguages.length\n`
  )

  fs.rmSync(path.join(websiteDir, 'targets.md'), { force: true })
}

function generateHelloExample() {
  const files = ['hello.kio', 'hello.pkg.kio'].map((name) => {
    const rendered = highlightKioFile(path.join(helloExampleDir, name))
    return { name, ...rendered }
  })
  writeText(path.join(generatedDir, 'helloExample.mjs'), `export const helloExampleFiles = ${JSON.stringify(files, null, 2)}\n`)
}

function generateKiodocHtml() {
  const pages = Object.fromEntries([...kiodocPages.entries()].sort(([left], [right]) => left.localeCompare(right)))
  writeText(path.join(generatedDir, 'kiodoc-html.mjs'), `export const kiodocHtml = ${JSON.stringify(pages, null, 2)}\n`)
}

function main() {
  const sections = parseDocsReadme()
  validateDocsCatalogue(sections)
  const languages = buildLanguages(sections.get('hosts').items)
  runKiodocBuild()
  generateDocsPages(sections)
  const blogEntries = generateBlog()
  generateKiodocHtml()
  generateLanguages(languages)
  generateHelloExample()
  generateNav(sections)
  console.log(`prepare-docs: generated docs for ${languages.length} host languages, ${blogEntries.length} blog post(s)`)
}

main()
