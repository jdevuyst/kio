//go:build conversion_negative

package main

import (
	alpha "generic_shell_host/shell_alpha"
	beta "generic_shell_host/shell_beta"
)

func conversionMustNotCompile() {
	var value alpha.Exp_Api__sumA_arg0[bool, int32, int64, string]
	_ = beta.Exp_Api__sumA_arg0[bool, int32, int64, string](value)
}
