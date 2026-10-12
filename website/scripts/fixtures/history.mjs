import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

export function historyFixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'website-history-fixture.'))
  const git = (...args) => {
    const result = spawnSync('git', ['-c', 'core.hooksPath=', '-c', 'commit.gpgSign=false', '-c', 'tag.gpgSign=false', '-C', root, ...args], {
      encoding: 'utf8', env: { ...process.env, GIT_AUTHOR_DATE: '2020-01-01T12:00:00Z', GIT_COMMITTER_DATE: '2020-01-01T12:00:00Z' }
    })
    if (result.error || result.status !== 0) throw new Error(result.error?.message ?? result.stderr)
    return result.stdout.trim()
  }
  const write = (name, text) => {
    const file = path.join(root, name)
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, text)
  }
  const commit = message => { git('add', '.'); git('commit', '--quiet', '-m', message); return git('rev-parse', 'HEAD') }
  git('-c', 'init.templateDir=', 'init', '--quiet')
  git('config', 'user.name', 'Website fixture')
  git('config', 'user.email', 'website-fixture@example.invalid')
  const equal = '# Equal article\n\nOne ordinary sentence.\n\n<!--kio {harness=old}\nhidden_old()\n-->\n\n```kio {@old check_exit_code=14}\nold_spelling()\n```\n'
  const changed = '# Changed article\n\nHistorical documentation text.\n\n## Stable anchor\n\n[Related](related.md#related-heading), [old docs](../README.md), [host sources](../hosts/), and [spec](../../specs/language.md#types).\n\n![Diagram](../assets/diagram.png)\n\n![Vector](../assets/vector.svg)\n\n```kio {ignore}\nold_spelling(1,  2)\n```\n\n<script>globalThis.historyScriptExecuted = true</script>\n\n<iframe src="javascript:alert(1)"></iframe>\n\n[Unsafe](javascript:alert(1))\n\n<<< ../../secret.txt\n\n{{ globalThis.historyInterpolationExecuted = true }}\n'
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a7QAAAABJRU5ErkJggg==', 'base64')
  write('docs/README.md', '# Archived docs\n\n[Changed](guides/changed.md)\n')
  write('docs/blog/fixture.md', '# Fixture post\n')
  write('specs/language.md', '# Fixture language specification\n')
  write('docs/guides/changed.md', changed)
  write('docs/guides/equal.md', equal)
  write('docs/guides/deleted.md', '# Removed article\n\nHistorical-only body.\n')
  write('docs/guides/related.md', '# Related article\n\n## Related heading\n\nOriginal neighboring article.\n')
  write('docs/guides/image-only.md', '# Image article\n\n![Diagram](../assets/diagram.png)\n')
  write('docs/guides/code-space.md', '# Code article\n\n```text\none  two\n```\n')
  write('docs/guides/frontmatter.md', '---js\n(() => { globalThis.historyFrontmatterExecuted = true; return {} })()\n---\n# Frontmatter article\n\nHistorical metadata is inert.\n')
  write('docs/assets/diagram.png', png)
  write('docs/assets/vector.svg', '<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 10 10"><script>parent.historySvgExecuted=true</script><circle cx="5" cy="5" r="4"/></svg>')
  write('historical-build-script.js', 'throw new Error("Old scripts must remain data")')
  write('secret.txt', 'THIS_INCLUDE_MUST_NOT_APPEAR')
  const assetRevision = commit('Historical asset objects')
  write('docs/guides/changed.md', `${changed}\n![Pinned raw diagram](https://github.com/jdevuyst/kio/blob/${assetRevision}/docs/assets/diagram.png?raw=1)\n`)
  const first = commit('First historical documentation')
  git('tag', 'releases/v0.1.0', first)
  git('tag', 'v0.1.0', first)
  git('tag', 'unrelated-name', first)
  write('docs/guides/related.md', '# Related article\n\n## Related heading\n\nSecond neighboring article.\n')
  const second = commit('Second historical documentation')
  git('tag', '-a', '-m', 'Exact release tag fixture', 'releases/v0.1.1', second)
  git('commit', '--quiet', '--allow-empty', '-m', 'Between releases')
  const between = git('rev-parse', 'HEAD')
  write('docs/guides/changed.md', changed.replace('Historical documentation text.', 'Current documentation text.'))
  write('docs/guides/equal.md', equal.replace('One ordinary sentence.', 'One  ordinary\nsentence.').replace('harness=old', 'harness=new').replace('hidden_old()', 'hidden_new()').replace('@old check_exit_code=14', '@new check_exit_code=0'))
  write('docs/guides/code-space.md', '# Code article\n\n```text\none two\n```\n')
  write('docs/assets/diagram.png', Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64'))
  fs.unlinkSync(path.join(root, 'docs/guides/deleted.md'))
  const current = commit('Current documentation')
  git('tag', 'releases/v0.2.0', current)
  const currentPages = new Map([
    ['index', '<div class="kiodoc-page"><h1>Current docs</h1><a href="./guides/changed.html">Changed</a></div>'],
    ['guides/changed', '<div class="kiodoc-page"><h1>Changed article</h1><p>Current documentation text.</p><h2>Stable anchor</h2><p><a href="./related.html#related-heading">Related current</a>, <a href="../hosts/">Hosts current</a>.</p><p><img src="../assets/diagram.png" alt="Current diagram"></p><pre><code class="language-kio"><span class="kio-ref">old_spelling</span>(1,  2)\n</code></pre></div>'],
    ['guides/equal', '<div class="kiodoc-page"><h1>Equal article</h1><p>One ordinary sentence.</p><pre><code>old_spelling()</code></pre></div>'],
    ['guides/related', '<div class="kiodoc-page"><h1>Related article</h1><h2>Related heading</h2><p>Second neighboring article.</p></div>'],
    ['guides/image-only', '<div class="kiodoc-page"><h1>Image article</h1><img src="../assets/diagram.png" alt="Diagram"></div>'],
    ['guides/code-space', '<div class="kiodoc-page"><h1>Code article</h1><pre><code>one two</code></pre></div>'],
    ['guides/frontmatter', '<div class="kiodoc-page"><h1>Frontmatter article</h1><p>Historical metadata is inert.</p></div>']
  ])
  const url = (revision, name) => `https://github.com/jdevuyst/kio/blob/${revision}/docs/guides/${name}.md`
  const blogPages = new Map([['blog/fixture.html', `<h1>Fixture post</h1>${['changed', 'equal', 'deleted', 'image-only', 'code-space', 'frontmatter'].map(name => `<a href="${url(first, name)}#stable-anchor">${name}</a>`).join('')}<a href="${url(second, 'related')}">second revision</a><a href="${url(first, 'changed')}">duplicate reference</a><a href="https://github.com/jdevuyst/kio/blob/${first}/specs/language.md">specs stay external</a>`]])
  return { root, first, second, between, current, blogPages, currentPages, git, write, cleanup: () => fs.rmSync(root, { recursive: true, force: true }) }
}
