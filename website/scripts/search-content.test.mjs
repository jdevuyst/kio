import assert from 'node:assert/strict'
import test from 'node:test'
import { createMarkdownRenderer } from 'vitepress'
import { createSearchRenderer, headingIds, searchHtml, withHeadingIds } from '../.vitepress/search-content.mjs'

const md = await createMarkdownRenderer(new URL('..', import.meta.url).pathname, {}, '/kio/')

test('Kiodoc headings use VitePress IDs, duplicate suffixes and explicit targets', async () => {
  const html = '<h1>Kio <code>fn</code> &amp; types</h1><h2>Same</h2><h2>Same</h2><h2 class="custom" id="kept">Named</h2>'
  const prepared = await withHeadingIds(html, md)
  assert.equal(prepared, '<h1 id="kio-fn-types">Kio <code>fn</code> &amp; types</h1><h2 id="same">Same</h2><h2 id="same-1">Same</h2><h2 class="custom" id="kept">Named</h2>')
  const indexed = searchHtml(prepared)
  for (const id of ['kio-fn-types', 'same', 'same-1', 'kept']) {
    assert.ok(prepared.includes(`id="${id}"`))
    assert.ok(indexed.includes(`href="#${id}"`))
  }
  assert.match(indexed, /<h1>Kio fn &amp; types<a/)
})

test('code, raw text elements and comments do not become heading targets', async () => {
  const protectedHtml = '<pre><code><h2>Fake</h2>\n# Example</code></pre><!-- <h1>Comment</h1> --><script>"<h3>Script</h3>"</script><code>&lt;h2&gt;Escaped&lt;/h2&gt;</code>'
  assert.equal(await withHeadingIds(protectedHtml, md), protectedHtml)
  assert.equal(searchHtml(protectedHtml), protectedHtml)
})

test('later explicit IDs are reserved and single-quoted attributes survive', async () => {
  const html = '<h2>Kept</h2><h2 id=\'kept\'>Named <code>`value`</code></h2><h2>Kept</h2>'
  const prepared = await withHeadingIds(html, md)
  assert.equal(prepared, '<h2 id="kept-1">Kept</h2><h2 id=\'kept\'>Named <code>`value`</code></h2><h2 id="kept-2">Kept</h2>')
  assert.match(searchHtml(prepared), /href="#kept"/)
})

test('raw HTML unquoted IDs survive and data-id is not a heading ID', async () => {
  const html = '<h2 data-id="not-an-id">Automatic</h2><h2 id = kept>Raw heading</h2>'
  const prepared = await withHeadingIds(html, md)
  assert.equal(prepared, '<h2 data-id="not-an-id" id="automatic">Automatic</h2><h2 id = kept>Raw heading</h2>')
  assert.match(searchHtml(prepared), /href="#kept"/)
})

test('component pages supply docs and blog content; ordinary Markdown stays searchable', async () => {
  const docs = await withHeadingIds('<h1>Guide</h1><p>Recursion</p><h2>Nested types</h2><p>Body</p>', md)
  const blog = await withHeadingIds('<h1>Introducing Kio</h1><p>Hosted language</p>', md)
  const render = createSearchRenderer({ 'guides/example': docs, 'blog/example': blog })
  for (const key of ['guides/example', 'blog/example']) {
    const indexed = await render(`<KiodocPage page=${JSON.stringify(key)} />`, {}, md)
    assert.equal(indexed, searchHtml(key.startsWith('blog/') ? blog : docs))
  }
  assert.match(await render('# Branding\n\nAssets', {}, md), /href="#branding"/)
  assert.equal(await render('---\nsearch: false\n---\n<KiodocPage page="guides/example" />', {}, md), '')
  await assert.rejects(render('<KiodocPage page="missing" />', {}, md), /missing/)
})

test('quoted attributes are parsed as HTML, not heading delimiters or IDs', async () => {
  const html = '<h2 title="a > b" data-x=" id=pretend">Visible</h2>'
  assert.equal(await withHeadingIds(html, md), '<h2 title="a > b" data-x=" id=pretend" id="visible">Visible</h2>')
})

test('all existing IDs are reserved; empty and literal-template headings remain valid', async () => {
  const html = '<div id="same"></div><h2>Same</h2><h1></h1><h2>!!!</h2><h2>{{literal}}</h2>'
  assert.deepEqual([...headingIds(await withHeadingIds(html, md))], ['same-1', 'section', 'section-1', 'literal'])
})

test('entity IDs are decoded as DOM targets and encoded as URL fragments', async () => {
  const html = '<h2 id="a&amp;b#c%">Visible</h2>'
  assert.equal(await withHeadingIds(html, md), html)
  assert.deepEqual([...headingIds(html)], ['a&b#c%'])
  assert.match(searchHtml(html), /href="#a%26b%23c%25"/)
})

test('multiline raw headings become single-line search sections without changing display HTML', async () => {
  const html = '<h2 title="a > b">Multi\n<code>line</code></h2><p>Body</p>'
  const prepared = await withHeadingIds(html, md)
  assert.equal(prepared, '<h2 title="a > b" id="multi-line">Multi\n<code>line</code></h2><p>Body</p>')
  assert.equal(searchHtml(prepared), '<h2>Multi line<a href="#multi-line" aria-hidden="true"></a></h2><p>Body</p>')
})

test('blank visible headings retain their display bytes and gain searchable section titles', async () => {
  const prepared = await withHeadingIds('<h1></h1><p>Empty section indexing witness.</p>', md)
  assert.equal(prepared, '<h1 id="section"></h1><p>Empty section indexing witness.</p>')
  assert.equal(searchHtml(prepared), '<h1>Section<a href="#section" aria-hidden="true"></a></h1><p>Empty section indexing witness.</p>')
})
