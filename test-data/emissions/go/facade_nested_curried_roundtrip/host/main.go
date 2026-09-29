package main

import (
	"fmt"

	artifact "driver/artifact"
)

type host struct{}

func (host) KioHostIn_api_String(value string) string { return value }

func (host) KioHostOut_api_String(value string) string { return value }

func (host) Api__roundHost(
	f artifact.Env_Api__roundHost_arg0[string],
) artifact.Env_Api__roundHost_ret[string] {
	fmt.Println("round host probe:", f("host-left")("host-right"))
	return f
}

func join(left string) func(string) string {
	return func(right string) string {
		return left + "/" + right
	}
}

func main() {
	pkg := artifact.CreateFacadeNestedCurriedRoundtrip[string](host{})

	viaHost := pkg.Api.ViaHost(join)
	fmt.Println("via host:", viaHost("env-left")("env-right"))

	roundExport := pkg.Api.RoundExport(join)
	fmt.Println("round export:", roundExport("export-left")("export-right"))
}
