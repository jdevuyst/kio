package main

import (
	"fmt"

	artifact "driver/artifact"
)

func main() {
	pkg := artifact.New(struct{}{})
	pkg.Api.Child()
	fmt.Println("api.child function")
	pkg.Api.KioModule_child.Value()
	fmt.Println("api/child.value")
	child := pkg.Api.KioType_Child.Mk()
	pkg.Api.KioType_Child.Get(child)
	fmt.Println("api.Child members")
	fooBar := pkg.Api.KioType_FooBar.Mk()
	pkg.Api.KioType_FooBar.Get(fooBar)
	fmt.Println("api.Foo_bar members")
	fooExact := pkg.Api.KioType__uFooBar.Mk()
	pkg.Api.KioType__uFooBar.Get(fooExact)
	fmt.Println("api._Foo_bar members")
	pkg.Foo.KioModule_bar.KioModule_baz.Value()
	fmt.Println("foo/bar/baz.value")
	pkg.FooBar.Value()
	fmt.Println("foo_bar.value")
	pkg.KioItem__ufooBar.Value()
	fmt.Println("_foo_bar.value")
	pkg.Api.FooBar()
	fmt.Println("api.foo_bar")
	pkg.Api.KioItem__ufooBar()
	fmt.Println("api._foo_bar")
}
