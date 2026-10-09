import { parse } from 'vue/compiler-sfc'

// Parse HTML with Vue, then insert attributes without reserializing authored HTML.
function elements(html) {
  const prefix = '<template>'
  const { descriptor } = parse(`${prefix}${html}</template>`, {
    templateParseOptions: { delimiters: ['\u0000{{', '}}\u0000'] }
  })
  const all = []
  const headings = []
  function visit(node, protectedContent = false) {
    if (node.type !== 1) return
    all.push(node)
    const protectedHere = protectedContent || /^(pre|code|script|style|textarea)$/i.test(node.tag)
    if (!protectedHere && /^h[1-6]$/i.test(node.tag)) headings.push(node)
    for (const child of node.children) visit(child, protectedHere)
  }
  for (const node of descriptor.template?.ast.children ?? []) visit(node)
  return { all, headings, offset: prefix.length }
}

function idOf(node) {
  return node.props.find((prop) => prop.type === 6 && prop.name.toLowerCase() === 'id')?.value?.content
}

function textOf(node) {
  if (node.type === 2) return node.content
  return (node.children ?? []).map(textOf).join('')
}

function insert(html, edits) {
  for (const [offset, text, length = 0] of edits.sort((a, b) => b[0] - a[0])) {
    html = html.slice(0, offset) + text + html.slice(offset + length)
  }
  return html
}

export function headingIds(html) {
  return new Set(elements(html).headings.map(idOf).filter((id) => id !== undefined))
}

export async function withHeadingIds(html, md) {
  const { all, headings, offset } = elements(html)
  const used = new Set(all.map(idOf).filter((id) => id !== undefined))
  const edits = []
  for (const node of headings) {
    if (idOf(node) !== undefined) continue
    const text = textOf(node).replace(/\r?\n/g, ' ').trim()
    const fence = '`'.repeat(Math.max(0, ...[...text.matchAll(/`+/g)].map((match) => match[0].length)) + 1)
    // VitePress derives the slug; collision allocation also reserves IDs on
    // nonheading elements. Empty slugs receive a usable section target.
    const rendered = await md.renderAsync(`${'#'.repeat(Number(node.tag[1]))} ${fence} ${text} ${fence}`)
    const base = [...headingIds(rendered)][0] || 'section'
    let id = base
    for (let suffix = 1; used.has(id); suffix++) id = `${base}-${suffix}`
    used.add(id)
    const afterAttributes = node.props.at(-1)?.loc.end.offset ?? node.loc.start.offset + node.tag.length + 1
    const end = html.indexOf('>', afterAttributes - offset)
    edits.push([end, ` id="${md.utils.escapeHtml(id)}"`])
  }
  return insert(html, edits)
}

export function searchHtml(html) {
  const { headings, offset } = elements(html)
  return insert(html, headings.map((node) => {
    const id = idOf(node)
    if (id === undefined) throw new Error('Kiodoc search heading has no target ID')
    // VitePress's section splitter expects a single-line heading. Search uses
    // its visible text, independent of authored attributes and inline markup.
    const text = textOf(node).trim() || 'Section'
    const title = text.replace(/\r?\n/g, ' ').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    return [node.loc.start.offset - offset,
      `<${node.tag}>${title}<a href="#${encodeURIComponent(id)}" aria-hidden="true"></a></${node.tag}>`,
      node.loc.source.length]
  }))
}

export function createSearchRenderer(pages) {
  return async (source, env, md) => {
    const rendered = await md.renderAsync(source, env)
    if (env.frontmatter?.search === false) return ''
    const page = source.match(/<KiodocPage\s+page=("(?:[^"\\]|\\.)*")\s*\/>/)
    if (!page) return rendered
    const key = JSON.parse(page[1])
    if (!(key in pages)) throw new Error(`Kiodoc search content is missing for ${key}`)
    return searchHtml(pages[key])
  }
}
