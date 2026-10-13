import KioRuntime

func wrongResult(_ value: KioFn<KioUnit, String>) -> KioFn<KioUnit, Int> {
    value
}
