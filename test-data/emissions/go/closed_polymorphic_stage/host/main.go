package main

import (
	p "driver/out/go"
	"fmt"
)

type numberIndex = p.Index_api__Number[int]
type host struct {
	offset, selections, invocations, incoming, outgoing int
}

func (h *host) KioHostIn_api_Number(value int) int32 {
	h.incoming++
	return int32(value + h.offset)
}
func (h *host) KioHostOut_api_Number(value int32) int {
	h.outgoing++
	return int(value) - h.offset
}
func (h *host) Api__selection()  { h.selections++ }
func (h *host) Api__invocation() { h.invocations++ }

func check(ok bool, subject string) {
	if !ok {
		panic(subject)
	}
}

func main() {
	first := &host{offset: 100}
	second := &host{offset: 1000}
	pkg := p.New[int, string](first)
	other := p.New[int, string](second)
	closed := p.CallExp_Api__outer(pkg.Api, p.PrepareAtom("unused"))
	_ = p.CallExp_Api__outer(pkg.Api, p.PrepareAtom(0))
	value := p.ValueExp_Api__outer_ret(pkg, closed)
	native := p.NativeExp_Api__outer_ret(pkg, value)
	prepared := p.PrepareExp_Api__outer_ret(pkg, native)
	recovered := p.UnprepareExp_Api__outer_ret(pkg, prepared)
	check(first.selections == 0 && first.invocations == 0 && first.incoming == 0 && first.outgoing == 0, "construction, discarded stage or exact views executed a stage")
	selected := p.SelectExp_Api__outer_ret[numberIndex](recovered)
	check(first.selections == 1 && first.invocations == 0, "Select did not execute exactly one type stage")
	check(pkg.Api.Read(selected(p.PrepareHost_api_Number(pkg, 13))) == 13, "first selected value")
	check(pkg.Api.Read(selected(p.PrepareHost_api_Number(pkg, 17))) == 17, "second selected value")
	check(first.selections == 1 && first.invocations == 2, "reusing the residual replayed its type stage")
	again := p.SelectExp_Api__outer_ret[numberIndex](closed)
	check(first.selections == 2 && first.invocations == 2, "independent Select was fused or invoked the value stage")
	check(pkg.Api.Read(again(p.PrepareHost_api_Number(pkg, 19))) == 19, "independent selected value")
	check(first.selections == 2 && first.invocations == 3, "independent residual stage count")

	otherView := p.NativeExp_Api__outer_ret(other, value)
	check(first.selections == 2 && first.invocations == 3 && second.selections == 0 && second.invocations == 0, "cross-instance recovery executed a stage")
	otherSelected := p.SelectExp_Api__outer_ret[numberIndex](otherView)
	check(first.selections == 3 && first.invocations == 3 && second.selections == 0 && second.invocations == 0, "recovered stage changed its captured host")
	beforeIncoming, beforeOutgoing := first.incoming, first.outgoing
	otherBox := otherSelected(p.PrepareHost_api_Number(other, 23))
	check(first.invocations == 4 && second.invocations == 0, "recovered residual changed its captured host")
	check(second.incoming == 1 && second.outgoing == 0, "recovered view did not use the receiving role adapter")
	check(other.Api.Read(otherBox) == 23 && second.outgoing == 1, "receiving native view changed the value")
	check(first.incoming == beforeIncoming && first.outgoing == beforeOutgoing, "receiving view used the creator role adapters")
	check(pkg.Api.Read(selected(p.PrepareHost_api_Number(pkg, 29))) == 29, "stored residual was rebound by another native view")
	check(first.selections == 3 && first.invocations == 5 && second.selections == 0 && second.invocations == 0, "selection or capture effects drifted")
	fmt.Println("closed polymorphic stage: inert views, exact effects and captured host pass")
}
