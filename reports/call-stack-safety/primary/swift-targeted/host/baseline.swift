typealias ScalarCallback = (Int32) -> Int32

func acceptsArm(_ arm: String) -> Bool { arm == "B" || arm == "T" }
func lift(_ body: @escaping (Int32) -> Int32) -> ScalarCallback { body }
func call(_ callback: ScalarCallback, _ value: Int32) -> Int32 { callback(value) }
func extract(_ callback: @escaping ScalarCallback) -> (Int32) -> Int32 { callback }
