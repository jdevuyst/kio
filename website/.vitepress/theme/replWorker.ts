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

type ReplInstance = {
  init_banner(): ReplResult
  eval(input: string): ReplResult
  current_poc(): string
  pocs(): PocInfo[]
  switch_poc(id: string): ReplResult
  complete(input: string, pos: number): CompletionResult
}

type WasmModule = {
  default(input?: RequestInfo | URL | Response | BufferSource | WebAssembly.Module): Promise<unknown>
  Repl: new () => ReplInstance
}

type WorkerRequest = {
  id: number
  type: 'init' | 'eval' | 'switchPoc' | 'complete'
  payload: Record<string, unknown>
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

let repl: ReplInstance | undefined

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

function copyResult(result: ReplResult): ReplResult {
  return {
    output: result.output,
    keep_running: result.keep_running,
    is_error: result.is_error
  }
}

function copyPocs(pocs: PocInfo[]): PocInfo[] {
  return pocs.map((poc) => ({ id: poc.id }))
}

function stringPayload(payload: Record<string, unknown>, key: string): string {
  const value = payload[key]
  if (typeof value !== 'string') {
    throw new Error(`missing ${key}`)
  }
  return value
}

function numberPayload(payload: Record<string, unknown>, key: string): number {
  const value = payload[key]
  if (typeof value !== 'number') {
    throw new Error(`missing ${key}`)
  }
  return value
}

async function initRepl(payload: Record<string, unknown>): Promise<ReplInitResponse> {
  const wasmJs = stringPayload(payload, 'wasmJs')
  const wasmBin = stringPayload(payload, 'wasmBin')
  const wasm = await import(/* @vite-ignore */ wasmJs) as WasmModule
  await wasm.default(wasmBin)
  repl = new wasm.Repl()
  return {
    banner: copyResult(repl.init_banner()),
    pocs: copyPocs(repl.pocs()),
    activePoc: repl.current_poc()
  }
}

function requireRepl(): ReplInstance {
  if (!repl) {
    throw new Error('REPL is not initialized')
  }
  return repl
}

function evalLine(payload: Record<string, unknown>): ReplResult {
  return copyResult(requireRepl().eval(stringPayload(payload, 'input')))
}

function completeLine(payload: Record<string, unknown>): CompletionResult {
  const result = requireRepl().complete(stringPayload(payload, 'input'), numberPayload(payload, 'pos'))
  return {
    replaceStart: result.replaceStart,
    replaceEnd: result.replaceEnd,
    candidates: result.candidates.map((candidate) => ({
      label: candidate.label,
      kind: candidate.kind,
      detail: candidate.detail
    }))
  }
}

function switchPoc(payload: Record<string, unknown>): ReplSwitchResponse {
  const instance = requireRepl()
  const banner = copyResult(instance.switch_poc(stringPayload(payload, 'id')))
  return {
    banner,
    activePoc: instance.current_poc()
  }
}

self.addEventListener('message', (event: MessageEvent<WorkerRequest>) => {
  const { id, type, payload } = event.data

  void (async () => {
    try {
      let response: ReplInitResponse | ReplResult | ReplSwitchResponse | CompletionResult
      if (type === 'init') {
        response = await initRepl(payload)
      } else if (type === 'eval') {
        response = evalLine(payload)
      } else if (type === 'switchPoc') {
        response = switchPoc(payload)
      } else if (type === 'complete') {
        response = completeLine(payload)
      } else {
        throw new Error(`unknown REPL worker request: ${type}`)
      }

      self.postMessage({ id, ok: true, payload: response })
    } catch (error) {
      self.postMessage({ id, ok: false, error: errorMessage(error) })
    }
  })()
})

export {}
