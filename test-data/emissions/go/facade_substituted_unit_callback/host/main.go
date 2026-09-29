package main

import (
	"fmt"

	artifact "driver/artifact"
)

type host struct{}

func (host) KioHostIn_api_Text(value string) string { return value }

func (host) KioHostOut_api_Text(value string) string { return value }

func (host) Api__invoke(
	callback artifact.Env_Api__invoke_arg0[string],
) string {
	return "host/" + callback(artifact.Unit{})
}

func main() {
	pkg := artifact.CreateFacadeSubstitutedUnitCallback[string](host{})
	result := pkg.Api.ViaHost(func(_ artifact.Unit) string {
		return "callback"
	})
	fmt.Println(result)
}
