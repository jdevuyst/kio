package main

import (
	"fmt"

	alpha "generic_shell_host/shell_alpha"
	beta "generic_shell_host/shell_beta"
)

type host struct{}

func (host) KioHostIn_api_Flag(value bool) bool { return value }

func (host) KioHostOut_api_Flag(value bool) bool { return value }

func (host) KioHostIn_api_I32(value int32) int32 { return value }

func (host) KioHostOut_api_I32(value int32) int32 { return value }

func (host) KioHostIn_api_I64(value int64) int64 { return value }

func (host) KioHostOut_api_I64(value int64) int64 { return value }

func (host) KioHostIn_api_Text(value string) string { return value }

func (host) KioHostOut_api_Text(value string) string { return value }

func main() {
	alphaPackage := alpha.CreateShellAlpha[bool, int32, int64, string](host{})
	betaPackage := beta.CreateShellBeta[bool, int32, int64, string](host{})

	pairA := alpha.Product[int32, string]{F0: 7, F1: "seven"}
	var pairAOut alpha.Product[int32, string] = alphaPackage.Api.PairA(pairA.F0, pairA.F1)
	var pairANumber int32 = pairAOut.F0
	var pairAText string = pairAOut.F1
	fmt.Printf("alpha product i32/string: %d %s\n", pairANumber, pairAText)

	pairB := alpha.Product[bool, int64]{F0: true, F1: 9}
	var pairBOut alpha.Product[bool, int64] = alphaPackage.Api.PairB(pairB.F0, pairB.F1)
	var pairBFlag bool = pairBOut.F0
	var pairBNumber int64 = pairBOut.F1
	fmt.Printf("alpha product bool/i64: %t %d\n", pairBFlag, pairBNumber)

	var sumA alpha.Sum[int32, string] = alpha.NewExp_Api__sumA_arg0_0[bool, int32, int64, string](11)
	sumAOut := alphaPackage.Api.SumA(sumA)
	switch arm := sumAOut.Case().(type) {
	case alpha.Exp_Api__sumA_ret_0[bool, int32, int64, string]:
		var value int32 = arm.Value()
		fmt.Printf("alpha sum i32/string: %d\n", value)
	default:
		panic("alpha sum_a returned the wrong arm")
	}

	var sumB alpha.Sum[bool, int64] = alpha.NewExp_Api__sumB_arg0_1[bool, int32, int64, string](12)
	sumBOut := alphaPackage.Api.SumB(sumB)
	switch arm := sumBOut.Case().(type) {
	case alpha.Exp_Api__sumB_ret_1[bool, int32, int64, string]:
		var value int64 = arm.Value()
		fmt.Printf("alpha sum bool/i64: %d\n", value)
	default:
		panic("alpha sum_b returned the wrong arm")
	}

	var zero alpha.Sum[int32, string]
	zeroOut := alphaPackage.Api.SumA(zero)
	switch arm := zeroOut.Case().(type) {
	case alpha.Exp_Api__sumA_ret_0[bool, int32, int64, string]:
		var value int32 = arm.Value()
		fmt.Printf("alpha sum zero: %d\n", value)
	default:
		panic("alpha zero sum did not select its first arm")
	}

	betaSum := beta.NewExp_Api__sumA_arg0_1[bool, int32, int64, string]("beta")
	betaOut := betaPackage.Api.SumA(betaSum)
	switch arm := betaOut.Case().(type) {
	case beta.Exp_Api__sumA_ret_1[bool, int32, int64, string]:
		var value string = arm.Value()
		fmt.Printf("beta sum i32/string: %s\n", value)
	default:
		panic("beta sum_a returned the wrong arm")
	}
}
