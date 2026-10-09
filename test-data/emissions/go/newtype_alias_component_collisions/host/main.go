package main

import (
	"fmt"

	artifact "driver/artifact"
)

type host struct{}

func (host) KioHostIn_api_I32(value int32) int32 { return value }

func (host) KioHostOut_api_I32(value int32) int32 { return value }

func main() {
	pkg := artifact.New[int32](host{})
	var left0 int32 = 1
	var left1 int32 = 2
	var right0 int32 = 3
	var right1 int32 = 4
	var left artifact.Exp_KioItem_api__AB__c_ret[int32] = pkg.Api.KioType_AB.C(artifact.Exp_KioItem_api__AB__c_arg0[int32]{F0: left0, F1: left1})
	var right artifact.Exp_Api__A_bC_ret[int32] = pkg.Api.KioType_A.BC(artifact.Exp_Api__A_bC_arg0[int32]{F0: right0, F1: right1})
	fmt.Println(left.F0, left.F1)
	fmt.Println(right.F0, right.F1)
}
