import assert from 'node:assert/strict'
import fs from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { capturePage, openBuiltPage, withBuiltSite } from './browser-site.mjs'
import { historyRoute } from './historical-docs.mjs'
import { assertHistoryFixture, buildHistoryFixture } from './fixtures/history-site.mjs'

export async function checkHistoricalBrowser({ output } = {}) {
  const fixture = await buildHistoryFixture()
  try {
    assertHistoryFixture(fixture)
    return await withBuiltSite({ dist: fixture.dist, output }, async ({ page, origin, runDir, report }) => {
      const route = name => `/kio${historyRoute(fixture.first, `docs/guides/${name}.md`)}`
      const waitBody = text => page.waitForFunction(expected => document.querySelector('.kiodoc-import')?.textContent.includes(expected), text)
      const notice = () => page.locator('.history-content-notice').textContent()
      const assertCurrentPage = async () => {
        assert.equal(await page.evaluate(() => location.pathname), '/kio/docs/guides/changed.html')
        assert.equal(await page.locator('.history-banner, .history-version-tabs, .history-provenance').count(), 0)
        assert.equal(await page.locator('.VPSidebar a[href="/kio/docs/guides/changed.html"]').count(), 1)
        assert.equal(await page.locator('.source-footer').textContent(), 'View source on GitHub')
        assert.equal(await page.locator('.source-footer a').getAttribute('href'), 'https://github.com/jdevuyst/kio/blob/main/docs/guides/changed.md')
      }
      const requests = []
      page.on('request', request => { if (/github\.com|githubusercontent\.com/.test(request.url())) requests.push(request.url()) })
      await openBuiltPage(page, `${origin}/kio/blog/fixture.html`)
      assert.equal(await page.locator('.source-footer').textContent(), 'View source on GitHub')
      assert.equal(await page.locator('.source-footer a').getAttribute('href'), 'https://github.com/jdevuyst/kio/blob/main/docs/blog/fixture.md')
      assert.equal(await page.locator('.blog-post-footer').evaluate(node => getComputedStyle(node).borderTopWidth), '1px')
      assert.equal(await page.locator('.source-footer').evaluate(node => getComputedStyle(node).borderTopWidth), '0px')
      await page.locator(`.kiodoc-import a[href="${route('changed')}#stable-anchor"]`).click()
      await waitBody('Historical documentation text.')
      assert.equal(await notice(), 'You’re reading documentation for Kio 0.1.0. This page has changed since then.')
      assert.equal(await page.locator('.history-version-tabs').count(), 0)
      assert.equal(await page.locator('.source-footer').textContent(), 'View source on GitHub')
      assert.equal(await page.locator('.source-footer a').getAttribute('href'), `https://github.com/jdevuyst/kio/blob/${fixture.first}/docs/guides/changed.md`)
      assert.equal(await page.locator('.history-banner .source-footer').count(), 0)
      assert.equal(await page.locator('.source-footer').evaluate(node => getComputedStyle(node).borderTopWidth), '1px')
      assert.equal(await page.locator('.VPSidebar').getByText('Source documentation').count(), 0)
      assert.equal(await page.locator(`.VPSidebar a[href="https://github.com/jdevuyst/kio/blob/${fixture.first}/specs/language.md"]`).count(), 1)
      assert.equal(await page.locator('.VPSidebar').getByText('CLI spec').count(), 0)
      assert.equal(await page.locator('.history-context').count(), 0)
      await page.waitForFunction(() => document.querySelector('.history-current-action a')?.getAttribute('href') === '/kio/docs/guides/changed.html#stable-anchor')
      assert.equal(await page.locator('.history-current-action a').textContent(), 'View current page')
      assert.equal(await page.locator('.VPSidebar a[href="/kio/docs/guides/changed.html"]').count(), 0)
      await page.waitForFunction(() => [...document.querySelectorAll('.kiodoc-import img')].every(image => image.complete && image.naturalWidth > 0))
      assert.deepEqual(await page.evaluate(() => [globalThis.historyScriptExecuted, globalThis.historySvgExecuted, globalThis.historyInterpolationExecuted, globalThis.historyFrontmatterExecuted].map(value => value ?? null)), [null, null, null, null])
      await page.locator('.history-current-action a').click()
      await waitBody('Current documentation text.')
      await assertCurrentPage()
      assert.equal(await page.evaluate(() => location.hash), '#stable-anchor')
      assert.equal(await page.locator('.kiodoc-import a').first().getAttribute('href'), './related.html#related-heading')
      await page.locator('.kiodoc-import a').first().click()
      await waitBody('Second neighboring article.')
      assert.equal(await page.evaluate(() => location.pathname), '/kio/docs/guides/related.html')
      await page.goBack()
      await waitBody('Current documentation text.')
      await assertCurrentPage()
      await page.goBack()
      await waitBody('Historical documentation text.')
      assert.equal(await page.locator('.source-footer').textContent(), 'View source on GitHub')
      await page.locator('.kiodoc-import a').first().click()
      await waitBody('Original neighboring article.')
      assert.equal(await page.evaluate(() => location.pathname), route('related'))
      assert.equal(await page.evaluate(() => location.hash), '#related-heading')
      await openBuiltPage(page, origin + route('equal'))
      assert.equal(await notice(), 'You’re reading documentation for Kio 0.1.0. This page matches the current documentation.')
      assert.equal(await page.locator('.history-current-action a').getAttribute('href'), '/kio/docs/guides/equal.html')
      await page.locator('.history-current-action a').click()
      await waitBody('One ordinary sentence.')
      assert.equal(await page.evaluate(() => location.pathname), '/kio/docs/guides/equal.html')
      assert.equal(await page.locator('.history-banner').count(), 0)
      await page.goBack()
      await page.waitForSelector('.history-banner')
      await openBuiltPage(page, origin + route('deleted'))
      assert.equal(await notice(), 'You’re reading documentation for Kio 0.1.0. This page has no counterpart in the current documentation.')
      assert.equal(await page.locator('.history-current-action a').textContent(), 'Browse current docs')
      assert.equal(await page.locator('.history-current-action a').getAttribute('href'), '/kio/docs/')
      assert.equal(await page.locator('.source-footer a').getAttribute('href'), `https://github.com/jdevuyst/kio/blob/${fixture.first}/docs/guides/deleted.md`)
      assert.equal(await page.locator('.VPNavBarMenuLink[href="/kio/docs/"]').count(), 1)
      await page.locator('.history-current-action a').click()
      await waitBody('Current docs')
      assert.equal(await page.locator('.history-banner').count(), 0)
      await page.goBack()
      await waitBody('Historical-only body.')
      await openBuiltPage(page, origin + route('frontmatter'))
      assert.equal(await page.evaluate(() => globalThis.historyFrontmatterExecuted ?? null), null)
      await openBuiltPage(page, `${origin}/kio${historyRoute(fixture.between, 'docs/guides/related.md')}`)
      assert.equal(await notice(), `You’re reading documentation at commit ${fixture.between.slice(0, 8)}. This page matches the current documentation.`)
      for (const [name, article] of [['changed', 'changed'], ['equal', 'equal'], ['missing', 'deleted']]) {
        if (output) {
          const directory = path.join(runDir, name)
          await fs.mkdir(directory)
          await capturePage(page, origin + route(article), directory, { fullPage: true })
        } else {
          await page.setViewportSize({ width: 390, height: 844 })
          await openBuiltPage(page, origin + route(article))
          await page.waitForFunction(() => [...document.images].every(image => image.complete && image.naturalWidth > 0))
        }
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true, 'historical banner overflows the narrow viewport')
      }
      if (output) {
        for (const [name, url] of [['current', '/kio/docs/guides/equal.html'], ['blog', '/kio/blog/fixture.html']]) {
          const directory = path.join(runDir, name)
          await fs.mkdir(directory)
          await capturePage(page, origin + url, directory, { fullPage: true })
        }
      }
      assert.deepEqual(requests, [], 'historical views must not request GitHub at browser time')
      report.history = { revisions: [fixture.first, fixture.second], deployedRevision: fixture.current, pages: fixture.pages.size,
        checks: ['static routes and search exclusion', 'pinned blog navigation', 'direct current links and anchors', 'exact release label', 'untagged commit provenance', 'ordinary current pages', 'pinned historical source footer', 'current docs and blog source footers on main', 'synthetic indexes omit source footers', 'current relative links', 'browser back', 'same-revision links', 'equal content', 'missing counterpart with current docs action', 'site Docs navigation', 'inert Markdown and SVG', 'local images', 'narrow viewport layout', ...(output ? ['desktop and narrow captures for all banner states and current docs/blog footers'] : [])] }
    })
  } finally {
    fixture.cleanup()
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  const args = process.argv.slice(2)
  if (args.some(argument => !argument.startsWith('--output=')) || args.length > 1) throw new Error('Usage: node scripts/history-browser.mjs [--output=NEW_DIRECTORY]')
  await checkHistoricalBrowser({ output: args[0]?.slice('--output='.length) })
}
