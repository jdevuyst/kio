package main

import (
	"encoding/json"
	"fmt"
	"os"
	"runtime"
	"strconv"
	"time"

	bench "kio-bench/workload/out/go"
)

type host struct {
	witness bool
	trace   []int32
}

func (h *host) KioHostIn_bench_I32(value int32) int32  { return value }
func (h *host) KioHostOut_bench_I32(value int32) int32 { return value }
func (h *host) Bench__step(value, label int32) int32 {
	if h.witness {
		h.trace = append(h.trace, label)
	}
	return value + label + 1
}

func require(ok bool, message string) {
	if !ok {
		panic(message)
	}
}

func integer(text string, low, high int) int {
	value, err := strconv.Atoi(text)
	require(err == nil && low <= value && value <= high, "out-of-range integer")
	return value
}

func main() {
	require(len(os.Args) == 8, "usage: driver ARM MODE DEPTH REPEAT SEED WARMUP PURPOSE")
	arm, mode, purpose := os.Args[1], os.Args[2], os.Args[7]
	require(acceptsArm(arm), "arm does not match the compiled public API adapter")
	depth := integer(os.Args[3], 0, 10000)
	repeat := integer(os.Args[4], 1, 1000000)
	seed := integer(os.Args[5], 0, 65535)
	warmup := integer(os.Args[6], 0, 1000000)
	require(purpose == "time" || purpose == "witness", "unknown purpose")
	require(purpose != "witness" || (depth <= 8 && repeat == 1 && warmup == 0), "invalid witness bounds")
	require((mode != "direct" && mode != "native") || depth == 0, "direct/native require depth zero")
	h := &host{witness: purpose == "witness", trace: []int32{}}
	start := time.Now()
	pkg := bench.New[int32](h)
	packageNS := time.Since(start).Nanoseconds()
	start = time.Now()
	roots := make([]any, 0, depth+1)
	var invoke func(int32) int32
	switch mode {
	case "tail", "pending":
		link := pkg.Bench.Seed()
		roots = append(roots, link)
		for label := 1; label <= depth; label++ {
			if mode == "tail" {
				link = pkg.Bench.Tail(link, int32(label))
			} else {
				link = pkg.Bench.Pending(link, int32(label))
			}
			roots = append(roots, link)
		}
		invoke = func(value int32) int32 { return pkg.Bench.Invoke(link, value) }
	case "callback-tail", "callback-pending", "callback-extract":
		link := pkg.Bench.CallbackSeed()
		roots = append(roots, link)
		for label := 1; label <= depth; label++ {
			if mode == "callback-pending" {
				link = pkg.Bench.CallbackPending(link, int32(label))
			} else {
				link = pkg.Bench.CallbackTail(link, int32(label))
			}
			roots = append(roots, link)
		}
		if mode == "callback-extract" {
			invoke = extract(link)
		} else {
			invoke = func(value int32) int32 { return call(link, value) }
		}
	case "native":
		native := lift(func(value int32) int32 { return h.Bench__step(value, 0) })
		invoke = func(value int32) int32 { return pkg.Bench.CallNative(native, value) }
	case "direct":
		invoke = pkg.Bench.Direct
	case "host-loop":
		invoke = func(value int32) int32 {
			for label := depth; label >= 0; label-- {
				value = h.Bench__step(value, int32(label))
			}
			return value
		}
	default:
		panic("unknown mode")
	}
	constructNS := time.Since(start).Nanoseconds()
	require(len(h.trace) == 0, "construction executed a host body")
	delta := int32((depth + 1) * (depth + 2) / 2)
	var expected int64
	for i := 0; i < repeat; i++ {
		expected += int64((seed+i*17)%65536) + int64(delta)
	}
	start = time.Now()
	for i := 0; i < warmup; i++ {
		value := int32((seed + i*17) % 65536)
		require(invoke(value) == value+delta, "warmup checksum mismatch")
	}
	warmupNS := time.Since(start).Nanoseconds()
	start = time.Now()
	var checksum int64
	for i := 0; i < repeat; i++ {
		checksum += int64(invoke(int32((seed + i*17) % 65536)))
	}
	invokeNS := time.Since(start).Nanoseconds()
	require(checksum == expected, "measured checksum mismatch")
	if h.witness {
		require(len(h.trace) == depth+1, "wrong effect count")
		for i, label := range h.trace {
			wanted := depth - i
			if mode == "pending" || mode == "callback-pending" {
				wanted = i
			}
			require(label == int32(wanted), "wrong effect order")
		}
	}
	// Keep all predecessors live through invocation; do not force collection.
	runtime.KeepAlive(roots)
	start = time.Now()
	invoke = nil
	for i := len(roots) - 1; i >= 0; i-- {
		roots[i] = nil
	}
	roots = nil
	pkg = nil
	releaseNS := time.Since(start).Nanoseconds()
	row := map[string]any{
		"schema": "kio-targeted-driver-v1", "backend": "go", "arm": arm,
		"mode": mode, "depth": depth, "repeat": repeat, "seed": seed, "warmup": warmup, "purpose": purpose,
		"checksum": checksum, "expected": expected, "trace": h.trace,
		"package_ns": packageNS, "construct_ns": constructNS, "warmup_ns": warmupNS,
		"invoke_ns": invokeNS, "release_roots_ns": releaseNS,
		"total_phases_ns": packageNS + constructNS + warmupNS + invokeNS + releaseNS,
		"release_limit":   "roots released in reverse order; no forced GC, live-heap or stack-slope claim",
	}
	bytes, err := json.Marshal(row)
	require(err == nil, "JSON encoding failed")
	fmt.Println(string(bytes))
}
