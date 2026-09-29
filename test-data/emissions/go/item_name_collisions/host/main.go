package main

import (
	"fmt"

	artifact "driver/artifact"
)

type host struct{}

func (host) A___c() {
	fmt.Println("host a._c")
}

func (host) KioItem_a_u__c() {
	fmt.Println("host a_.c")
}

func main() {
	pkg := artifact.CreateGoItemNameCollisions(host{})
	pkg.A.CallHost()
	pkg.KioItem_a_u.CallHost()
	pkg.A.BC()
	fmt.Println("module a.b_c")
	pkg.A.KioModule_b.C()
	fmt.Println("module a/b.c")
}
