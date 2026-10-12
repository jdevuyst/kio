#!/usr/bin/env node
import path from 'node:path'
import { parseArgs } from 'node:util'
import { capturePage, defaultDist, siteBase, smokeRepl, withBuiltSite } from './browser-site.mjs'

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: { dist: { type: 'string' }, output: { type: 'string' }, page: { type: 'string' }, 'full-page': { type: 'boolean' }, help: { type: 'boolean', short: 'h' } }
})
if (values.help) {
  console.log('Usage: npm run browser:smoke -- [--dist DIR] [--output NEW_DIR]\n       npm run browser:capture -- --output NEW_DIR [--dist DIR] [--page /kio/route.html#anchor] [--full-page]\n\nReuses a built site and mise-managed Firefox with its normal content sandbox.\nPlatforms: Linux x64/arm64 (Ubuntu22-compatible GTK3 runtime), macOS14+ x64/arm64, Windows11+ x64.\nCapture writes desktop/narrow rendered viewport screenshots for human visual review; it is not a visual assertion.')
} else {
  const [mode] = positionals
  if (positionals.length !== 1 || !['smoke', 'capture'].includes(mode)) throw new Error('expected smoke or capture; use --help')
  if (mode === 'capture' && !values.output) throw new Error('capture requires --output NEW_DIR')
  if (mode === 'smoke' && (values.page || values['full-page'])) throw new Error('--page and --full-page apply only to capture')
  const route = values.page ?? siteBase
  if (!route.startsWith(siteBase) || route.includes('..') || route.includes('\\')) throw new Error('--page must be a local /kio/ route')
  await withBuiltSite({ dist: path.resolve(values.dist ?? defaultDist), output: values.output }, async ({ page, origin, runDir }) => {
    if (mode === 'smoke') await smokeRepl(page, origin)
    else await capturePage(page, origin + route, runDir, { fullPage: values['full-page'] })
  })
}
