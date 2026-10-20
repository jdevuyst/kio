import KioRuntime

typealias ScalarCallback = KioRuntime.KioFn<Int32, Int32>

func acceptsArm(_ arm: String) -> Bool { arm == "C" || arm == "S" || arm == "P" }
func lift(_ body: @escaping (Int32) -> Int32) -> ScalarCallback { KioRuntime.KioFn(body) }
func call(_ callback: ScalarCallback, _ value: Int32) -> Int32 { callback(value) }
func extract(_ callback: ScalarCallback) -> (Int32) -> Int32 { callback.callAsFunction }
