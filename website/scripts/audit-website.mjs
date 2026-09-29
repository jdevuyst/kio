#!/usr/bin/env node
import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'

const websiteDir = path.resolve(new URL('..', import.meta.url).pathname)
const repoRoot = path.resolve(websiteDir, '..')
const docsDir = path.join(repoRoot, 'docs')

const docsSections = [
  ['Tutorials', 'tutorials'],
  ['Guides', 'guides'],
  ['Case studies', 'poc'],
  ['Host integrations', 'hosts']
]

const findings = []

function add(message) {
  findings.push(message)
}

function read(relative) {
  return fs.readFileSync(path.join(repoRoot, relative), 'utf8')
}

function exists(relative) {
  return fs.existsSync(path.join(repoRoot, relative))
}

function executable(relative) {
  try {
    return (fs.statSync(path.join(repoRoot, relative)).mode & 0o111) !== 0
  } catch {
    return false
  }
}

function expectedTokenClasses() {
  const source = read('kio-rs/src/tokens.rs')
  return [...source.matchAll(/=> "(kio-[^"]+)"/g)].map((match) => match[1]).sort()
}

function formatterColumnBudget() {
  const source = read('kio-rs/src/pretty.rs')
  const match = source.match(/const WIDTH:\s*usize\s*=\s*(\d+);/)
  if (!match) {
    add('kio formatter width constant was not found in kio-rs/src/pretty.rs')
    return null
  }
  return Number(match[1])
}

function countSpellings(count) {
  const words = new Map([
    [0, 'zero'],
    [1, 'one'],
    [2, 'two'],
    [3, 'three'],
    [4, 'four'],
    [5, 'five'],
    [6, 'six'],
    [7, 'seven'],
    [8, 'eight'],
    [9, 'nine'],
    [10, 'ten'],
    [11, 'eleven'],
    [12, 'twelve']
  ])
  return [String(count), words.get(count)].filter(Boolean)
}

function backendIds() {
  return fs.readdirSync(path.join(repoRoot, 'specs', 'backends'))
    .filter((name) => name.endsWith('.md') && name !== 'README.md')
    .map((name) => path.basename(name, '.md'))
    .sort()
}

function stripMdExt(value) {
  return value.replace(/\.md$/i, '')
}

function docsLink(href) {
  if (href === 'README.md') return '/docs/'
  return `/docs/${stripMdExt(href)}`
}

function parseDocsCatalogue() {
  const lines = read('docs/README.md').split(/\r?\n/)
  const sections = new Map(docsSections.map(([title, dir]) => [title, { dir, items: [] }]))
  let current = null

  for (const line of lines) {
    const heading = line.match(/^## (.+)$/)
    if (heading) {
      current = sections.get(heading[1]) ?? null
      continue
    }
    if (!current) continue
    if (/^- \*\*\[/.test(line)) {
      add('docs/README.md catalogue entries should use plain links, not bold link labels')
      continue
    }
    const item = line.match(/^- \[([^\]]+)\]\(([^)]+)\)/)
    if (!item) continue
    current.items.push({ text: item[1], href: item[2] })
  }

  return sections
}

function listMarkdownFiles(relativeDir) {
  return fs.readdirSync(path.join(docsDir, relativeDir))
    .filter((name) => name.endsWith('.md'))
    .map((name) => `${relativeDir}/${name}`)
    .sort()
}

function walkMarkdownFiles(dir = docsDir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'out') continue
    const full = path.join(dir, entry.name)
    if (entry.isDirectory()) {
      walkMarkdownFiles(full, out)
    } else if (entry.name.endsWith('.md')) {
      out.push(path.relative(repoRoot, full).split(path.sep).join(path.posix.sep))
    }
  }
  return out.sort()
}

function checkVersionMirrors() {
  const versionCheck = read('ci/checks/repo-lint/version-check.sh')
  for (const manifest of ['website/package.json', 'kio-repl-wasm/Cargo.toml']) {
    if (!versionCheck.includes(manifest)) {
      add(`version-check.sh does not mirror ${manifest}`)
    }
  }
}

function checkLanguageCount() {
  const backends = backendIds()
  const sections = parseDocsCatalogue()
  const hostItems = sections.get('Host integrations').items
  const docsHostOrder = hostItems.map((item) => path.basename(item.href, '.md'))
  const readme = read('README.md')
  const countPattern = countSpellings(backends.length)
    .map((value) => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
    .join('|')
  if (!new RegExp(`(?:${countPattern}) host languages`, 'i').test(readme)) {
    add(`README.md does not mention the derived ${backends.length} host-language count`)
  }
  if (!readme.includes('docs/hosts/')) {
    add('README.md language-count sentence does not link to docs/hosts/')
  }

  const docsReadme = read('docs/README.md')
  for (const backend of backends) {
    if (!docsReadme.includes(`hosts/${backend}.md`)) {
      add(`docs/README.md Host integrations is missing hosts/${backend}.md`)
    }
  }
  if (new Set(docsHostOrder).size !== docsHostOrder.length) {
    add('docs/README.md Host integrations lists a host guide more than once')
  }
  if ([...docsHostOrder].sort().join(',') !== backends.join(',')) {
    add(`docs/README.md Host integrations order source must contain exactly these backends: ${backends.join(', ')}`)
  }

  if (exists('website/.vitepress/generated/languages.mjs')) {
    const generated = read('website/.vitepress/generated/languages.mjs')
    for (const backend of backends) {
      if (!generated.includes(`"id": "${backend}"`)) {
        add(`generated language data is missing backend ${backend}`)
      }
    }
    const generatedOrder = [...generated.matchAll(/"id": "([^"]+)"/g)].map((match) => match[1])
    if (generatedOrder.join(',') !== docsHostOrder.join(',')) {
      add('generated language data does not follow docs/README.md Host integrations order')
    }
    if (!generated.includes('export const languageCount = supportedLanguages.length')) {
      add('generated languageCount is not derived from supportedLanguages.length')
    }
  }

  const landing = read('website/.vitepress/theme/components/Landing.vue')
  if (!landing.includes('languageCount') || !landing.includes('supportedLanguages') || /Targets\s+6\s+host languages/.test(landing)) {
    add('landing page is not using generated languageCount data')
  }
  if (exists('website/targets.md')) {
    add('website should not generate a redundant top-level targets page')
  }
}

function checkGeneratedData() {
  for (const generated of [
    'website/.vitepress/generated/nav.mjs',
    'website/.vitepress/generated/languages.mjs',
    'website/.vitepress/generated/helloExample.mjs',
    'website/.vitepress/generated/kiodoc-html.mjs',
    'website/docs/index.md',
    'website/.vitepress/public/kio-favicon.png',
    'website/.vitepress/public/kio-logo-home.webp',
    'website/.vitepress/public/kio-nav-icon.png'
  ]) {
    if (!exists(generated)) {
      add(`${generated} is absent; run npm run prepare:docs`)
    }
  }
}

function checkNavigation() {
  if (!exists('website/.vitepress/generated/nav.mjs')) return
  const config = read('website/.vitepress/config.ts')
  if (/text:\s*['"]GitHub['"]/.test(config) && /icon:\s*['"]github['"]/.test(config)) {
    add('website nav has both a text GitHub link and the GitHub social icon')
  }
  if (!/logoLink:\s*base/.test(config)) {
    add('website title should link to the homepage under the configured base')
  }
  if (!/text:\s*['"]Home['"][\s\S]*?link:\s*['"]\/['"]/.test(config)) {
    add('website nav is missing an explicit Home link')
  }
  if (!/text:\s*['"]Docs['"][\s\S]*?link:\s*['"]\/docs\/['"]/.test(config)) {
    add('website nav is missing an explicit Docs link')
  }
  if (!/icon:\s*['"]github['"][\s\S]*?link:\s*['"]https:\/\/github\.com\/jdevuyst\/kio['"]/.test(config)) {
    add('website nav is missing the GitHub social icon')
  }
  if (/text:\s*['"]Languages['"]/.test(config)) {
    add('website top nav should stay to Home, Docs, and the GitHub icon')
  }

  const sections = parseDocsCatalogue()
  const navSource = read('website/.vitepress/generated/nav.mjs')
  for (const [, { dir }] of sections) {
    if (!exists(`website/docs/${dir}/index.md`)) {
      add(`website/docs/${dir}/index.md is missing; docs section routes should be addressable`)
    }
    if (!navSource.includes(JSON.stringify(`/docs/${dir}/`))) {
      add(`docs sidebar section ${dir} should link to /docs/${dir}/`)
    }
  }
  const hostNavOrder = [...navSource.matchAll(/"link": "\/docs\/hosts\/([^"]+)"/g)].map((match) => match[1])
  const expectedHostOrder = sections.get('Host integrations').items.map((item) => path.basename(item.href, '.md'))
  if (hostNavOrder.join(',') !== expectedHostOrder.join(',')) {
    add('docs sidebar Host integrations order does not follow docs/README.md')
  }
  const catalogued = new Set()

  for (const [, section] of sections) {
    for (const item of section.items) {
      catalogued.add(item.href)
      if (!navSource.includes(JSON.stringify(item.text)) || !navSource.includes(JSON.stringify(docsLink(item.href)))) {
        add(`generated nav is missing docs/README.md entry ${item.href}`)
      }
    }
  }

  for (const [, { dir }] of sections) {
    for (const file of listMarkdownFiles(dir)) {
      if (!catalogued.has(file)) {
        add(`${file} is not listed in docs/README.md`)
      }
    }
  }
}

function checkGitignore() {
  const gitignore = read('.gitignore')
  for (const pattern of [
    'website/node_modules/',
    'website/.vitepress/dist/',
    'website/.vitepress/cache/',
    'website/.vitepress/generated/',
    'website/.vitepress/public/wasm/',
    'kio-repl-wasm/target/'
  ]) {
    if (!gitignore.includes(pattern)) {
      add(`.gitignore does not cover ${pattern}`)
    }
  }
}

function checkDeployWorkflow() {
  if (!exists('.github/workflows/pages.yml')) {
    add('Pages workflow is missing')
    return
  }
  const workflow = read('.github/workflows/pages.yml')
  for (const expected of [
    'pages: write',
    'id-token: write',
    'persist-credentials: false',
    'npm run build',
    'website/.vitepress/dist',
    'actions/upload-pages-artifact@',
    'actions/deploy-pages@'
  ]) {
    if (!workflow.includes(expected)) {
      add(`Pages workflow is missing ${expected}`)
    }
  }
  if (!/uses: [^@\s]+@[0-9a-f]{40}/.test(workflow)) {
    add('Pages workflow does not appear to use pinned action SHAs')
  }
}

function checkE2E() {
  const script = 'ci/checks/orchestrators/website-e2e.sh'
  if (!exists(script)) {
    add('website-e2e.sh is missing')
    return
  }
  if (!executable(script)) {
    add('website-e2e.sh is not executable')
  }
  const source = read(script)
  for (const expected of ['--help', 'npm ci', 'npm run check:examples', 'npm run build', 'npm run audit', 'npm run smoke', 'wasm-pack']) {
    if (!source.includes(expected)) {
      add(`website-e2e.sh is missing ${expected}`)
    }
  }
}

function checkHelloExample() {
  const packageSource = read('website/examples/hello-world/hello.pkg.kio')
  const moduleSource = read('website/examples/hello-world/hello.kio')
  const landing = read('website/.vitepress/theme/components/Landing.vue')
  const packageJson = read('website/package.json')
  const checkScript = read('website/scripts/check-examples.sh')

  if (!packageSource.includes('target js') || !packageSource.includes('bridge')) {
    add('website hello-world package example should include a concrete target and bridge')
  }
  if (!moduleSource.includes('host fn print(str: String)') || !moduleSource.includes('pub fn main()')) {
    add('website hello-world module example does not contain the expected checked source')
  }
  if (!landing.includes('helloExampleFiles') || !landing.includes('highlightedHtml')) {
    add('landing page should render the generated hello-world example files')
  }
  if (!packageJson.includes('"check:examples"')) {
    add('website/package.json is missing the check:examples script')
  }
  for (const expected of ['"$KIO_BIN" check', '"$KIO_BIN" fmt', 'hello-world']) {
    if (!checkScript.includes(expected)) {
      add(`website example check script is missing ${expected}`)
    }
  }

  if (exists('website/.vitepress/generated/helloExample.mjs')) {
    const generated = read('website/.vitepress/generated/helloExample.mjs')
    for (const expected of ['hello.pkg.kio', 'hello.kio', 'highlightedHtml']) {
      if (!generated.includes(expected)) {
        add(`generated hello example data is missing ${expected}`)
      }
    }
    if (generated.indexOf('hello.kio') > generated.indexOf('hello.pkg.kio')) {
      add('homepage hello example should present hello.kio before hello.pkg.kio')
    }
  }
}

function checkReplLazyLoad() {
  const theme = read('website/.vitepress/theme/index.ts')
  const landing = read('website/.vitepress/theme/components/Landing.vue')
  const repl = read('website/.vitepress/theme/components/ReplIsland.vue')
  const worker = read('website/.vitepress/theme/replWorker.ts')
  if (/import\s+ReplIsland\s+from/.test(theme) || /app\.component\(['"]ReplIsland['"]/.test(theme)) {
    add('website theme entry should not eagerly register the REPL island')
  }
  if (theme.includes('@xterm/xterm/css/xterm.css')) {
    add('xterm CSS should load with the lazy REPL island, not the theme entry')
  }
  if (!landing.includes("import('./ReplIsland.vue')") || /import\s+ReplIsland\s+from/.test(landing)) {
    add('landing page should lazy-load the REPL island')
  }
  // `?raw`, not `?url`: a `?url` import emits xterm's CSS as a second CSS asset,
  // and VitePress links the first CSS asset in the Rollup output as the theme
  // stylesheet, so the real stylesheet is never linked and every page is unstyled.
  if (!repl.includes("@xterm/xterm/css/xterm.css?raw") || !repl.includes('ensureXtermCss')) {
    add('REPL island should inline xterm CSS from its lazy mount path')
  }
  if (!repl.includes('new Worker') || !repl.includes('replWorker.ts')) {
    add('REPL island should initialize the wasm REPL in a worker')
  }
  if (!worker.includes('@vite-ignore') || !worker.includes("type: 'init' | 'eval' | 'switchPoc'")) {
    add('REPL worker should runtime-import the public wasm module and own REPL calls')
  }
}

function checkTokenCss() {
  const css = read('website/.vitepress/theme/custom.css')
  const expected = expectedTokenClasses()
  for (const klass of expected) {
    if (!css.includes(`.${klass}`)) {
      add(`website CSS does not style .${klass}`)
    }
  }
  const expectedSet = new Set(expected)
  // Non-token `.kio-*` selectors are structural theme classes (the docs
  // home, the wasm-REPL terminal), not syntax-highlighting token colors,
  // so they are exempt from the token-parity check. Everything else under
  // the `.kio-*` prefix is expected to be a token class derived from
  // tokens.rs, so a stale/renamed one is flagged loud.
  const structural = (klass) => klass === 'kio-home' || klass.startsWith('kio-terminal')
  for (const match of css.matchAll(/\.(kio-[a-z0-9-]+)\s*\{/g)) {
    const klass = match[1]
    if (!structural(klass) && !expectedSet.has(klass)) {
      add(`website CSS styles .${klass}, which is not an expected Kio token class`)
    }
  }
}

function checkDocsLayout() {
  const config = read('website/.vitepress/config.ts')
  const css = read('website/.vitepress/theme/custom.css')
  const theme = read('website/.vitepress/theme/index.ts')
  const fmtColumns = formatterColumnBudget()
  if (!/aside:\s*false/.test(config)) {
    add('website docs layout should disable the empty right aside')
  }
  if (fmtColumns !== null && !new RegExp(`--kio-fmt-column-budget:\\s*${fmtColumns};`).test(css)) {
    add(`website docs code width is not aligned with kio fmt's ${fmtColumns}-column budget`)
  }
  if (!/--kio-doc-prose-width:\s*780px;/.test(css) || !/\.VPDoc \.vp-doc :where\(h1, h2, h3, h4, h5, h6, p, ul, ol, blockquote, dl\)/.test(css)) {
    add('website docs prose measure is not constrained separately from code width')
  }
  if (!/\.VPDoc\.has-aside \.content \.content-container/.test(css) || !/max-width:\s*min\(100%,\s*var\(--kio-page-code-width,\s*var\(--kio-doc-code-width\)\)\)/.test(css)) {
    add('website docs content width override is missing or too weak for VitePress scoped defaults')
  }
  for (const expected of ['updateCodeLaneWidth', '--kio-page-code-width', "querySelectorAll<HTMLPreElement>('pre')", 'document.fonts?.ready']) {
    if (!theme.includes(expected)) {
      add(`website theme is missing dynamic docs code-lane logic: ${expected}`)
    }
  }
}

function looksLikeImportOnlyKioSource(source) {
  const lines = source.split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
  return lines.length >= 2 && lines.every((line) => /^use\s+[\w\s{},]+from\s+[\w/]+;$/.test(line))
}

function checkKioFenceLanguages() {
  for (const file of walkMarkdownFiles()) {
    const lines = read(file).split(/\r?\n/)
    let inFence = false
    let info = ''
    let startLine = 0
    let body = []

    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index]
      const fenceStart = line.match(/^```(.*)$/)
      if (!inFence && fenceStart) {
        inFence = true
        info = fenceStart[1].trim()
        startLine = index + 1
        body = []
        continue
      }

      if (inFence && line.startsWith('```')) {
        const language = info.split(/\s+/, 1)[0]
        if ((language === '' || language === 'text') && looksLikeImportOnlyKioSource(body.join('\n'))) {
          add(`${file}:${startLine} has import-only Kio code in a ${language || 'plain'} fence; use a kio fence`)
        }
        inFence = false
        continue
      }

      if (inFence) {
        body.push(line)
      }
    }
  }
}

function checkGeneratedDocsClean() {
  const generated = 'website/.vitepress/generated/kiodoc-html.mjs'
  if (!exists(generated)) return
  const html = read(generated)
  if (/<p>\s*&lt;!--/.test(html)) {
    add('generated Kiodoc HTML contains rendered HTML comments; run npm run prepare:docs')
  }
  if (/<p>\s*\|[\s\S]*?\|\s+\|\s*:?-+/.test(html)) {
    add('generated Kiodoc HTML contains a rendered Markdown table paragraph; run npm run prepare:docs')
  }
}

function checkBuiltOutputIfPresent() {
  const dist = path.join(repoRoot, 'website', '.vitepress', 'dist')
  if (!fs.existsSync(dist)) return
  const structuralSums = path.join(dist, 'docs', 'guides', 'sums.html')
  if (!fs.existsSync(structuralSums) || !readText(structuralSums).includes('kio-keyword')) {
    add('built sums page is missing Kio token spans')
  }
  const kiodoc = path.join(dist, 'docs', 'guides', 'kiodoc.html')
  if (!fs.existsSync(kiodoc) || !readText(kiodoc).includes('Call [`render`] after [`parse`].')) {
    add('built Kiodoc page is missing literal fenced reference brackets')
  }

  // VitePress links whichever CSS asset it finds first in the Rollup output as
  // the theme stylesheet, so any extra emitted CSS asset silently displaces the
  // real one and ships the whole site unstyled.
  const styles = fs
    .readdirSync(path.join(dist, 'assets'))
    .filter((file) => file.endsWith('.css'))
  const themeStyle = styles.find((file) => /^style\..*\.css$/.test(file))
  if (styles.length !== 1 || !themeStyle) {
    add(`built assets/ should hold exactly one CSS bundle, found: ${styles.join(', ') || 'none'}`)
  }

  for (const htmlFile of fs.readdirSync(dist, { recursive: true })) {
    if (!htmlFile.endsWith('.html')) continue
    const html = readText(path.join(dist, htmlFile))
    if (!themeStyle || !html.includes(`assets/${themeStyle}`)) {
      add(`built ${htmlFile} does not link the theme stylesheet`)
    }
    if (/<p>\s*&lt;!--/.test(html)) {
      add(`built ${htmlFile} contains a rendered HTML comment`)
    }
    if (/<p>\s*\|[\s\S]*?\|\s+\|\s*:?-+/.test(html)) {
      add(`built ${htmlFile} contains a rendered Markdown table paragraph`)
    }
  }
}

function readText(file) {
  return fs.readFileSync(file, 'utf8')
}

checkVersionMirrors()
checkLanguageCount()
checkGeneratedData()
checkNavigation()
checkGitignore()
checkDeployWorkflow()
checkE2E()
checkHelloExample()
checkReplLazyLoad()
checkTokenCss()
checkDocsLayout()
checkKioFenceLanguages()
checkGeneratedDocsClean()
checkBuiltOutputIfPresent()

if (findings.length > 0) {
  for (const finding of findings) {
    console.error(`audit-website: ${finding}`)
  }
  process.exit(1)
}

console.log('audit-website: no findings')
