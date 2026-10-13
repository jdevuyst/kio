package main

import (
	"fmt"
	"runtime/debug"
	guest "tail-host/workdir/out/go"
)

type host struct {
	ticks      *int
	order      *[]int32
	panicAfter *int
}

func (h host) Api__tick() {
	*h.ticks++
	if *h.panicAfter > 0 {
		*h.panicAfter--
		if *h.panicAfter == 0 {
			panic("expected host panic")
		}
	}
}
func (h host) Api__observe(value int32) int32 {
	*h.order = append(*h.order, value)
	return value
}
func (host) KioHostIn_api_I32(value int32) int32  { return value }
func (host) KioHostOut_api_I32(value int32) int32 { return value }

func require(ok bool) {
	if !ok {
		panic("tail-call result or captured owner mismatch")
	}
}

func hostPanics(call func()) (caught bool) {
	defer func() {
		if failure := recover(); failure != nil {
			if failure != "expected host panic" {
				panic(failure)
			}
			caught = true
		}
	}()
	call()
	return false
}

func exercise() {
	const depth = 20_001
	counts := [2]int{}
	order := []int32{}
	panicAfter := 0
	packages := [2]*guest.Package[int32]{
		guest.New[int32](host{&counts[0], &order, &panicAfter}),
		guest.New[int32](host{&counts[1], &order, &panicAfter}),
	}
	chain := packages[0].Api.Root()
	require(packages[0].Api.CallZero() == 0)
	require(packages[1].Api.CallOne(7) == 7)
	require(packages[0].Api.Ordered(11, chain, 13, 17, 29) == 29)
	require(fmt.Sprint(order) == "[0 7 11 13 17 29]")
	for index := range depth {
		if index%3 == 0 {
			chain = packages[index%2].Api.WideLink(chain)
		} else {
			chain = packages[index%2].Api.Link(chain)
		}
	}
	require(counts == [2]int{})
	require(packages[1].Api.Call(chain, 17) == 17)
	require(packages[0].Api.Call(chain, 29) == 29)
	require(counts == [2]int{depth + 1, depth - 1})
	panicAfter = 257
	require(hostPanics(func() { packages[1].Api.Call(chain, 41) }))
	require(counts == [2]int{depth + 1 + 129, depth - 1 + 128})
	require(packages[0].Api.Call(chain, 53) == 53)
	require(counts == [2]int{depth + 1 + 129 + (depth+1)/2, depth - 1 + 128 + depth/2})
	valueCounts := counts

	poly := packages[0].Api.TypeRoot()
	for index := range depth {
		poly = packages[index%2].Api.TypeLink(poly)
	}
	require(counts == valueCounts)
	require(packages[1].Api.TypeCall(poly) == 37)
	require(packages[0].Api.TypeCall(poly) == 37)
	require(counts == [2]int{valueCounts[0] + depth + 1, valueCounts[1] + depth - 1})
}

func main() {
	debug.SetMaxStack(128 * 1024)
	exercise()
	fmt.Println("value and forall chains")
}
