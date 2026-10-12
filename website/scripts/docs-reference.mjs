const entries = [
  { text: 'Language spec', source: 'specs/language.md', kind: 'blob' },
  { text: 'CLI spec', source: 'specs/cli.md', kind: 'blob' },
  { text: 'Package spec', source: 'specs/package.md', kind: 'blob' },
  { text: 'Backend contracts', source: 'specs/backends', kind: 'tree' }
]

export function docsReference(revision = 'main', include = () => true) {
  return {
    text: 'Reference',
    items: entries.filter(include).map(({ text, source, kind }) => ({
      text, link: `https://github.com/jdevuyst/kio/${kind}/${revision}/${source}`
    }))
  }
}
