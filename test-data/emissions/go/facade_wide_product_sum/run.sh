#!/bin/sh
# SUBJECT: Go's public generic facade compiles and crosses every field and arm at the recorded Product257 and Sum513 capacities.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

product_arity=$(cat "$scratch/host/product_arity")
sum_arity=$(cat "$scratch/host/sum_arity")

emit_type_chain() {
  etc_count=$1
  etc_separator=$2
  etc_index=0
  while [ "$etc_index" -lt "$etc_count" ]; do
    if [ "$etc_index" -ne 0 ]; then
      printf ' %s ' "$etc_separator"
    fi
    printf 'I32'
    etc_index=$((etc_index + 1))
  done
}

emit_api() {
  ea_product_arity=$1
  ea_sum_arity=$2
  {
    printf 'module api;\n\n'
    printf 'host type I32 role(i32);\n\n'
    printf 'pub type Product%s = ' "$ea_product_arity"
    emit_type_chain "$ea_product_arity" '&'
    printf ';\n\n'
    printf 'pub type Sum%s = ' "$ea_sum_arity"
    emit_type_chain "$ea_sum_arity" '|'
    printf ';\n\n'
    printf 'host fn supply_product() -> Product%s;\n\n' "$ea_product_arity"
    printf 'pub fn product() -> Product%s { supply_product() }\n\n' "$ea_product_arity"
    printf 'pub fn sum(value: Sum%s) -> Sum%s { value }\n' \
      "$ea_sum_arity" "$ea_sum_arity"
  } >"$scratch/workdir/api.kio"
}

package_dir=$scratch/workdir
emit_api "$product_arity" "$sum_arity"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

stage=$scratch/driver
mkdir -p \
  "$stage/artifact" \
  "$stage/.gocache" \
  "$stage/.gomodcache" \
  "$stage/.gotmp" \
  "$stage/.telemetry"
cp "$package_dir"/out/go/*.go "$stage/artifact/"

{
  printf 'module driver\n\n'
  printf 'go 1.26\n'
} >"$stage/go.mod"

{
  printf 'package main\n\n'
  printf 'import (\n'
  printf '\t"fmt"\n\n'
  printf '\tartifact "driver/artifact"\n'
  printf ')\n\n'
  printf 'type host struct{}\n\n'
  printf 'func (host) KioHostIn_api_I32(value int32) int32 { return value }\n\n'
  printf 'func (host) KioHostOut_api_I32(value int32) int32 { return value }\n\n'
  printf 'func (host) Api__supplyProduct() artifact.Env_Api__supplyProduct_ret[int32] {\n'
  printf '\treturn artifact.Env_Api__supplyProduct_ret[int32]{\n'
  field=0
  while [ "$field" -lt "$product_arity" ]; do
    printf '\t\tF%s: int32(%s),\n' "$field" "$field"
    field=$((field + 1))
  done
  printf '\t}\n'
  printf '}\n\n'
  printf 'func main() {\n'
  printf '\tpkg := artifact.CreateWideLarge[int32](host{})\n'
  printf '\tproduct := pkg.Api.Product()\n'
  field=0
  while [ "$field" -lt "$product_arity" ]; do
    printf '\tif product.F%s != int32(%s) {\n' "$field" "$field"
    printf '\t\tpanic(fmt.Sprintf("product field F%s: %%d", product.F%s))\n' \
      "$field" "$field"
    printf '\t}\n'
    field=$((field + 1))
  done

  arm=0
  while [ "$arm" -lt "$sum_arity" ]; do
    printf '\t{\n'
    printf '\t\tvalue := pkg.Api.Sum(artifact.NewExp_Api__sum_arg0_%s[int32](int32(%s)))\n' \
      "$arm" "$arm"
    printf '\t\tswitch selected := value.Case().(type) {\n'
    printf '\t\tcase artifact.Exp_Api__sum_ret_%s[int32]:\n' "$arm"
    printf '\t\t\tif selected.Value() != int32(%s) {\n' "$arm"
    printf '\t\t\t\tpanic(fmt.Sprintf("sum arm %s payload: %%d", selected.Value()))\n' \
      "$arm"
    printf '\t\t\t}\n'
    printf '\t\tdefault:\n'
    printf '\t\t\tpanic("sum arm %s selected the wrong case")\n' "$arm"
    printf '\t\t}\n'
    printf '\t}\n'
    arm=$((arm + 1))
  done

  printf '\tvar zero artifact.Exp_Api__sum_arg0[int32]\n'
  printf '\tzeroValue := pkg.Api.Sum(zero)\n'
  printf '\tswitch selected := zeroValue.Case().(type) {\n'
  printf '\tcase artifact.Exp_Api__sum_ret_0[int32]:\n'
  printf '\t\tif selected.Value() != int32(0) {\n'
  printf '\t\t\tpanic(fmt.Sprintf("sum zero payload: %%d", selected.Value()))\n'
  printf '\t\t}\n'
  printf '\tdefault:\n'
  printf '\t\tpanic("sum zero selected the wrong case")\n'
  printf '\t}\n'
  printf '\tfmt.Println("go facade wide product/sum: ok")\n'
  printf '}\n'
} >"$stage/main.go"

(
  cd "$stage"
  GOENV=off \
    GOEXPERIMENT='' \
    GOTOOLCHAIN=local \
    GOWORK=off \
    GOPROXY=off \
    GOSUMDB=off \
    GOTELEMETRY=off \
    CGO_ENABLED=0 \
    GOCACHE="$stage/.gocache" \
    GOMODCACHE="$stage/.gomodcache" \
    GOTMPDIR="$stage/.gotmp" \
    TEST_TELEMETRY_DIR="$stage/.telemetry" \
    TMPDIR="$stage/.gotmp" \
    go run .
)
