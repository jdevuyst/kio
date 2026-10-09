package main

import (
	artifact "driver/artifact"
	"fmt"
)

type host struct{ events *[]int32 }

func (host) KioHostIn_api_Count(value int32) int32  { return value }
func (host) KioHostOut_api_Count(value int32) int32 { return value }
func (h host) Api__split(left, right int32) int32 {
	*h.events = append(*h.events, left, right)
	return left
}
func (h host) Api__packed(pair artifact.Env_Api__packed_arg0[int32]) int32 {
	*h.events = append(*h.events, pair.F0, pair.F1)
	return pair.F0
}
func (h host) Api__seed() int32 {
	value := int32(len(*h.events) + 1)
	*h.events = append(*h.events, value)
	return value
}

var _ artifact.Host[int32] = host{}

func main() {
	events := []int32{}
	var pkg *artifact.Package[int32] = artifact.New[int32](host{&events})
	var split func(int32, int32) int32 = pkg.Api.First
	var packed func(artifact.Product[int32, int32]) int32 = pkg.Api.Second
	pair := artifact.Product[int32, int32]{F0: 7, F1: 9}
	if split(3, 5) != 3 || packed(pair) != 7 || pkg.Api.Staged(11, 13) != 11 {
		panic("declaration grouping")
	}
	if pkg.Api.Returned(17)(19) != 17 {
		panic("returned function stage")
	}
	if pkg.Api.Direct() != 1 || fmt.Sprint(events) != "[1 2 1 2]" {
		panic("host evaluation order")
	}
	if pkg.Api.ThroughValue(pair) != 7 || fmt.Sprint(events) != "[1 2 1 2 7 9]" {
		panic("host declaration as a function value")
	}
	step := func(value artifact.Product[int32, int32]) int32 { return value.F1 }
	if pkg.Api.Callback(step, pair) != 9 || pkg.Api.Substituted(step, pair) != 9 {
		panic("product callback or substitution")
	}
	if pkg.Api.KioType_Pair.Read(pkg.Api.KioType_Pair.Make(pair)) != pair {
		panic("newtype member grouping")
	}
	fmt.Println("source grouping and package roles ok")
}
