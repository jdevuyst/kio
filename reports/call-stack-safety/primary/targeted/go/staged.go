//go:build kio_staged

package main

import bench "kio-bench/workload/out/go"

type callback = bench.Exp_Bench__callbackSeed_ret[int32]

func acceptsArm(arm string) bool               { return arm == "C" || arm == "S" || arm == "P" }
func lift(body func(int32) int32) callback     { return bench.Func[int32, int32](body) }
func call(value callback, input int32) int32   { return value.Call(input) }
func extract(value callback) func(int32) int32 { return value.Call }
