//go:build kio_baseline

package main

import bench "kio-bench/workload/out/go"

type callback = bench.Exp_Bench__callbackSeed_ret[int32]

func acceptsArm(arm string) bool               { return arm == "B" || arm == "T" }
func lift(body func(int32) int32) callback     { return body }
func call(value callback, input int32) int32   { return value(input) }
func extract(value callback) func(int32) int32 { return value }
