package main

import app "retained_host/app"

type host struct{}

func (host) Api__open() string { return "live" }

func (host) KioHostIn_api_Str(value string) string { return value }

func (host) KioHostOut_api_Str(value string) string { return value }

// oldHost is the unchanged pre-removal structural host: its extra methods use
// the frozen source aliases, while the current AppHost interface requires only
// the live method and adapters embedded from host.
type oldHost struct{ host }

func (oldHost) Api__log(value string) { panic("removed log method dispatched") }

func (oldHost) Api__archived(
	head string,
	value app.Env_Api__archived_arg1[string],
) app.Env_Api__archived_ret[string] {
	panic("removed archived method dispatched")
}

func main() {
	pkg := app.CreateApp[string](host{})
	if pkg.Api.Echo("live") != "live" {
		panic("live export returned the wrong value")
	}
	oldPkg := app.CreateApp[string](oldHost{})
	if oldPkg.Api.Echo("old host") != "old host" {
		panic("live export returned the wrong value for the unchanged host")
	}

	var head string = "head"
	var input app.Env_Api__archived_arg1[string] = app.NewEnv_Api__archived_arg1_0[string]("left")
	var nested app.Env_Api__archived_ret_0_value[string] = app.NewEnv_Api__archived_ret_0_value_1[string]("right")
	var output app.Env_Api__archived_ret[string] = app.NewEnv_Api__archived_ret_0[string](nested)
	_, _, _ = head, input, output
}
