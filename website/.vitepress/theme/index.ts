import DefaultTheme from 'vitepress/theme'
import { defineAsyncComponent, nextTick, onMounted, onUnmounted, watch } from 'vue'
import { useRoute } from 'vitepress'
import './custom.css'
import Landing from './components/Landing.vue'

function rootCssNumber(name: string, fallback: number): number {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim()
  const parsed = Number.parseFloat(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

function horizontalExtras(style: CSSStyleDeclaration): number {
  return [
    style.paddingLeft,
    style.paddingRight,
    style.borderLeftWidth,
    style.borderRightWidth
  ].reduce((sum, value) => sum + (Number.parseFloat(value) || 0), 0)
}

function codeTextWidth(pre: HTMLPreElement, canvas: HTMLCanvasElement): number {
  const code = pre.querySelector<HTMLElement>('code') ?? pre
  const style = getComputedStyle(code)
  const preStyle = getComputedStyle(pre)
  const context = canvas.getContext('2d')
  if (!context) return pre.scrollWidth
  context.font = style.font
  const textWidth = code.innerText
    .replace(/\n$/, '')
    .split('\n')
    .reduce((max, line) => Math.max(max, context.measureText(line).width), 0)
  return Math.ceil(textWidth + horizontalExtras(style) + horizontalExtras(preStyle) + 8)
}

function updateCodeLaneWidth(): void {
  const container = document.querySelector<HTMLElement>('.VPDoc .content .content-container')
  if (!container) return

  const proseWidth = rootCssNumber('--kio-doc-prose-width', 780)
  const fmtColumns = rootCssNumber('--kio-fmt-column-budget', 100)
  const codeColumnWidth = rootCssNumber('--kio-code-column-width', 9.8)
  const maxCodeWidth = fmtColumns * codeColumnWidth
  const canvas = document.createElement('canvas')

  const codeWidths = [...container.querySelectorAll<HTMLPreElement>('pre')]
    .map((pre) => codeTextWidth(pre, canvas))
  const tableWidths = [...container.querySelectorAll<HTMLElement>('table')]
    .map((table) => Math.ceil(table.scrollWidth))
  const desired = Math.max(proseWidth, ...codeWidths, ...tableWidths)
  const pageCodeWidth = Math.min(maxCodeWidth, desired)
  container.style.setProperty('--kio-page-code-width', `${Math.ceil(pageCodeWidth)}px`)
}

function useCodeLaneWidth(): void {
  const route = useRoute()
  let frame = 0

  const schedule = () => {
    if (typeof window === 'undefined') return
    window.cancelAnimationFrame(frame)
    frame = window.requestAnimationFrame(() => {
      void nextTick(() => updateCodeLaneWidth())
    })
  }

  onMounted(() => {
    schedule()
    window.addEventListener('resize', schedule)
    void document.fonts?.ready.then(schedule)
  })
  onUnmounted(() => {
    window.cancelAnimationFrame(frame)
    window.removeEventListener('resize', schedule)
  })
  watch(() => route.path, schedule, { flush: 'post' })
}

export default {
  extends: DefaultTheme,
  setup() {
    useCodeLaneWidth()
  },
  enhanceApp({ app }) {
    app.component('KiodocPage', defineAsyncComponent(() => import('./components/KiodocPage.vue')))
    app.component('Landing', Landing)
  }
}
