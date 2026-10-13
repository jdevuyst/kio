package main

import (
	artifact "driver/artifact"
	"fmt"
	"reflect"
)

type vault struct{ value artifact.Value }
type host struct {
	name          string
	number, count int
	events        *[]string
}

func (h host) event(name string) { *h.events = append(*h.events, h.name+name) }
func (h host) KioHostIn_api_Number(value int) int32 {
	h.event("NumberIn")
	return int32(value + h.number)
}
func (h host) KioHostOut_api_Number(value int32) int {
	h.event("NumberOut")
	return int(value) - h.number
}
func (h host) KioHostIn_api_Count(value int) int32  { h.event("CountIn"); return int32(value + h.count) }
func (h host) KioHostOut_api_Count(value int32) int { h.event("CountOut"); return int(value) - h.count }
func (h host) Api__store(value artifact.Value) artifact.KioHostType_api__Box[artifact.KioErasedIndex_S] {
	return artifact.UnsafeKioHostType_api__BoxFromNative[artifact.KioErasedIndex_S](&vault{value})
}
func (h host) Api__extract(value artifact.KioHostType_api__Box[artifact.KioErasedIndex_S]) artifact.Value {
	h.event("extract")
	return value.UnsafeNative().(*vault).value
}
func (h host) Api__inspect(value int) int { h.event("inspect"); return value + 3 }
func (h host) Api__created()              { h.event("created") }
func (h host) Api__tick()                 { h.event("tick") }

func checkEvents(events *[]string, expected ...string) {
	if !reflect.DeepEqual(*events, expected) {
		panic(fmt.Sprintf("events: %v; wanted %v", *events, expected))
	}
	*events = nil
}

func mustPanic(call func()) {
	defer func() {
		if recover() == nil {
			panic("private representation became a structural view")
		}
	}()
	call()
}

func main() {
	var events []string
	first := artifact.New[int, int, any](host{"first.", 100, 1000, &events})
	second := artifact.New[int, int, any](host{"second.", 200, 2000, &events})
	callback := artifact.Func[int, int](func(value int) int { events = append(events, "callback"); return value + 7 })
	prepared := artifact.PrepareExp_Api__useCallback_arg0_typearg0(first, callback)
	checkEvents(&events)
	box := artifact.UnsafeKioHostType_api__BoxFromNative[artifact.IndexExp_Api__useCallback_arg0_typearg0[int, int, any]](&vault{prepared.Erase()})
	if second.Api.UseCallback(box, 11) != 21 {
		panic("receiver role conversion changed callback result")
	}
	checkEvents(&events, "second.NumberIn", "second.extract", "second.NumberOut", "callback", "second.CountIn", "second.CountOut", "second.inspect", "second.CountIn", "second.CountOut")

	// A prepared exact view converts at native-to-body entry, even for identity.
	converted := artifact.CallExp_Api__relay(second.Api, prepared)
	checkEvents(&events)
	native := artifact.UnprepareExp_Api__useCallback_arg0_typearg0(first, converted)
	checkEvents(&events)
	if native.Call(11) != 918 {
		panic("already-converted callback lost its receiving instance")
	}
	checkEvents(&events, "first.NumberIn", "second.NumberOut", "callback", "second.CountIn", "first.CountOut")

	pair := artifact.Exp_Api__pair_arg0[int, int, any]{F0: 13, F1: 17}
	value := artifact.PrepareExp_Api__pair_arg0(first, pair)
	checkEvents(&events)
	canonical := artifact.CallExp_Api__relay(second.Api, value)
	checkEvents(&events, "second.NumberIn", "second.CountIn")
	canonical = artifact.CallExp_Api__relay(first.Api, canonical)
	checkEvents(&events)
	got := artifact.UnprepareExp_Api__pair_ret(first, canonical)
	if got.F0 != 113 || got.F1 != 1017 {
		panic("declaration-aware compound conversion lost ownership")
	}
	checkEvents(&events, "first.NumberOut", "first.CountOut")

	kioBox := second.Api.OwnCallback()
	checkEvents(&events)
	kioCallback := artifact.NativeExp_Api__useCallback_arg0_typearg0(first, kioBox.UnsafeNative().(*vault).value)
	if kioCallback.Call(1) != -993 {
		panic("Kio-created Count was inferred from native int")
	}
	checkEvents(&events, "first.NumberIn", "first.CountOut")

	secret := first.Api.MakeSecret(23, 29)
	checkEvents(&events, "first.NumberIn", "first.CountIn")
	outer := first.Api.KioType_Outer.Wrap(secret)
	secret = first.Api.KioType_Outer.Unwrap(outer)
	secret = first.Api.ThroughOuter(outer)
	secret = first.Forward.Unwrap(outer)
	secret = first.Api.Field(artifact.Exp_Api__field_arg0[int, int, any]{SecretSlot: first.Api.Labelled(secret), F1: artifact.Unit{}})
	secret = first.Api.OpenPacked(first.Api.MakePacked(secret))
	secretValue := artifact.CallExp_Api__relay(first.Api, artifact.PrepareExp_Api__makeSecret_ret(first, secret))
	checkEvents(&events)
	mustPanic(func() { _ = artifact.NativeExp_Api__pair_ret(first, secretValue.Erase()) })
	mustPanic(func() { _ = artifact.NativeAtom[any](secretValue.Erase()) })
	read := first.Api.ReadSecret(artifact.UnprepareExp_Api__makeSecret_ret(first, secretValue))
	if read.F0 != 23 || read.F1 != 29 {
		panic("opaque nominal identity changed")
	}
	checkEvents(&events, "first.NumberOut", "first.CountOut")

	var nilInterface any
	var nilPointer *int
	if artifact.NativeAtom[any](artifact.CallExp_Api__relay(first.Api, artifact.PrepareAtom(nilInterface)).Erase()) != nil {
		panic("nil interface atom")
	}
	if artifact.NativeAtom[*int](artifact.CallExp_Api__relay(first.Api, artifact.PrepareAtom(nilPointer)).Erase()) != nil {
		panic("typed nil atom")
	}
	if first.Api.EchoToken(nil) != nil {
		panic("nil native interface")
	}
	mustPanic(func() { _ = artifact.NativeAtom[any](artifact.ValueExp_Api__unit_ret(first, artifact.Unit{})) })
	type embedded struct{ artifact.Product[int, int] }
	atom := embedded{artifact.Product[int, int]{F0: 31, F1: 37}}
	if artifact.NativeAtom[embedded](artifact.CallExp_Api__relay(first.Api, artifact.PrepareAtom(atom)).Erase()) != atom {
		panic("host atom was structurally unwrapped")
	}
	function := func() int { return 41 }
	if artifact.NativeAtom[func() int](artifact.CallExp_Api__relay(first.Api, artifact.PrepareAtom(function)).Erase())() != 41 {
		panic("host callable atom")
	}
	boxed := artifact.CallExp_Api__relayBox(first.Api, artifact.PrepareHost_api_Box(first, box))
	if artifact.UnprepareExp_Api__useCallback_arg0(first, boxed).UnsafeNative() != box.UnsafeNative() {
		panic("abstract constructor transport inspected storage")
	}
	checkEvents(&events)
	stage := first.Api.Stage()
	checkEvents(&events, "first.created")
	artifact.CallExp_Api__ignore(first.Api, artifact.PrepareExp_Api__stage_ret(first, stage))
	checkEvents(&events)
	stage.Invoke()
	stage.Invoke()
	checkEvents(&events, "first.tick", "first.tick")
	fmt.Println("canonical value views ok")
}
