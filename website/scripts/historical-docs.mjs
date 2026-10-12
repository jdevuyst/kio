import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { createMarkdownRenderer } from 'vitepress'
import { parse } from 'vue/compiler-sfc'
import { withHeadingIds } from '../.vitepress/search-content.mjs'
import { docsReference } from './docs-reference.mjs'

const github = 'https://github.com/jdevuyst/kio'
const rasterExtensions = new Set(['.png', '.jpg', '.jpeg', '.gif', '.webp', '.avif', '.ico'])
const historyLimits = { revisions: 64, articles: 4096, bytes: 128 * 1024 * 1024 }
const releaseTagName = /^releases\/v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$/

function digest(value) {
  return createHash('sha256').update(value).digest('hex')
}

function write(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true })
  fs.writeFileSync(file, value)
}

function json(file, value) {
  write(file, `${JSON.stringify(value)}\n`)
}

function articlePath(value) {
  return value.startsWith('docs/') && value.endsWith('.md') && path.posix.normalize(value) === value
    && !/[\\\u0000-\u001f]/.test(value)
}

function pinnedRepositoryFile(href, { image = false } = {}) {
  let url
  try { url = new URL(href) } catch { return null }
  if (url.username || url.password || (url.search && (!image || ![...url.searchParams].every(([key, value]) => key === 'raw' && /^(1|true)$/.test(value))))) return null
  const match = url.origin === 'https://github.com'
    ? url.pathname.match(/^\/jdevuyst\/kio\/(?:blob|tree)\/([a-f0-9]{40})\/(.+)$/)
    : url.origin === 'https://raw.githubusercontent.com' ? url.pathname.match(/^\/jdevuyst\/kio\/([a-f0-9]{40})\/(.+)$/) : null
  if (!match) return null
  let source
  try { source = decodeURIComponent(match[2]) } catch { return null }
  return path.posix.normalize(source) === source && !source.startsWith('../') && !/[\\\u0000-\u001f]/.test(source)
    ? { revision: match[1], source, hash: url.hash } : null
}

export function pinnedDoc(href) {
  const target = pinnedRepositoryFile(href)
  return target && articlePath(target.source) ? target : null
}

export function historyRoute(revision, source) {
  if (!/^[a-f0-9]{40}$/.test(revision) || !articlePath(source)) throw new Error('Invalid historical documentation identity')
  const article = source === 'docs/README.md' ? 'docs/index' : source.slice(0, -3)
  return `/history/${revision}/${article}.html`
}

function git(root, args, options = {}) {
  const result = spawnSync('git', ['--literal-pathspecs', '-c', 'core.hooksPath=', '-c', 'gc.auto=0', '-c', 'maintenance.auto=false', '-C', root, ...args], {
    encoding: options.binary ? null : 'utf8', maxBuffer: options.maxBuffer ?? 32 * 1024 * 1024,
    timeout: options.timeout ?? 120_000,
    env: { ...process.env, GIT_TERMINAL_PROMPT: '0' }
  })
  if (result.error || result.status !== 0) {
    if (options.optional) return null
    throw new Error(`Historical documentation: git ${args[0]} failed: ${result.error?.message ?? String(result.stderr).trim()}`)
  }
  return result.stdout
}

export function releaseTagCommits(output) {
  const tags = new Map()
  for (const line of output.split('\n')) {
    const match = line.match(/^([a-f0-9]{40})\trefs\/tags\/(releases\/v[^\s^]+)(\^\{\})?$/)
    if (!match) continue
    const entry = tags.get(match[2]) ?? {}
    entry[match[3] ? 'commit' : 'object'] = match[1]
    tags.set(match[2], entry)
  }
  return new Map([...tags].filter(([, entry]) => entry.object).map(([name, entry]) => [name, entry.commit ?? entry.object]))
}

export class HistorySource {
  constructor(repoRoot, cacheDir, { acquire = true, repository = `${github}.git` } = {}) {
    this.repoRoot = repoRoot
    this.cacheDir = cacheDir
    this.acquire = acquire
    this.repository = repository
    this.revisions = new Map()
    this.blobs = new Map()
    this.bytes = 0
    this.remoteTags = undefined
  }

  revision(revision) {
    if (this.revisions.has(revision)) return this.revisions.get(revision)
    if (this.revisions.size >= historyLimits.revisions) throw new Error(`Historical documentation exceeds ${historyLimits.revisions} referenced revisions; review the build's historical inventory.`)
    if (!/^[a-f0-9]{40}$/.test(revision)) throw new Error('Historical documentation requires a full commit ID')
    let root = this.repoRoot
    const exists = candidate => git(candidate, ['cat-file', '-t', revision], { optional: true })?.trim() === 'commit'
    if (!exists(root)) {
      root = this.cacheDir
      if (!fs.existsSync(path.join(root, 'HEAD'))) {
        fs.mkdirSync(root, { recursive: true })
        git(root, ['-c', 'init.templateDir=', 'init', '--bare', '--quiet'])
      }
      if (!exists(root)) {
        if (!this.acquire) throw new Error(`Historical documentation commit ${revision} is unavailable locally; fetch that exact commit before building offline.`)
        const fetched = git(root, ['fetch', '--quiet', '--no-tags', '--depth=1', '--no-write-fetch-head', this.repository, revision], { optional: true })
        if (fetched === null || !exists(root)) throw new Error(`Historical documentation commit ${revision} could not be acquired from ${this.repository}; fetch that exact commit before building offline.`)
      }
    }
    const identity = git(root, ['rev-parse', `${revision}^{commit}`]).trim()
    if (identity !== revision) throw new Error(`Historical documentation commit identity mismatch: ${revision}`)
    if (root === this.cacheDir) git(root, ['update-ref', `refs/history/${revision}`, revision])
    const files = new Map()
    for (const record of git(root, ['ls-tree', '-rz', revision, '--', 'docs']).split('\0').filter(Boolean)) {
      const [metadata, name] = record.split('\t')
      const [mode, type, object] = metadata.split(' ')
      if (type === 'blob' && /^(100644|100755)$/.test(mode)) files.set(name, object)
    }
    let releases
    const owner = this
    const snapshot = { root, files,
      get releases() { return releases ??= owner.releases(revision) } }
    this.revisions.set(revision, snapshot)
    return snapshot
  }

  read(revision, source) {
    const snapshot = this.revision(revision)
    if (!snapshot.files.has(source) && !source.startsWith('docs/') && path.posix.normalize(source) === source && !source.startsWith('../') && !source.startsWith('/') && !/[\\\u0000-\u001f]/.test(source)) {
      const entry = git(snapshot.root, ['ls-tree', '-z', revision, '--', source]).split('\0')[0]
      const match = entry.match(/^(100644|100755) blob ([a-f0-9]{40})\t/)
      snapshot.files.set(source, match?.[2] ?? null)
    }
    const object = snapshot.files.get(source)
    if (!object) return null
    if (!this.blobs.has(object)) {
      const bytes = git(snapshot.root, ['cat-file', 'blob', object], { binary: true })
      this.bytes += bytes.length
      if (this.bytes > historyLimits.bytes) throw new Error(`Historical documentation exceeds ${historyLimits.bytes / 1024 / 1024} MiB of extracted content; review the build's historical inventory.`)
      this.blobs.set(object, bytes)
    }
    return this.blobs.get(object)
  }

  releases(revision) {
    const candidates = new Set((git(this.repoRoot, ['tag', '--points-at', revision], { optional: true }) ?? '').split('\n').filter(Boolean))
    if (this.acquire && ![...candidates].some(name => releaseTagName.test(name))) {
      if (this.remoteTags === undefined) {
        const metadata = git(this.repoRoot, ['ls-remote', '--tags', this.repository, 'refs/tags/releases/v*'], { optional: true, timeout: 15_000, maxBuffer: 1024 * 1024 })
        this.remoteTags = metadata === null ? null : releaseTagCommits(metadata)
        if (metadata === null) console.warn('Historical documentation: release-tag metadata unavailable; using commit provenance.')
      }
      for (const [name, commit] of this.remoteTags ?? []) if (commit === revision) candidates.add(name)
    }
    const versions = new Map()
    for (const name of [...candidates].sort().reverse()) {
      const match = name.match(releaseTagName)
      if (match) versions.set(match[1], { version: match[1], tag: name, url: `${github}/tree/${encodeURIComponent(name)}` })
    }
    return [...versions.values()].sort((a, b) => a.version.localeCompare(b.version, 'en', { numeric: true }))
  }
}

export async function historicalRenderer(websiteDir) {
  const headings = await createMarkdownRenderer(websiteDir, {}, '/kio/')
  // An isolated Markdown engine has no file includes, Vue compilation, or
  // frontmatter evaluation. Only synthesized heading text reaches VitePress.
  const md = new headings.constructor({ html: true, linkify: true })
  const inertHtml = (tokens, index) => md.utils.escapeHtml(tokens[index].content.replace(/<!--[\s\S]*?-->/g, ''))
  md.renderer.rules.html_inline = inertHtml
  md.renderer.rules.html_block = inertHtml
  const fence = md.renderer.rules.fence
  md.renderer.rules.fence = (tokens, index, options, env, renderer) => {
    tokens[index].info = tokens[index].info.trim().split(/\s+/, 1)[0]
    return fence(tokens, index, options, env, renderer)
  }
  function tokens(source) {
    const withoutFrontmatter = source.replace(/^\uFEFF?---[^\n]*\r?\n[\s\S]*?\r?\n(?:---|\.\.\.)[^\n]*(?:\r?\n|$)/, '')
    return md.parse(withoutFrontmatter, {})
  }
  return {
    md, tokens,
    render: items => md.renderer.render(items, md.options, {}),
    headings: html => withHeadingIds(html, headings)
  }
}

function visitTokens(tokens, callback) {
  for (const token of tokens) {
    callback(token)
    if (token.children) visitTokens(token.children, callback)
  }
}

function relativeTarget(href, source) {
  if (/^(?:[a-z][a-z0-9+.-]*:|\/\/|#)/i.test(href)) return null
  const match = href.match(/^([^?#]*)([?#].*)?$/)
  if (!match?.[1]) return null
  let decoded
  try { decoded = decodeURIComponent(match[1]) } catch { return null }
  if (/[\\\u0000-\u001f]/.test(decoded)) return null
  const target = decoded.startsWith('/') ? decoded.slice(1) : path.posix.join(path.posix.dirname(source), decoded)
  if (target.startsWith('../')) return null
  return { source: target, suffix: match[2] ?? '' }
}

function sourceUrl(revision, source, suffix = '') {
  return `${github}/${source.endsWith('/') ? 'tree' : 'blob'}/${revision}/${source.split('/').map(encodeURIComponent).join('/')}${suffix}`
}

function githubImage(href) {
  try { return /^(?:[^.]+\.)*github(?:usercontent)?\.com$/i.test(new URL(href, 'https://example.invalid').hostname) }
  catch { return false }
}

function currentPageKey(source) {
  return source === 'docs/README.md' ? 'index' : source.slice(5, -3)
}

// Compare reader content, not syntax-color spans, container classes, or dates.
// The current side is the actual deployed Kiodoc output, not a second raw render.
function visibleContent(html, article, read) {
  const { descriptor } = parse(`<template>${html}</template>`, {
    templateParseOptions: { whitespace: 'preserve', delimiters: ['\u0000{{', '}}\u0000'] }
  })
  const attribute = (node, name) => node.props.find(prop => prop.type === 6 && prop.name === name)?.value?.content ?? ''
  const text = node => node.type === 2 ? node.content : (node.children ?? []).map(text).join('')
  const content = []
  const append = value => {
    if (typeof value === 'string' && typeof content.at(-1) === 'string') content[content.length - 1] += value
    else content.push(value)
  }
  const target = href => {
    if (href.startsWith('/kio/')) href = href.slice(4)
    const deployedSource = href.match(/^https:\/\/github\.com\/jdevuyst\/kio\/(?:blob|tree)\/main\/([^?#]+)([?#].*)?$/)
    if (deployedSource) return `${deployedSource[1]}${deployedSource[2] ?? ''}`
    const local = relativeTarget(href, article)
    if (!local) return href
    const source = local.source.replace(/\.html$/, '.md').replace(/(^docs\/)index\.md$/, '$1README.md')
    return `${source}${local.suffix}`
  }
  const blocks = /^(?:h[1-6]|p|pre|blockquote|ul|ol|li|table|thead|tbody|tr|th|td|hr|br)$/
  const semanticTags = /^(?:h[1-6]|p|pre|blockquote|ul|ol|li|table|thead|tbody|tr|th|td|hr|br|strong|em|s|del)$/
  function visit(node) {
    if (node.type === 2) { append(node.content); return }
    if (node.type !== 1) return
    const classes = attribute(node, 'class').split(/\s+/)
    if (classes.some(name => ['blog-post-date', 'blog-post-footer'].includes(name)) || /^(script|style|button)$/.test(node.tag)) return
    if (node.tag === 'pre' || node.tag === 'code') {
      append([node.tag, text(node).replace(/\r\n/g, '\n').replace(node.tag === 'pre' ? /\n$/ : /$^/, '')])
      return
    }
    if (node.tag === 'img') {
      const href = attribute(node, 'src')
      const image = pinnedRepositoryFile(href, { image: true }) ?? relativeTarget(href, article)
      const bytes = image ? read(image.source, image.revision) : null
      append(['image', bytes ? digest(bytes) : target(href), attribute(node, 'alt'), attribute(node, 'title'), href.match(/#.*$/)?.[0] ?? ''])
      return
    }
    if (node.tag === 'a') append(['link', target(attribute(node, 'href')), attribute(node, 'title')])
    else if (semanticTags.test(node.tag)) append([node.tag, node.tag === 'ol' ? attribute(node, 'start') || '1' : ''])
    for (const child of node.children) visit(child)
    if (node.tag === 'a' || semanticTags.test(node.tag)) append([`/${node.tag}`])
  }
  for (const node of descriptor.template?.ast.children ?? []) visit(node)
  const blockBoundary = value => value === undefined || (Array.isArray(value) && blocks.test(value[0].replace(/^\//, '')))
  return content.map((value, index) => {
    if (typeof value !== 'string') return value
    let prose = value.replace(/[ \t\r\n\f]+/g, ' ')
    if (blockBoundary(content[index - 1])) prose = prose.trimStart()
    if (blockBoundary(content[index + 1])) prose = prose.trimEnd()
    return prose
  }).filter(value => value !== '')
}

export async function generateHistoricalDocs({ repoRoot, websiteDir, blogPages, currentPages, acquire = true, historySource }) {
  const source = historySource ?? new HistorySource(repoRoot, path.join(websiteDir, '.vitepress/cache/history-git'), { acquire })
  const generated = path.join(websiteDir, '.vitepress/generated/history')
  const routes = path.join(websiteDir, 'history')
  const assets = path.join(websiteDir, '.vitepress/public/history-assets')
  for (const dir of [generated, routes, assets]) fs.rmSync(dir, { recursive: true, force: true })
  const queued = new Map()
  const pages = new Map()
  const enqueue = pin => {
    const key = `${pin.revision}/${pin.source}`
    if (!queued.has(key)) {
      if (queued.size >= historyLimits.articles) throw new Error(`Historical documentation exceeds ${historyLimits.articles} reachable articles; review the build's historical inventory.`)
      queued.set(key, pin)
    }
    return historyRoute(pin.revision, pin.source)
  }
  const rewrittenBlogs = new Map([...blogPages].map(([key, html]) => [key,
    html.replace(/href="([^"]+)"/g, (original, href) => {
      const pin = pinnedDoc(href)
      return pin ? `href="/kio${enqueue(pin)}${pin.hash}"` : original
    })]))
  const renderer = await historicalRenderer(websiteDir)
  for (const [key, pin] of queued) {
    const bytes = source.read(pin.revision, pin.source)
    if (!bytes) throw new Error(`Historical documentation source is missing or is not a regular file: ${pin.revision}:${pin.source}`)
    const snapshot = source.revision(pin.revision)
    const historicalTokens = renderer.tokens(bytes.toString('utf8'))
    const historicalContent = visibleContent(renderer.render(historicalTokens), pin.source, (target, revision) => source.read(revision ?? pin.revision, target))
    const pageKey = currentPageKey(pin.source)
    const currentFile = path.join(repoRoot, pin.source)
    const currentAvailable = currentPages.has(pageKey) && fs.existsSync(currentFile) && fs.lstatSync(currentFile).isFile()
    let differs = false
    if (currentAvailable) {
      const current = visibleContent(currentPages.get(pageKey), pin.source, (target, revision) => {
        if (revision) return source.read(revision, target)
        const file = path.join(repoRoot, target)
        return fs.existsSync(file) && fs.lstatSync(file).isFile() ? fs.readFileSync(file) : null
      })
      differs = JSON.stringify(historicalContent) !== JSON.stringify(current)
    }
    visitTokens(historicalTokens, token => {
      const attribute = token.type === 'link_open' ? 'href' : token.type === 'image' ? 'src' : null
      if (!attribute) return
      const href = token.attrGet(attribute)
      const absolutePin = pinnedDoc(href)
      if (absolutePin && token.type === 'link_open') {
        token.attrSet(attribute, `/kio${enqueue(absolutePin)}${absolutePin.hash}`)
        return
      }
      const target = token.type === 'image' ? pinnedRepositoryFile(href, { image: true }) ?? relativeTarget(href, pin.source) : relativeTarget(href, pin.source)
      if (!target && (token.type !== 'image' || !githubImage(href))) return
      if (token.type === 'link_open') {
        let article = target.source
        if (article.endsWith('/')) article += 'README.md'
        if (articlePath(article) && source.read(pin.revision, article)) {
          token.attrSet(attribute, `/kio${enqueue({ revision: pin.revision, source: article })}${target.suffix}`)
        } else token.attrSet(attribute, sourceUrl(pin.revision, target.source, target.suffix))
        return
      }
      const image = target ? source.read(target.revision ?? pin.revision, target.source) : null
      const extension = target ? path.posix.extname(target.source).toLowerCase() : ''
      if (image && rasterExtensions.has(extension)) {
        const name = `${digest(image)}${extension}`
        write(path.join(assets, name), image)
        token.attrSet(attribute, `/kio/history-assets/${name}${target.suffix ?? target.hash ?? ''}`)
      } else if (image && extension === '.svg') {
        // SVG image documents are inert in an img's data URL and cannot
        // acquire the site's origin when opened as a separate document.
        token.attrSet(attribute, `data:image/svg+xml;base64,${image.toString('base64')}`)
      } else {
        token.type = 'code_inline'
        token.tag = 'code'
        token.content = `[Image unavailable: ${token.content}]`
        token.attrs = null
        token.children = null
      }
    })
    const html = await renderer.headings(`<div class="kiodoc-page">${renderer.render(historicalTokens)}</div>`)
    const title = renderer.md.utils.unescapeAll(html.match(/<h1[^>]*>([\s\S]*?)<\/h1>/)?.[1].replace(/<[^>]+>/g, '') ?? path.posix.basename(pin.source, '.md'))
    const article = {
      title, source: pin.source, revision: pin.revision, releases: snapshot.releases,
      sourceUrl: sourceUrl(pin.revision, pin.source),
      historicalRoute: historyRoute(pin.revision, pin.source),
      currentRoute: currentAvailable ? (pageKey.startsWith('blog/') ? `/${pageKey}.html` : `/docs/${pageKey === 'index' ? '' : `${pageKey}.html`}`) : null,
      currentAvailable, differs
    }
    const bodyFile = path.join(generated, 'bodies', `${digest(html)}.json`)
    json(bodyFile, html)
    const articleId = digest(key)
    const articleFile = path.join(generated, 'articles', `${articleId}.json`)
    json(articleFile, article)
    const out = path.join(websiteDir, article.historicalRoute.slice(1).replace(/\.html$/, '.md'))
    const imports = `import article from ${JSON.stringify(`./${path.relative(path.dirname(out), articleFile).split(path.sep).join('/')}`)}\n`
      + `import articleHtml from ${JSON.stringify(`./${path.relative(path.dirname(out), bodyFile).split(path.sep).join('/')}`)}\n`
    write(out, `---\ntitle: ${JSON.stringify(title)}\nsearch: false\neditLink: false\nlastUpdated: false\nprev: false\nnext: false\n---\n\n<script setup>\n${imports}</script>\n\n<HistoricalDocPage :article="article" :html="articleHtml" />\n`)
    pages.set(key, article)
  }
  const sidebars = {}
  for (const revision of new Set([...pages.values()].map(page => page.revision))) {
    const items = [...pages.values()].filter(page => page.revision === revision)
      .sort((a, b) => a.source.localeCompare(b.source))
      .map(page => ({ text: page.title, link: page.historicalRoute }))
    sidebars[`/history/${revision}/docs/`] = [{ text: `Docs at ${revision.slice(0, 8)}`, items }]
    const snapshot = source.revision(revision)
    const reference = docsReference(revision, entry => git(snapshot.root,
      ['cat-file', '-t', `${revision}:${entry.source}`], { optional: true })?.trim() === entry.kind)
    if (reference.items.length) sidebars[`/history/${revision}/docs/`].push(reference)
  }
  write(path.join(websiteDir, '.vitepress/generated/history-nav.mjs'), `export const historicalSidebars = ${JSON.stringify(sidebars)}\n`)
  return { rewrittenBlogs, pages, sidebars }
}
