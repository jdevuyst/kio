//go:build assignment_negative

package main

import (
	alpha "generic_shell_host/shell_alpha"
	beta "generic_shell_host/shell_beta"
)

func assignmentMustNotCompile() {
	var left alpha.Exp_Api__sumA_arg0[bool, int32, int64, string]
	var right beta.Exp_Api__sumA_arg0[bool, int32, int64, string]
	left = right
	_ = left
}
