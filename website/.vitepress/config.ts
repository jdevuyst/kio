import { defineConfig } from 'vitepress'
import { docsSidebar } from './generated/nav.mjs'
import { siteUrl } from './site.mjs'
import { fileURLToPath } from 'node:url'

const publicDir = fileURLToPath(new URL('./public', import.meta.url))
const base = '/kio/'

export default defineConfig({
  base,
  title: 'Kio',
  description: 'An ultra-portable, embeddable programming language that compiles to host languages.',
  head: [
    ['link', { rel: 'icon', href: `${base}kio-favicon.png` }],
    ['link', { rel: 'alternate', type: 'application/rss+xml', title: 'Kio Blog', href: `${siteUrl}/blog/feed.xml` }]
  ],
  cleanUrls: false,
  lastUpdated: true,
  themeConfig: {
    logo: '/kio-nav-icon.png',
    logoLink: base,
    siteTitle: 'Kio',
    nav: [
      { text: 'Home', link: '/' },
      { text: 'Docs', link: '/docs/' },
      { text: 'Blog', link: '/blog/' }
    ],
    sidebar: {
      '/docs/': docsSidebar
    },
    aside: false,
    socialLinks: [
      { icon: 'github', link: 'https://github.com/jdevuyst/kio' }
    ],
    search: {
      provider: 'local'
    },
    outline: {
      level: [2, 3]
    },
    lastUpdated: {
      formatOptions: {
        year: 'numeric',
        month: 'numeric',
        day: 'numeric',
        hour: 'numeric',
        minute: '2-digit',
        timeZoneName: 'short'
      }
    }
  },
  vite: {
    publicDir,
    optimizeDeps: {
      exclude: ['@xterm/xterm', '@xterm/addon-fit']
    }
  }
})
