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

type storage struct{ value artifact.Value }

func box[T artifact.KioIndexKind_S](value artifact.Value) artifact.KioHostType_api__Box[T] {
	return artifact.UnsafeKioHostType_api__BoxFromNative[T](&storage{value})
}

func main() {
	events := []string{}
	pkg := artifact.New[int32](host{&events})
	payload := pkg.Api.ProductValue()
	input := box[artifact.IndexExp_Api__productValue_ret[int32]](artifact.ValueExp_Api__productValue_ret(pkg, payload))
	output := pkg.Api.ConcreteProduct(input)
	product := artifact.NativeExp_Api__productValue_ret(pkg, output.UnsafeNative().(*storage).value)
	if product.F0 != 11 || product.F1 != 22 || product.F2 != 33 {
		panic("concrete product")
	}

	type numberIndex = artifact.AtomIndex[int32]
	type pairIndex = artifact.AtomIndex[artifact.Product[int32, int32]]
	nested := artifact.Product[int32, artifact.Product[int32, int32]]{F0: 11, F1: artifact.Product[int32, int32]{F0: 22, F1: 33}}
	prepared := artifact.Exp_Api__product_arg0_typearg0[int32, numberIndex, pairIndex]{F0: artifact.PrepareAtom(nested.F0), F1: artifact.PrepareAtom(nested.F1)}
	checkProduct := func(value artifact.Exp_Api__product_ret[int32, numberIndex, pairIndex], label string) {
		decoded := artifact.NativeExp_Api__product_arg0_typearg0[int32, numberIndex, pairIndex](pkg, value.UnsafeNative().(*storage).value)
		actual := artifact.Product[int32, artifact.Product[int32, int32]]{F0: artifact.NativeAtom[int32](decoded.F0.Erase()), F1: artifact.NativeAtom[artifact.Product[int32, int32]](decoded.F1.Erase())}
		if !reflect.DeepEqual(actual, nested) {
			panic(label)
		}
	}
	generic := artifact.CallExp_Api__product(pkg.Api, box[artifact.IndexExp_Api__product_arg0_typearg0[int32, numberIndex, pairIndex]](artifact.ValueExp_Api__product_arg0_typearg0[int32, numberIndex, pairIndex](pkg, prepared)))
	checkProduct(generic, "generic product")
	callbackCalls := 0
	callback := artifact.Func[artifact.Exp_Api__callback_arg1[int32, numberIndex, pairIndex], artifact.Exp_Api__callback_ret[int32, numberIndex, pairIndex]](func(value artifact.Exp_Api__callback_arg1[int32, numberIndex, pairIndex]) artifact.Exp_Api__callback_ret[int32, numberIndex, pairIndex] {
		callbackCalls++
		events = append(events, "callback")
		return value
	})
	called := artifact.CallExp_Api__callback(pkg.Api, callback, generic)
	later := artifact.CallExp_Api__returned(pkg.Api, called)
	if callbackCalls != 1 {
		panic("callback count")
	}
	checkProduct(later.Call(artifact.Unit{}), "returned product")
	checkProduct(later.Call(artifact.Unit{}), "repeated returned product")
	if !reflect.DeepEqual(events, []string{"echo", "echo", "callback"}) {
		panic(events)
	}

	values := []artifact.Exp_Api__sumValue_arg0[int32]{
		artifact.NewExp_Api__sumValue_arg0_0[int32](11),
		artifact.NewExp_Api__sumValue_arg0_1[int32](22),
		artifact.NewExp_Api__sumValue_arg0_2[int32](33),
	}
	for index, value := range values {
		input := box[artifact.IndexExp_Api__sumValue_ret[int32]](artifact.ValueExp_Api__sumValue_ret(pkg, pkg.Api.SumValue(value)))
		output := pkg.Api.ConcreteSum(input)
		sum := artifact.NativeExp_Api__sumValue_ret(pkg, output.UnsafeNative().(*storage).value)
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
