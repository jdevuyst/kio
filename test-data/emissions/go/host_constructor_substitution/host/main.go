package main

import (
	artifact "driver/artifact"
	"fmt"
	"reflect"
)

type host struct{ events *[]string }

func (host) KioHostIn_api_I32(value int32) int32  { return value }
func (host) KioHostOut_api_I32(value int32) int32 { return value }
func (h host) Api__echo(value artifact.Env_Api__echo_arg0[int32]) artifact.Env_Api__echo_ret[int32] {
	*h.events = append(*h.events, "echo")
	return value
}

func box[T any](value T) artifact.KioHostType_api__Box[T] {
	return artifact.UnsafeKioHostType_api__BoxFromNative[T](value)
}

func main() {
	events := []string{}
	pkg := artifact.New[int32](host{&events})
	payload := pkg.Api.ProductValue()
	input := box(payload)
	output := pkg.Api.ConcreteProduct(input)
	product := output.UnsafeNative().(artifact.Exp_Api__productValue_ret[int32])
	if product.F0 != 11 || product.F1 != 22 || product.F2 != 33 {
		panic("concrete product")
	}

	// Function-local binders use the documented erased slots, not native Go generic methods.
	nested := artifact.Product[any, any]{F0: int32(11), F1: artifact.Product[int32, int32]{F0: 22, F1: 33}}
	generic := pkg.Api.Product(box(nested))
	if !reflect.DeepEqual(generic.UnsafeNative(), nested) {
		panic("generic product")
	}
	callbackCalls := 0
	callback := func(value artifact.Exp_Api__callback_arg1[int32]) artifact.Exp_Api__callback_ret[int32] {
		callbackCalls++
		events = append(events, "callback")
		return value
	}
	called := pkg.Api.Callback(callback, generic)
	later := pkg.Api.Returned(called)
	if callbackCalls != 1 {
		panic("callback count")
	}
	if !reflect.DeepEqual(later().UnsafeNative(), nested) {
		panic("returned product")
	}
	if !reflect.DeepEqual(later().UnsafeNative(), nested) {
		panic("repeated returned product")
	}
	if !reflect.DeepEqual(events, []string{"echo", "echo", "callback"}) {
		panic(events)
	}

	values := []artifact.Exp_Api__sumValue_arg0[int32]{
		artifact.NewExp_Api__sumValue_arg0_0[int32](11),
		artifact.NewExp_Api__sumValue_arg0_1[int32](22),
		artifact.NewExp_Api__sumValue_arg0_2[int32](33),
	}
	for index, value := range values {
		input := box(pkg.Api.SumValue(value))
		output := pkg.Api.ConcreteSum(input)
		sum := output.UnsafeNative().(artifact.Exp_Api__sumValue_ret[int32])
		switch arm := sum.Case().(type) {
		case artifact.Exp_Api__sumValue_ret_0[int32]:
			if index != 0 || arm.Value() != 11 {
				panic("first")
			}
		case artifact.Exp_Api__sumValue_ret_1[int32]:
			if index != 1 || arm.Value() != 22 {
				panic("middle")
			}
		case artifact.Exp_Api__sumValue_ret_2[int32]:
			if index != 2 || arm.Value() != 33 {
				panic("last")
			}
		default:
			panic("wrong sum row")
		}
	}
	fmt.Println("host constructor substitution ok")
}
