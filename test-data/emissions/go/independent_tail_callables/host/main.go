package main

import (
	"fmt"
	"reflect"
	"runtime"
	"runtime/debug"

	a "tail-host/workdir/out/go"
	b "tail-host/workdir/peer/out/go"
)

type observation struct {
	owner int
	code  int
}

type state struct {
	marks         []observation
	conversions   []string
	stores        [2]int
	panicIncoming bool
}
type host struct {
	state  *state
	owner  int
	offset int
}

func (h host) KioHostIn_api_Number(value int) int32 {
	h.state.conversions = append(h.state.conversions, fmt.Sprintf("%d:in", h.owner))
	if h.owner == 1 && h.state.panicIncoming {
		h.state.panicIncoming = false
		panic("expected result codec panic")
	}
	return int32(value + h.offset)
}
func (h host) KioHostOut_api_Number(value int32) int {
	h.state.conversions = append(h.state.conversions, fmt.Sprintf("%d:out", h.owner))
	return int(value) - h.offset
}
func (h host) Api__mark(value int) {
	h.state.marks = append(h.state.marks, observation{h.owner, value + h.offset})
}

// The two nominal Value types stay distinct. Each host stores and returns its
// package's opaque value without inspecting its representation.
type originHost struct{ host }
type peerHost struct{ host }
type storage[T any] struct{ value T }

func (h originHost) Api__keep(value a.Env_Api__keep_arg0[int]) a.Env_Api__keep_ret[int] {
	h.state.stores[h.owner]++
	box := storage[a.Value]{value}
	return box.value
}
func (h peerHost) Api__keep(value b.Env_Api__keep_arg0[int]) b.Env_Api__keep_ret[int] {
	h.state.stores[h.owner]++
	box := storage[b.Value]{value}
	return box.value
}

func require(ok bool, subject string) {
	if !ok {
		panic(subject)
	}
}

func codecPanics(call func()) (caught bool) {
	defer func() {
		if failure := recover(); failure != nil {
			require(failure == "expected result codec panic", "changed codec panic")
			caught = true
		}
	}()
	call()
	return false
}

func marks(s *state, depth, link, root int) {
	require(len(s.marks) == depth+1, "wrong number of guest effects")
	for index := range depth {
		require(s.marks[index] == (observation{(depth - index - 1) % 2, link}), "wrong captured link owner or order")
	}
	require(s.marks[depth] == (observation{0, root}), "wrong captured root owner")
	s.marks = nil
}

func main() {
	debug.SetMaxStack(128 * 1024)
	s := &state{}
	ah := originHost{host{s, 0, 100}}
	bh := peerHost{host{s, 1, 1000}}
	origin := a.New[int](ah)
	peer := b.New[int](bh)

	// Structural aliases agree recursively, not merely at the first Call.
	var root b.Callable[int, b.Callable[b.Unit, int]] = origin.Api.Root()
	var unchanged a.Callable[int, a.Callable[a.Unit, int]] = root
	inner := peer.Api.Link(unchanged).Call(17)
	require(reflect.DeepEqual(s.marks, []observation{{1, 11}, {0, 12}}), "small link owner order")
	require(reflect.DeepEqual(s.conversions, []string{
		"1:in", "1:out", "1:out", "0:in", "0:out",
	}), "argument conversion order")
	s.marks, s.conversions = nil, nil
	require(inner.Call(a.Unit{}) == 17, "nested returned callable value")
	require(reflect.DeepEqual(s.marks, []observation{{0, 13}}), "returned callable owner")
	require(reflect.DeepEqual(s.conversions, []string{
		"0:out", "0:out", "1:in", "1:out",
	}), "returned callable result conversion order")
	s.marks, s.conversions = nil, nil
	s.panicIncoming = true
	require(codecPanics(func() { inner.Call(a.Unit{}) }), "result codec did not panic")
	require(reflect.DeepEqual(s.conversions, []string{
		"0:out", "0:out", "1:in",
	}), "queued conversion continued after codec panic")
	require(reflect.DeepEqual(s.marks, []observation{{0, 13}}), "codec panic changed guest effects")
	s.marks, s.conversions = nil, nil
	require(inner.Call(a.Unit{}) == 17, "callable was not reusable after codec panic")
	require(reflect.DeepEqual(s.conversions, []string{
		"0:out", "0:out", "1:in", "1:out",
	}), "codec panic retained stale completion maps")
	require(reflect.DeepEqual(s.marks, []observation{{0, 13}}), "reuse changed captured owner")
	s.marks, s.conversions = nil, nil

	const depth = 4096
	chain := unchanged
	retained := make([]any, 0, depth)
	for index := range depth {
		if index%2 == 0 {
			chain = origin.Api.KeepCallback(origin.Api.Link(chain))
		} else {
			chain = peer.Api.KeepCallback(peer.Api.Link(chain))
		}
		retained = append(retained, chain)
	}
	require(len(s.marks) == 0 && len(s.conversions) == 0, "construction or storage executed guest work")
	require(s.stores == [2]int{depth / 2, depth / 2}, "opaque host storage was bypassed")
	for _, value := range []int{29, 37} {
		returned := chain.Call(value)
		marks(s, depth, 11, 12)
		for range 2 {
			require(returned.Call(b.Unit{}) == value, "deep returned callable value")
			require(reflect.DeepEqual(s.marks, []observation{{0, 13}}), "deep returned callable owner")
			s.marks = nil
		}
	}
	runtime.KeepAlive(retained)
	s.conversions = nil

	// The multi-slot domain has the same anonymous structural type in both
	// artifacts; its shape comes from the resolved type, not source grouping.
	var pair b.Callable[struct{ F0, F1 int }, int] = origin.Api.PairRoot()
	retained = retained[:0]
	for index := range depth {
		if index%2 == 0 {
			pair = origin.Api.PairLink(pair)
		} else {
			pair = peer.Api.PairLink(pair)
		}
		retained = append(retained, pair)
	}
	require(pair.Call(struct{ F0, F1 int }{41, 47}) == 47, "cross-artifact product domain")
	marks(s, depth, 31, 32)
	runtime.KeepAlive(retained)

	// A Unit callback keeps its semantic index through an opaque generic value
	// position. Its native view remains assignable to the peer's callable.
	unit := origin.Api.UnitRoot()
	prepared := a.PrepareExp_Api__unitRoot_ret(origin, unit)
	roundtrip := a.CallExp_Api__genericIdentity(origin.Api, prepared)
	var foreignUnit b.Callable[b.Unit, int] = a.UnprepareExp_Api__unitRoot_ret(origin, roundtrip)
	require(foreignUnit.Call(b.Unit{}) == -57, "substituted Unit callable")
	require(reflect.DeepEqual(s.marks, []observation{{0, 42}}), "substituted Unit owner")
	s.marks, s.conversions = nil, nil

	// Native stages import with the receiving instance's role codecs. Direct
	// selection uses the constructor's retained context instead.
	other := a.New[int](originHost{host{s, 1, 1000}})
	stageCalls := 0
	native := a.TypeFunc[int](func() int { stageCalls++; return 7 })
	stage := a.NewExp_Api__invokeStage_arg0Stage(ah, native)
	require(stageCalls == 0 && len(s.conversions) == 0, "native stage construction executed")
	require(other.Api.InvokeStage(stage) == 7 && stageCalls == 1, "receiving native stage value")
	require(reflect.DeepEqual(s.conversions, []string{"1:in", "1:out"}), "native stage import used the creator codecs")
	s.conversions = nil
	require(a.SelectExp_Api__invokeStage_arg0[a.KioIndexUnit](stage) == 7 && stageCalls == 2, "direct native stage selection")
	require(reflect.DeepEqual(s.conversions, []string{"0:in", "0:out"}), "native stage selection lost its retained context")
	fmt.Println("independent callable transport ok")
}
