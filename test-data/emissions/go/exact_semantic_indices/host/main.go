package main

import (
	p "driver/out/go"
	"fmt"
)

type numberIndex = p.Index_api__Number[int]
type countIndex = p.Index_api__Count[int]
type textIndex = p.Index_api__Text[string]

type host struct{}

func (host) KioHostIn_api_Number(value int) int32  { return int32(value + 100) }
func (host) KioHostOut_api_Number(value int32) int { return int(value) - 100 }
func (host) KioHostIn_api_Count(value int) int32   { return int32(value + 1000) }
func (host) KioHostOut_api_Count(value int32) int  { return int(value) - 1000 }

type storage struct{ value p.Value }

func check(ok bool, subject string) {
	if !ok {
		panic(subject)
	}
}

func main() {
	pkg := p.New[int, int, string](host{})
	box := p.CallExp_Api__Box_make(pkg.Api.KioType_Box, p.PrepareHost_api_Number(pkg, 11))
	check(pkg.Api.ReadNumber(box) == 11, "generic constructor binds its input index")
	check(pkg.Api.ReadCount(p.CallExp_Api__Box_make(pkg.Api.KioType_Box, p.PrepareHost_api_Count(pkg, 13))) == 13, "same Go type retains Count adapter")
	check(pkg.Api.ReadNumber(pkg.Api.Number(17)) == 17, "Kio constructor preserves Number")
	check(pkg.Api.ReadCount(pkg.Api.Count(19)) == 19, "Kio constructor preserves Count")

	raw := &storage{p.ValueExp_Api__number_arg0(pkg, 23)}
	vault := p.UnsafeKioHostType_api__VaultFromNative[numberIndex](raw)
	crate := p.UnsafeKioHostType_api__CrateFromNative[numberIndex](raw)
	partial := p.UnsafeKioHostType_api__BiFromNative[textIndex, numberIndex](raw)
	fromVault := p.CallExp_Api__Applied_make(pkg.Api.KioType_Applied, p.PrepareHost_api_Vault(pkg, vault))
	fromCrate := p.CallExp_Api__Applied_make(pkg.Api.KioType_Applied, p.PrepareHost_api_Crate(pkg, crate))
	fromPartial := p.CallExp_Api__Applied_make(pkg.Api.KioType_Applied, p.PrepareHost_api_Bi(pkg, partial))
	for _, recovered := range []any{
		pkg.Api.ReadVault(fromVault).UnsafeNative(),
		pkg.Api.ReadCrate(fromCrate).UnsafeNative(),
		pkg.Api.ReadPartial(fromPartial).UnsafeNative(),
		pkg.Api.ReadVault(pkg.Api.Vault(vault)).UnsafeNative(),
		pkg.Api.ReadCrate(pkg.Api.Crate(crate)).UnsafeNative(),
		pkg.Api.ReadPartial(pkg.Api.Partial(partial)).UnsafeNative(),
	} {
		check(recovered == raw, "opaque application storage identity")
		check(p.NativeExp_Api__number_arg0(pkg, recovered.(*storage).value) == 23, "host-owned extraction preserves payload")
	}

	empty := p.CallExp_Api__List_make(pkg.Api.KioType_List, p.NewExp_Api__List_make_arg0_0[int, int, string, numberIndex](p.Unit{}))
	list := p.CallExp_Api__List_make(pkg.Api.KioType_List, p.NewExp_Api__List_make_arg0_1[int, int, string, numberIndex](p.Exp_Api__List_make_arg0_1_value[int, int, string, numberIndex]{p.PrepareHost_api_Number(pkg, 31), empty}))
	opened := p.CallExp_Api__List_open(pkg.Api.KioType_List, list)
	cons := opened.Case().(p.Exp_Api__List_open_ret_1[int, int, string, numberIndex]).Value()
	check(pkg.Api.ReadNumber(p.CallExp_Api__Box_make(pkg.Api.KioType_Box, cons.F0)) == 31, "generic recursive projection preserves index")
	_, nilTail := p.CallExp_Api__List_open(pkg.Api.KioType_List, cons.List).Case().(p.Exp_Api__List_open_ret_0[int, int, string, numberIndex])
	check(nilTail, "generic recursive tail")
	check(pkg.Api.ReadList(list).Case().(p.Exp_Api__readList_ret_1[int, int, string]).Value().F0 == 31, "generic recursive value reaches concrete Kio projector")
	kioList := pkg.Api.List(p.NewExp_Api__list_arg0_1[int, int, string](p.Exp_Api__list_arg0_1_value[int, int, string]{37, empty}))
	check(pkg.Api.ReadList(kioList).Case().(p.Exp_Api__readList_ret_1[int, int, string]).Value().F0 == 37, "Kio recursive constructor")
	tree := pkg.Api.Tree(p.NewExp_Api__tree_arg0_0[int, int, string](p.Unit{}))
	_, emptyTree := pkg.Api.ReadTree(tree).Case().(p.Exp_Api__readTree_ret_0[int, int, string])
	check(emptyTree, "distinct recursive declaration")
	branch := pkg.Api.Branch(p.NewExp_Api__branch_arg0_0[int, int, string](39))
	grove := pkg.Api.Grove(p.NewExp_Api__grove_arg0_1[int, int, string](branch))
	recoveredBranch := pkg.Api.ReadGrove(grove).Case().(p.Exp_Api__readGrove_ret_1[int, int, string]).Value()
	check(pkg.Api.ReadBranch(recoveredBranch).Case().(p.Exp_Api__readBranch_ret_0[int, int, string]).Value() == 39, "mutually recursive declarations execute through both roots")

	bottom := pkg.Api.NestQuad(p.NewExp_Api__nestQuad_arg0_0[int, int, string](p.Unit{}))
	middle := pkg.Api.NestPair(p.NewExp_Api__nestPair_arg0_1[int, int, string](p.Exp_Api__nestPair_arg0_1_value[int, int, string]{p.Exp_Api__nestPair_arg0_1_value_field0[int, int, string]{43, 47}, bottom}))
	nest := pkg.Api.Nest(p.NewExp_Api__nest_arg0_1[int, int, string](p.Exp_Api__nest_arg0_1_value[int, int, string]{41, middle}))
	first := pkg.Api.ReadNest(nest).Case().(p.Exp_Api__readNest_ret_1[int, int, string]).Value()
	check(first.F0 == 41, "nonregular recursive first head")
	second := pkg.Api.ReadNestPair(first.Nest).Case().(p.Exp_Api__readNestPair_ret_1[int, int, string]).Value()
	check(second.F0.F0 == 43 && second.F0.F1 == 47, "nonregular recursive changed argument")
	regenerated := p.CallExp_Api__Nest_make(pkg.Api.KioType_Nest, p.CallExp_Api__Nest_open(pkg.Api.KioType_Nest, nest))
	check(pkg.Api.ReadNest(regenerated).Case().(p.Exp_Api__readNest_ret_1[int, int, string]).Value().F0 == 41, "generic nonregular public members")
	check(pkg.Api.ReadWrap(pkg.Api.Wrap(53)) == 53, "ignored Const argument is finite")
	check(pkg.Api.ReadWrap(pkg.Api.KioType_Wrap.Make(59)) == 59, "ignored Const argument keeps native constructor")
	check(pkg.Api.Reveal(pkg.Api.Hide(61)) == 61, "hidden existential stays behind public operations")
	stages, projections := 0, 0
	type continuationDomain = struct {
		F0 p.Prepared[numberIndex]
		F1 p.Value
	}
	continuation := p.TypeFunc[p.Callable[continuationDomain, p.Value]](func() p.Callable[continuationDomain, p.Value] {
		stages++
		return p.Func[continuationDomain, p.Value](func(payload continuationDomain) p.Value {
			projections++
			return payload.F0.Erase()
		})
	})
	for _, number := range []int{67, 71} {
		projected := p.CallExp_Api__Hidden_open(pkg.Api.KioType_Hidden, pkg.Api.Hide(number), continuation)
		check(p.NativeExp_Api__number_arg0(pkg, projected) == number, "native existential continuation retains public index")
	}
	check(stages == 2 && projections == 2, "existential projection executes exactly one hidden stage and continuation")

	fmt.Println("exact semantic indices: constructors, opaque applications and recursion pass")
}
