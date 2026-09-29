<script setup lang="ts">
import { computed, nextTick, onActivated, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useData, withBase } from 'vitepress'
// Inlined as a string, not `?url`: a `?url` import emits xterm's CSS as a
// second CSS asset, and VitePress links the *first* CSS asset in the Rollup
// output as the theme stylesheet, which would leave every page unstyled.
import xtermCss from '@xterm/xterm/css/xterm.css?raw'

type ReplResult = {
  output: string
  keep_running: boolean
  is_error: boolean
}

type PocInfo = {
  id: string
}

type CompletionCandidate = {
  label: string
  kind: string
  detail: string | null
}

type CompletionResult = {
  replaceStart: number
  replaceEnd: number
  candidates: CompletionCandidate[]
}

type ReplInitResponse = {
  banner: ReplResult
  pocs: PocInfo[]
  activePoc: string
}

type ReplSwitchResponse = {
  banner: ReplResult
  activePoc: string
}

type WorkerResponse =
  | { id: number, ok: true, payload: unknown }
  | { id: number, ok: false, error: string }

type WorkerPending = {
  resolve: (value: unknown) => void
  reject: (error: Error) => void
}

const props = withDefaults(defineProps<{
  autofocus?: boolean
  initialInput?: string
}>(), {
  autofocus: false,
  initialInput: ''
})

const terminalEl = ref<HTMLElement | null>(null)
const terminalWrapEl = ref<HTMLElement | null>(null)
const status = ref('loading')
const loadError = ref('')
const pocs = ref<PocInfo[]>([])
const activePoc = ref('')
const switchingPoc = ref('')
const panelEl = ref<HTMLElement | null>(null)
const isExpanded = ref(false)
const expandVisible = ref(false)
const { isDark } = useData()

const completionOpen = ref(false)
const completionItems = ref<CompletionCandidate[]>([])
const completionIndex = ref(0)
const completionLeft = ref(0)
const completionTop = ref(0)
const completionBottom = ref(0)
const completionFlipUp = ref(false)
const completionListEl = ref<HTMLElement | null>(null)
const completionStyle = computed(() => ({
  left: `${completionLeft.value}px`,
  ...(completionFlipUp.value
    ? { bottom: `${completionBottom.value}px` }
    : { top: `${completionTop.value}px` })
}))

let term: import('@xterm/xterm').Terminal | undefined
let fit: import('@xterm/addon-fit').FitAddon | undefined
let line = ''
let cursor = 0
const history: string[] = []
let historyIndex = 0
let draft = ''
let resizeObserver: ResizeObserver | undefined
let worker: Worker | undefined
let workerMessageId = 0
let commandPending = false
let completionReplace = { start: 0, end: 0 }
let completionSeq = 0
let completionNavigated = false
const workerPending = new Map<number, WorkerPending>()

const textEncoder = new TextEncoder()
const textDecoder = new TextDecoder()

// The maximum menu height, matching the CSS `max-height`, so the
// dropdown flips above the cursor when it would not fit below.
const COMPLETION_MAX_HEIGHT = 168

function ensureXtermCss(): void {
  const id = 'kio-xterm-css'
  if (document.getElementById(id)) return

  const style = document.createElement('style')
  style.id = id
  style.textContent = xtermCss
  document.head.appendChild(style)
}

function writePrompt(input = '') {
  term?.write('\r\nkio> ')
  line = input
  cursor = input.length
  if (input.length > 0) {
    term?.write(input)
  }
}

// Redraw the current input line and place the terminal cursor at
// `cursor`. The prompt line is single-row, so `\r\x1b[K` clears it and a
// cursor-back sequence moves in from the end.
function renderLine() {
  if (!term) return
  term.write(`\r\x1b[Kkio> ${line}`)
  const back = line.length - cursor
  if (back > 0) term.write(`\x1b[${back}D`)
}

function setLine(next: string) {
  line = next
  cursor = next.length
  renderLine()
}

function historyPrev() {
  if (historyIndex === 0) return
  if (historyIndex === history.length) draft = line
  historyIndex -= 1
  setLine(history[historyIndex] ?? '')
}

function historyNext() {
  if (historyIndex === history.length) return
  historyIndex += 1
  setLine(historyIndex === history.length ? draft : (history[historyIndex] ?? ''))
}

function writeOutput(output: string, isError = false) {
  if (!term || output.length === 0) return
  const normalized = output.replace(/\r?\n/g, '\r\n')
  term.write(isError ? `\x1b[31m${normalized}\x1b[0m` : normalized)
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (
    error !== null
    && typeof error === 'object'
    && 'message' in error
    && typeof (error as { message: unknown }).message === 'string'
  ) {
    return (error as { message: string }).message
  }
  return String(error)
}

function cssVar(name: string, fallback: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
}

function termTheme() {
  return {
    background: cssVar('--kio-terminal-bg', '#062632'),
    foreground: cssVar('--kio-terminal-text', '#e8f8fb'),
    cursor: cssVar('--kio-terminal-accent', '#f2c766'),
    selectionBackground: cssVar('--kio-terminal-selection', '#245b6b')
  }
}

function rejectWorkerPending(error: Error) {
  for (const pending of workerPending.values()) {
    pending.reject(error)
  }
  workerPending.clear()
}

function ensureWorker(): Worker {
  if (worker) return worker

  worker = new Worker(new URL('../replWorker.ts', import.meta.url), { type: 'module' })
  worker.addEventListener('message', (event: MessageEvent<WorkerResponse>) => {
    const pending = workerPending.get(event.data.id)
    if (!pending) return
    workerPending.delete(event.data.id)

    if (event.data.ok) {
      pending.resolve(event.data.payload)
    } else {
      pending.reject(new Error(event.data.error))
    }
  })
  worker.addEventListener('error', (event) => {
    rejectWorkerPending(new Error(event.message || 'REPL worker failed'))
  })
  return worker
}

function callWorker<T>(type: string, payload: unknown = {}): Promise<T> {
  const id = ++workerMessageId
  return new Promise((resolve, reject) => {
    workerPending.set(id, {
      resolve: (value) => resolve(value as T),
      reject
    })
    ensureWorker().postMessage({ id, type, payload })
  })
}

async function runLine(input: string) {
  if (status.value !== 'ready' || commandPending) return
  commandPending = true
  status.value = 'running'
  try {
    const result = await callWorker<ReplResult>('eval', { input })
    if (result.output.trim().length > 0) {
      term?.write('\r\n')
      writeOutput(result.output, result.is_error)
    }
    if (result.keep_running) {
      writePrompt()
    } else {
      status.value = 'closed'
    }
  } catch (error) {
    term?.write('\r\n')
    writeOutput(errorMessage(error), true)
    writePrompt()
    status.value = 'ready'
  } finally {
    commandPending = false
    if (status.value === 'running') {
      status.value = 'ready'
    }
  }
}

function writeBanner(result: ReplResult) {
  line = ''
  cursor = 0
  closeCompletion()
  historyIndex = history.length
  draft = ''
  term?.reset()
  writeOutput(result.output, result.is_error)
  term?.write('\r\nkio> ')
}

function waitForPaint(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(resolve))
  })
}

async function focusTerminal() {
  if (!props.autofocus) return
  await nextTick()
  await waitForPaint()
  term?.focus()
}

let collapseTimer: ReturnType<typeof setTimeout> | undefined

function setExpanded(next: boolean) {
  closeCompletion()
  if (collapseTimer !== undefined) {
    clearTimeout(collapseTimer)
    collapseTimer = undefined
  }
  if (next) {
    isExpanded.value = true
    document.body.style.overflow = 'hidden'
    void nextTick().then(() => {
      // Commit the collapsed (opacity 0) state with a forced reflow before fading in,
      // so the enter transition runs for the same duration as the exit — without it the
      // browser can collapse the two style changes and the expand feels instant.
      void panelEl.value?.offsetHeight
      expandVisible.value = true
      fit?.fit()
      term?.focus()
    })
  } else {
    expandVisible.value = false
    collapseTimer = setTimeout(() => {
      collapseTimer = undefined
      isExpanded.value = false
      document.body.style.overflow = ''
      void waitForPaint().then(() => {
        fit?.fit()
        term?.focus()
      })
    }, 180)
  }
}

function toggleExpanded() {
  setExpanded(!isExpanded.value)
}

function onExpandKeydown(event: KeyboardEvent) {
  if (event.key !== 'Escape') return
  if (completionOpen.value) {
    closeCompletion()
    event.preventDefault()
    event.stopPropagation()
    return
  }
  if (isExpanded.value) {
    setExpanded(false)
  }
}

watch(isDark, () => {
  void nextTick().then(() => {
    if (term) term.options.theme = termTheme()
  })
})

onMounted(() => {
  window.addEventListener('keydown', onExpandKeydown, true)
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onExpandKeydown, true)
  if (collapseTimer !== undefined) clearTimeout(collapseTimer)
  document.body.style.overflow = ''
})

async function selectPoc(id: string) {
  if (!term || status.value !== 'ready' || id === activePoc.value || switchingPoc.value) return
  switchingPoc.value = id
  status.value = 'loading'
  await nextTick()
  await waitForPaint()
  try {
    const result = await callWorker<ReplSwitchResponse>('switchPoc', { id })
    activePoc.value = result.activePoc
    writeBanner(result.banner)
    status.value = 'ready'
  } catch (error) {
    term.write('\r\n')
    writeOutput(errorMessage(error), true)
    term.write('\r\nkio> ')
    status.value = 'ready'
  } finally {
    switchingPoc.value = ''
  }
}

function closeCompletion() {
  completionOpen.value = false
  completionItems.value = []
  completionIndex.value = 0
}

function moveCompletion(delta: number) {
  const count = completionItems.value.length
  if (count === 0) return
  completionIndex.value = (completionIndex.value + delta + count) % count
  completionNavigated = true
  void nextTick(() => {
    completionListEl.value?.querySelector('.repl-completion-item.active')?.scrollIntoView({ block: 'nearest' })
  })
}

// Place the dropdown at the terminal cursor, flipping above when it would
// not fit below. Offsets are relative to the terminal wrapper.
function positionCompletion() {
  if (!term || !terminalWrapEl.value) return
  const screen = term.element?.querySelector('.xterm-screen') as HTMLElement | null
  if (!screen) return
  const screenRect = screen.getBoundingClientRect()
  const wrapRect = terminalWrapEl.value.getBoundingClientRect()
  const cellW = screenRect.width / term.cols
  const cellH = screenRect.height / term.rows
  const cx = term.buffer.active.cursorX
  const cy = term.buffer.active.cursorY
  const originX = screenRect.left - wrapRect.left
  const originY = screenRect.top - wrapRect.top
  completionLeft.value = Math.max(0, Math.min(originX + cx * cellW, wrapRect.width - 8))
  const cursorRowTop = originY + cy * cellH
  const belowTop = cursorRowTop + cellH
  const fitsBelow = belowTop + COMPLETION_MAX_HEIGHT <= wrapRect.height
  completionFlipUp.value = !fitsBelow
  if (fitsBelow) {
    completionTop.value = belowTop
  } else {
    completionBottom.value = Math.max(0, wrapRect.height - cursorRowTop)
  }
}

// The byte offset of the cursor within `line`, since the wasm completion
// entry works in UTF-8 byte offsets.
function cursorByteOffset(): number {
  return textEncoder.encode(line.slice(0, cursor)).length
}

async function requestCompletion() {
  if (status.value !== 'ready' || commandPending) return
  const forLine = line
  const forCursor = cursor
  const pos = cursorByteOffset()
  const seq = ++completionSeq
  try {
    const result = await callWorker<CompletionResult>('complete', { input: forLine, pos })
    // Drop a stale response: a newer request superseded it, or the line /
    // cursor moved on while it was in flight.
    if (seq !== completionSeq || line !== forLine || cursor !== forCursor) return
    if (result.candidates.length === 0) {
      closeCompletion()
      return
    }
    completionItems.value = result.candidates
    completionReplace = { start: result.replaceStart, end: result.replaceEnd }
    completionIndex.value = 0
    completionNavigated = false
    completionOpen.value = true
    void nextTick(positionCompletion)
  } catch {
    closeCompletion()
  }
}

function acceptCompletion(index: number) {
  const item = completionItems.value[index]
  closeCompletion()
  if (!item) return
  const bytes = textEncoder.encode(line)
  const head = textDecoder.decode(bytes.slice(0, completionReplace.start))
  const tail = textDecoder.decode(bytes.slice(completionReplace.end))
  line = head + item.label + tail
  cursor = head.length + item.label.length
  renderLine()
  term?.focus()
}

function insertText(text: string) {
  line = line.slice(0, cursor) + text + line.slice(cursor)
  cursor += text.length
  renderLine()
}

function backspace() {
  if (cursor === 0) return
  line = line.slice(0, cursor - 1) + line.slice(cursor)
  cursor -= 1
  renderLine()
}

function deleteForward() {
  if (cursor >= line.length) return
  line = line.slice(0, cursor) + line.slice(cursor + 1)
  renderLine()
}

function setCursor(next: number) {
  cursor = Math.max(0, Math.min(line.length, next))
  renderLine()
}

function submitLine() {
  const current = line
  line = ''
  cursor = 0
  closeCompletion()
  if (current.trim().length > 0 && current !== history[history.length - 1]) {
    history.push(current)
  }
  historyIndex = history.length
  draft = ''
  void runLine(current)
}

// Backspace arrives as DEL (0x7f) or BS (0x08) depending on the platform.
function isBackspace(char: string): boolean {
  const code = char.charCodeAt(0)
  return code === 0x7f || code === 0x08
}

function handleEscapeSequence(data: string) {
  switch (data) {
    case '\x1b[A':
    case '\x1bOA':
      if (completionOpen.value) moveCompletion(-1)
      else historyPrev()
      return
    case '\x1b[B':
    case '\x1bOB':
      if (completionOpen.value) moveCompletion(1)
      else historyNext()
      return
    case '\x1b[C':
    case '\x1bOC':
      closeCompletion()
      setCursor(cursor + 1)
      return
    case '\x1b[D':
    case '\x1bOD':
      closeCompletion()
      setCursor(cursor - 1)
      return
    case '\x1b[H':
    case '\x1bOH':
    case '\x1b[1~':
      closeCompletion()
      setCursor(0)
      return
    case '\x1b[F':
    case '\x1bOF':
    case '\x1b[4~':
      closeCompletion()
      setCursor(line.length)
      return
    case '\x1b[3~':
      deleteForward()
      if (completionOpen.value) void requestCompletion()
      return
    case '\x1b':
      closeCompletion()
  }
}

function handleInput(data: string) {
  if (!term || status.value !== 'ready' || commandPending) return
  if (data.charCodeAt(0) === 0x1b) {
    handleEscapeSequence(data)
    return
  }
  for (const char of data) {
    if (char === '\t') {
      if (completionOpen.value) acceptCompletion(completionIndex.value)
      else void requestCompletion()
    } else if (char === '\r') {
      if (completionOpen.value && completionNavigated) acceptCompletion(completionIndex.value)
      else submitLine()
    } else if (isBackspace(char)) {
      backspace()
      void requestCompletion()
    } else if (char >= ' ') {
      insertText(char)
      void requestCompletion()
    }
  }
}

onMounted(async () => {
  try {
    ensureXtermCss()
    await waitForPaint()
    const [{ Terminal }, { FitAddon }] = await Promise.all([
      import('@xterm/xterm'),
      import('@xterm/addon-fit')
    ])

    await nextTick()
    if (!terminalEl.value) return

    term = new Terminal({
      cursorBlink: true,
      convertEol: true,
      cols: 100,
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
      fontSize: 12,
      rows: 16,
      theme: termTheme()
    })
    fit = new FitAddon()
    term.loadAddon(fit)
    term.open(terminalEl.value)
    fit.fit()
    term.onData(handleInput)
    await waitForPaint()

    const wasmJs = new URL(withBase('/wasm/kio_repl_wasm.js'), window.location.href).href
    const wasmBin = new URL(withBase('/wasm/kio_repl_wasm_bg.wasm'), window.location.href).href
    const init = await callWorker<ReplInitResponse>('init', { wasmJs, wasmBin })
    pocs.value = init.pocs
    activePoc.value = init.activePoc
    status.value = 'ready'
    writeOutput(init.banner.output, init.banner.is_error)
    writePrompt(props.initialInput)
    void focusTerminal()

    resizeObserver = new ResizeObserver(() => fit?.fit())
    resizeObserver.observe(terminalEl.value)
  } catch (error) {
    status.value = 'unavailable'
    loadError.value = errorMessage(error)
  }
})

onActivated(() => {
  void focusTerminal()
})

onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  rejectWorkerPending(new Error('REPL unmounted'))
  worker?.terminate()
  term?.dispose()
})
</script>

<template>
  <section ref="panelEl" class="repl-panel" :class="{ expanded: isExpanded, visible: expandVisible }" aria-label="Kio browser REPL">
    <div class="repl-panel-head">
      <span class="repl-title">Kio REPL</span>
      <div class="repl-head-right">
        <span class="repl-status" :data-state="status">{{ status }}</span>
        <button
          class="repl-expand"
          type="button"
          :aria-label="isExpanded ? 'Exit full viewport' : 'Expand to full viewport'"
          @click="toggleExpanded"
        >
          <svg v-if="isExpanded" viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M5 16h3v3h2v-5H5v2zm3-8H5v2h5V5H8v3zm6 11h2v-3h3v-2h-5v5zm2-11V5h-2v5h5V8h-3z" /></svg>
          <svg v-else viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z" /></svg>
        </button>
      </div>
    </div>
    <div v-if="loadError" class="repl-fallback">
      <p>Browser REPL unavailable.</p>
      <code>{{ loadError }}</code>
    </div>
    <div ref="terminalWrapEl" class="repl-terminal-wrap">
      <div ref="terminalEl" class="repl-terminal" />
      <ul
        v-if="completionOpen"
        ref="completionListEl"
        class="repl-completion"
        role="listbox"
        aria-label="Completions"
        :style="completionStyle"
      >
        <li
          v-for="(item, index) in completionItems"
          :key="index"
          class="repl-completion-item"
          :class="{ active: index === completionIndex }"
          role="option"
          :aria-selected="index === completionIndex"
          @mousedown.prevent="acceptCompletion(index)"
        >
          <span class="repl-completion-label">{{ item.label }}</span>
          <span class="repl-completion-kind">{{ item.kind }}</span>
        </li>
      </ul>
    </div>
    <div v-if="pocs.length > 1" class="repl-poc-list" aria-label="REPL examples">
      <span class="repl-poc-label">Examples:</span>
      <button
        v-for="poc in pocs"
        :key="poc.id"
        class="repl-poc-button"
        :class="{ active: !switchingPoc && poc.id === activePoc, pending: poc.id === switchingPoc }"
        :aria-pressed="poc.id === activePoc"
        :aria-busy="poc.id === switchingPoc"
        :disabled="status !== 'ready' || Boolean(switchingPoc)"
        type="button"
        @click="selectPoc(poc.id)"
      >
        <span v-if="poc.id === switchingPoc" class="repl-poc-spinner" aria-hidden="true" />
        <span>{{ poc.id }}</span>
      </button>
    </div>
  </section>
</template>
