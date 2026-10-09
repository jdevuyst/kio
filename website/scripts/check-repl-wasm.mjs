import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

const bundle = new URL('../.vitepress/public/wasm/', import.meta.url)
const { default: initialize, Repl } = await import(new URL('kio_repl_wasm.js', bundle))
await initialize({ module_or_path: readFileSync(new URL('kio_repl_wasm_bg.wasm', bundle)) })

const clean = value => value.replace(/\x1b\[[0-9;]*m/g, '').trim()
const output = turn => {
  assert.deepEqual(Object.keys(turn).sort(), ['is_error', 'keep_running', 'output'])
  assert.equal(turn.keep_running, true)
  assert.equal(turn.is_error, false)
  assert.equal(typeof turn.output, 'string')
  return clean(turn.output)
}

const repl = new Repl()
try {
  assert.equal(repl.current_poc(), 'list')
  assert.ok(repl.pocs().some(poc => poc.id === 'list'))
  const banner = output(repl.init_banner())
  assert.ok(banner.includes('fn filter'), banner)
  assert.ok(banner.includes('if!'), banner)
  const source = output(repl.eval(':source filter'))
  assert.ok(source.includes('fn filter'), source)
  assert.ok(source.includes('if!'), source)
  assert.equal(output(repl.eval(':normalize ()')), '()')

  for (const prefix of ['', ':t ', ':pure ', ':normalize ']) {
    const diagnostic = output(repl.eval(`${prefix}index([1], 0)`))
    assert.ok(diagnostic.includes('not a name or a Kio expression'), diagnostic)
    assert.ok(diagnostic.includes('<repl>:1:8: error: expected identifier'), diagnostic)
    assert.match(diagnostic, /1 \| index\(\[1\], 0\)/)
    assert.match(diagnostic, /\| {8}\^/)
    assert.ok(!diagnostic.includes('_expr') && !diagnostic.includes('_repl_parse_check_'), diagnostic)
  }
  assert.equal(output(repl.eval(':t index([* 1 *], 0)')), 'index([* 1 *], 0) : list.I32 | .')

  const completion = repl.complete('fi', 2)
  assert.deepEqual(Object.keys(completion).sort(), ['candidates', 'replaceEnd', 'replaceStart'])
  assert.equal(completion.replaceStart, 0)
  assert.equal(completion.replaceEnd, 2)
  assert.ok(completion.candidates.some(candidate => candidate.label === 'filter'))
  const continuationSource = 'if! .t { () } el'
  const continuation = repl.complete(continuationSource, continuationSource.length)
  assert.equal(continuation.replaceStart, continuationSource.length - 2)
  assert.equal(continuation.replaceEnd, continuationSource.length)
  assert.deepEqual(continuation.candidates.map(candidate => candidate.label), ['else'])
  assert.equal(continuation.candidates[0].kind, 'keyword')
  const listing = repl.eval(':ls list').output
  for (const keyword of ['op', 'varop']) {
    assert.ok(listing.includes(`\x1b[38;2;229;192;123mpub\x1b[0m \x1b[38;2;229;192;123m${keyword}\x1b[0m`), listing)
  }
  for (const [input, expected] of [
    [':sig list.(op _ ++', 'pub op _ ++ _ { impl append }'],
    [':sig list.(varop', 'pub varop [* *] { foldr prepend empty }'],
  ]) {
    const result = repl.complete(input, input.length)
    assert.equal(result.replaceStart, 5)
    assert.equal(result.replaceEnd, input.length)
    const selected = result.candidates.find(item => item.kind === 'op' && item.label.startsWith(input.slice(5)))
    assert.ok(selected?.label.endsWith(')'), JSON.stringify(result))
    const inserted = input.slice(0, result.replaceStart) + selected.label + input.slice(result.replaceEnd)
    assert.equal(output(repl.eval(inserted)), expected)
  }
  console.log('Public REPL WASM startup, source, normalization and completion passed.')
} finally {
  repl.free()
}
