#!/bin/sh
# POC for `dyn_load_prime` — dynamic loading of whole-package Kio'
# (prime) images, implemented as an ordinary Kio package. A host links
# `dyn_load_prime` once and from then on loads any pre-compiled package
# at runtime: it lexes and scans the emitted Kio' image text into the
# loader's declaration list, resolves the bodies into erased Kio' terms,
# contract-matches the image's declared surface against the interface the
# host expects, evaluates the exports through a CEK machine, and presents
# them behind a universal existential `Surface(P)`. The worked example in
# `testapi/main` loads a guest module from its verbatim emitted image
# text and calls its exports — a host-call greeting, a cross-fn `quad`, a
# polymorphic `use_poly`, the multi-value-param `add3` / `use_add3`, a
# branching `pick` over both arms — then contract-matches three contracts
# and runs one contract-gated call. The library lives in the regular
# module tree (`loader/scan` / `loader` / `eval` / `value` / `parse` / `lex` /
# …); the host capabilities a guest's effects flow through stand in as
# `testapi/**` host fns a real host replaces.
#
# Checks the public signature, then chains `kio check` + `kio test` +
# per-backend build + run.
# `expected.stdout` snapshots the worked example's output only; `kio
# check` is silent on success and `kio test`'s status lines are
# discarded so the snapshot stays focused on the program's own output.
set -u
cd workdir || exit

main_source=testapi/main.kio
loader_source=loader.kio
count_occurrences() {
  awk -v needle="$1" '
    {
      rest = $0
      while ((at = index(rest, needle)) != 0) {
        count++
        rest = substr(rest, at + length(needle))
      }
    }
    END { print count + 0 }
  ' "$2"
}

# The loader's type-origin reconciliation and identity-alias member catalog share
# one exact declaration index when either is needed. Keep this private
# architecture guard close to the Kio-authored loader: it prevents the index
# from becoming dead scaffolding and prevents the former per-alias whole-module
# scans from returning unnoticed. Qualified Ntab enumeration remains outside
# the guarded resolver region because emitting one visible spelling per provider
# alias is output-proportional.
index_builds=$(count_occurrences 'exact_type_index_for_modules(ms)' "$loader_source") || exit
resolver_region=$(awk '
  /^fn resolve_provider_type_head\(/ { inside = 1 }
  /^labels \{ punct_run_done:/ { inside = 0 }
  inside { print }
' "$loader_source") || exit
helper_region=$(awk '
  /^fn type_context_find_module\(/ { inside = 1 }
  /^\/\/ One lexical type binder/ { inside = 0 }
  inside { print }
' "$loader_source") || exit
context_index_region=$(awk '
  /^fn type_context_exact_index\(/ { inside = 1 }
  /^fn type_context_for_alias\(/ { inside = 0 }
  inside { print }
' "$loader_source") || exit
catalog_region=$(awk '
  /^fn identity_alias_ref_has_exact_kinds\(/ { inside = 1 }
  /^fn ntab_add_identity_aliases_from\(/ { inside = 0 }
  inside { print }
' "$loader_source") || exit
identity_shape_region=$(awk '
  /^fn tyaliases_have_identity_member_namespace\(/ { inside = 1 }
  /^fn ntab_add_identity_alias\(/ { inside = 0 }
  inside { print }
' "$loader_source") || exit
type_overlap_region=$(awk '
  /^fn type_spelling_scan_add\(/ { inside = 1 }
  /^\/\/ Validate every selective import/ { inside = 0 }
  inside { print }
' "$loader_source") || exit
projection_region=$(awk '
  /^fn identity_alias_member_for_projection\(/ { inside = 1 }
  /^\/\/\/ Resolves a whole package/ { inside = 0 }
  inside { print }
' "$loader_source") || exit
flatten_region=$(awk '
  /^fn flatten_modules\(/ { inside = 1 }
  /^\/\/ ====/ { if (inside) { inside = 0 } }
  inside { print }
' "$loader_source") || exit
scope_cache_region=$(awk '
  /^fn local_hostfn_between\(/ { inside = 1 }
  /^fn import_visibility_diag\(/ { inside = 0 }
  inside { print }
' "$loader_source") || exit
load_region=$(awk '
  /^pub\(loader\) fn resolve_package\(/ { inside = 1 }
  /^\/\/ The shared lookup-failure diagnostics/ { inside = 0 }
  inside { print }
' "$loader_source") || exit
resolver_compact=$(printf '%s\n' "$resolver_region" | tr -d '[:space:]') || exit
helper_compact=$(printf '%s\n' "$helper_region" | tr -d '[:space:]') || exit
context_index_compact=$(printf '%s\n' "$context_index_region" | tr -d '[:space:]') || exit
catalog_compact=$(printf '%s\n' "$catalog_region" | tr -d '[:space:]') || exit
identity_shape_compact=$(printf '%s\n' "$identity_shape_region" | tr -d '[:space:]') || exit
type_overlap_compact=$(printf '%s\n' "$type_overlap_region" | tr -d '[:space:]') || exit
projection_compact=$(printf '%s\n' "$projection_region" | tr -d '[:space:]') || exit
flatten_compact=$(printf '%s\n' "$flatten_region" | tr -d '[:space:]') || exit
scope_cache_compact=$(printf '%s\n' "$scope_cache_region" | tr -d '[:space:]') || exit
load_compact=$(printf '%s\n' "$load_region" | tr -d '[:space:]') || exit

if [ "$index_builds" -ne 1 ]; then
  printf 'dyn_load_prime must build one exact type index for type-origin loading\n' >&2
  exit 1
fi
case "$context_index_compact" in
  *'index:.|Exact_type_index){index}}}'*) ;;
  *)
    printf 'dyn_load_prime type context discards its shared exact type index\n' >&2
    exit 1
    ;;
esac
for required in \
  '.(index:Exact_type_index){exact_type_index_module(index,owner)}' \
  '.(index:Exact_type_index){exact_type_index_aliases(index,module_name(module),name)}' \
  '.(index:Exact_type_index){exact_type_index_members(index,module_name(module),name)}' \
  '.(index:Exact_type_index){exact_type_index_host_headers(index,module_name(module),name)}' \
  '.(index:Exact_type_index){exact_type_index_imports(index,module_name(module),name)}' \
  '.(index:Exact_type_index){exact_type_index_qualifiers(index,module_name(module),name)}'
do
  case "$helper_compact" in
    *"$required"*) ;;
    *)
      printf 'dyn_load_prime indexed type-context helper is not wired: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done
for required in \
  'type_context_find_module(' \
  'type_context_aliases(' \
  'type_context_members(' \
  'pick_type_context_host_type(' \
  'type_context_imports(' \
  'type_context_qualifiers('
do
  case "$resolver_compact" in
    *"$required"*) ;;
    *)
      printf 'dyn_load_prime alias resolver bypasses indexed helper: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done
for forbidden in \
  'find_module(type_context_modules(' \
  'module_tyaliases(module)' \
  'module_members(module)' \
  'module_imports(module)' \
  'module_aliases(module)' \
  'pick_module_host_type('
do
  case "$resolver_compact" in
    *"$forbidden"*)
      printf 'dyn_load_prime alias resolver restored a whole-module scan: %s\n' "$forbidden" >&2
      exit 1
      ;;
    *) ;;
  esac
done
for required in \
  'mk_indexed_type_context(' \
  'exact_type_index_aliases(' \
  'type_context_find_module(' \
  'type_context_members(' \
  'alias_refs_has(processed,' \
  'identity_alias_catalog_insert_refs('
do
  case "$catalog_compact" in
    *"$required"*) ;;
    *)
      printf 'dyn_load_prime alias catalog bypasses indexed wiring: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done
for forbidden in \
  'find_module(modules,' \
  'module_members(' \
  'tyaliases_find(module_tyaliases('
do
  case "$catalog_compact" in
    *"$forbidden"*)
      printf 'dyn_load_prime alias catalog restored a declaration-list scan: %s\n' "$forbidden" >&2
      exit 1
      ;;
    *) ;;
  esac
done
for required in \
  'modules_have_identity_member_namespace(ms)' \
  'modules_have_type_spelling_overlap(ms)' \
  'exact_type_index_for_modules(ms)' \
  'identity_alias_catalog_for_modules(,ordered,ms,types,type_index)' \
  'ntab_for_module_with_exact_type_origins(,module,ms,types,type_index,catalog)' \
  'ntab_for_module(module,ms)'
do
  case "$load_compact" in
    *"$required"*) ;;
    *)
      printf 'dyn_load_prime resolve callback does not share its exact type index: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done
for required in \
  'pnames_has(seen,name)' \
  'leq_i32(32(I32),count)' \
  'pnames_cons(name,seen)' \
  'type_spelling_scan_names(module_typenames(module),initial)' \
  'type_spelling_scan_members(module_members(module),with_names)' \
  'type_spelling_scan_imports(module_imports(module),with_members)' \
  'if!module_has_type_spelling_overlap(module){widen_sum!(.t(Bool),Modules|Bool)}'
do
  required_compact=$(printf '%s' "$required" | tr -d '[:space:]') || exit
  case "$type_overlap_compact" in
    *"$required_compact"*) ;;
    *)
      printf 'dyn_load_prime type-origin overlap predicate lost bounded wiring: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done
case "$type_overlap_compact" in
  *'fntype_spelling_scan_add(scan:Type_spelling_scan,name:String)->Type_spelling_scan{match!scan{.(seen:Type_spelling_presence,count:I32,overlap:Bool){if!overlap{scan}elseif!pnames_has(seen,name){(seen,(count,.t(Bool)))}else{letat_limit=leq_i32(32(I32),count);(pnames_cons(name,seen),(add_i32(count,1(I32)),at_limit))}}}}'*) ;;
  *)
    printf 'dyn_load_prime type-origin overlap predicate must preserve equality and bounded fallback\n' >&2
    exit 1
    ;;
esac
case "$type_overlap_compact" in
  *'fnmodule_has_type_spelling_overlap(module:Module)->Bool{letinitial=(pnames_nil(),(0(I32),.f(Bool)));letwith_names=type_spelling_scan_names(module_typenames(module),initial);letwith_members=type_spelling_scan_members(module_members(module),with_names);match!type_spelling_scan_imports(module_imports(module),with_members){.(_seen:Type_spelling_presence,_count:I32,overlap:Bool){overlap}}}fnmodules_have_type_spelling_overlap(modules:Modules)->Bool{loop(,.(remaining:Modules){match!un_modules(remaining){(,.(module:Module,rest:Modules){if!module_has_type_spelling_overlap(module){widen_sum!(.t(Bool),Modules|Bool)}else{widen_sum!(rest,Modules|Bool)}},.(_empty:.){widen_sum!(.f(Bool),Modules|Bool)})}},modules)}'*) ;;
  *)
    printf 'dyn_load_prime type-origin overlap predicate must preserve firing and no-op arms\n' >&2
    exit 1
    ;;
esac
# shellcheck disable=SC2016 # Backticks are literal diagnostic text.
for required in \
  'validate_visible_type_binding_origins(module,index)' \
  'validate_type_spelling_names(module_typenames(module),owner,empty)' \
  'validate_type_spelling_members(module_members(module),owner,with_names)' \
  'validate_type_spelling_imports(,module_imports(module),owner,index,with_members)' \
  'if!exact_type_index_has_type_binding(index,provider,name){' \
  'validate_type_spelling_occurrence(seen,owner,name)' \
  'validate_type_spelling_occurrence(seen,owner,member_name(member))' \
  'typeValidated_type_spellings=Exact_key_catalog(Bool);' \
  'match!exact_key_catalog_find(seen,owner,name){(,.(_prior:Bool){widen_sum!(,mk_diag(string_concat(' \
  '"`introducesvisibletypebinding`"(String)' \
  'string_concat(name,"`morethanonce"(String))' \
  '.(_first:.){widen_sum!(,exact_key_catalog_insert(seen,owner,name,.t(Bool)),Type_spelling_validation)}'
do
  case "$projection_compact" in
    *"$required"*) ;;
    *)
      printf 'dyn_load_prime visible type validation lost strict occurrence wiring: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done
# shellcheck disable=SC2016 # Backticks are literal diagnostic text.
for required in \
  'identity_alias_catalog_find(catalog,module_name(owner),tyalias_name(alias))' \
  'mk_indexed_type_context(,modules,types,module,2147483647(I32),no_recursive_type_scope(),index)' \
  'resolve_exact_type_origin_cached(,consumer_context,spelling,' \
  'letterminal=identity_alias_member_terminal(resolved);' \
  'type_origin_eq(origin_terminal_origin(terminal),visible_origin)' \
  'visible_type_binding_origin_diag(consumer,spelling)' \
  '"`hasvisibletypebindingsnamed`"(String)' \
  '"`thatdonotshareonedeclarationorigin"(String)' \
  'letinitial=ntab_for_module(module,modules);'
do
  case "$projection_compact" in
    *"$required"*) ;;
    *)
      printf 'dyn_load_prime alias Ntab projection lost exact-origin reconciliation: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done
for forbidden in \
  'find_module(modules,' \
  'members_find(module_members(provider),' \
  'Exact_key_catalog(Type_origin)' \
  'Visible_ntab_origins' \
  'visible_literal_origins_for_module' \
  'mk_indexed_type_context(,modules,types,owner,tyalias_decl_pos(alias)'
do
  case "$projection_compact" in
    *"$forbidden"*)
      printf 'dyn_load_prime alias Ntab projection restored a singular/raw origin lookup: %s\n' "$forbidden" >&2
      exit 1
      ;;
    *) ;;
  esac
done
for required in \
  'visible_newtypes:Module->Ntab|Diag' \
  'widen_sum!(diag,(Modules&Gdecls)|Gdecls|Diag)' \
  '.(diag:Diag){widen_sum!(diag,Gdecls|Diag)}'
do
  case "$flatten_compact" in
    *"$required"*) ;;
    *)
      printf 'dyn_load_prime flattening no longer propagates alias-origin diagnostics: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done
for required in \
  'identity_alias_target(alias)' \
  '.(_target:String){widen_sum!(.t(Bool),Tyaliases|Bool)}' \
  '.(_not_identity:.){widen_sum!(rest,Tyaliases|Bool)}' \
  '.(_empty:.){widen_sum!(.f(Bool),Tyaliases|Bool)}' \
  'if! tyaliases_have_identity_member_namespace(module_tyaliases(module))' \
  'widen_sum!(.t(Bool),Modules|Bool)' \
  'widen_sum!(rest,Modules|Bool)' \
  '.(_empty:.){widen_sum!(.f(Bool),Modules|Bool)}'
do
  required_compact=$(printf '%s' "$required" | tr -d '[:space:]') || exit
  case "$identity_shape_compact" in
    *"$required_compact"*) ;;
    *)
      printf 'dyn_load_prime identity-alias shape gate lost a firing/no-op arm: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done
case "$load_compact" in
  *'letneeds_exact_type_index=if!modules_have_identity_member_namespace(ms){.t(Bool)}else{modules_have_type_spelling_overlap(ms)};if!needs_exact_type_index{lettype_index=exact_type_index_for_modules(ms);letcatalog=identity_alias_catalog_for_modules(,ordered,ms,types,type_index);.(module:Module){ntab_for_module_with_exact_type_origins(,module,ms,types,type_index,catalog)}}else{.(module:Module){widen_sum!(,ntab_for_module(module,ms),Ntab|Diag)}}}'*) ;;
  *)
    printf 'dyn_load_prime must build exact type indexes only for aliases or visible type-origin overlap\n' >&2
    exit 1
    ;;
esac

# Consecutive declarations in one module reuse the exact advanced scope. An
# owner/cutoff discontinuity or a source-local host function in the intervening
# range must rebuild it. The worked image below supplies the semantic fallback
# witness; this private wiring guard keeps the measured reuse arm live.
for required in \
  'labels{fn_scope_cache:String&I32&Scope}' \
  'if!local_hostfn_between(modules,owner,saved_cutoff,cutoff){rebuild_fn_scope(processed_rev,g,hostfns,modules)}else{saved_scope}' \
  'letscope=scope_for_gdecl(processed_rev,cache,g,hostfns,ms);' \
  'advance_scope(g,scope)' \
  'terms_acc_cons(Dr_ok.get(t),acc),next_cache'
do
  case "$scope_cache_compact" in
    *"$required"*) ;;
    *)
      printf 'dyn_load_prime declaration-scope reuse lost a firing/fallback edge: %s\n' "$required" >&2
      exit 1
      ;;
  esac
done

load_calls=$(count_occurrences 'load_package(' "$main_source") || exit
default_calls=$(count_occurrences 'instantiate_default(' "$main_source") || exit
callback_calls=$(count_occurrences 'instantiate(' "$main_source") || exit
checked_calls=$(count_occurrences 'instantiate_checked_default(' "$main_source") || exit
ordinary_flows=$(count_occurrences 'flow(default_loaded,' "$main_source") || exit
if [ "$load_calls" -ne 1 ] \
  || [ "$default_calls" -ne 1 ] \
  || [ "$callback_calls" -ne 1 ] \
  || [ "$checked_calls" -ne 1 ] \
  || [ "$ordinary_flows" -ne 14 ] \
  || ! grep -Eq '^[[:space:]]*match![[:space:]]+un_load_outcome\(load_package\(guest_image\(\)\)\)[[:space:]]*[{]$' "$main_source" \
  || ! grep -Eq '^[[:space:]]*let[[:space:]]+default_loaded[[:space:]]*=[[:space:]]*instantiate_default\(image\);$' "$main_source" \
  || ! grep -Eq '^[[:space:]]*match![[:space:]]+un_inst\(instantiate\($' "$main_source" \
  || ! grep -Eq '^[[:space:]]*match![[:space:]]+un_inst\(instantiate_checked_default\(image,[[:space:]]*good_contract\(\)\)\)[[:space:]]*[{]$' "$main_source"; then
  printf 'dyn_load_prime worked example must load once and reuse its three instance paths\n' >&2
  exit 1
fi

signature=$("$KIO_BIN" sig stage --force --stdout) || exit
loader_scan_signature=$(printf '%s\n' "$signature" | awk '
  /^[[:space:]]*module loader\/scan \{$/ { inside = 1 }
  inside { print }
  inside && /^[[:space:]]*}$/ { exit }
')
if [ -z "$loader_scan_signature" ] \
  || ! printf '%s\n' "$loader_scan_signature" | grep -Fq 'pub fn load_package('; then
  printf 'dyn_load_prime signature is missing loader/scan.load_package\n' >&2
  exit 1
fi
for private in resolve_package scan_package un_pscan_outcome pscan_; do
  if printf '%s\n' "$signature" | grep -Fiq "$private"; then
    printf 'dyn_load_prime signature exposes private loader detail: %s\n' "$private" >&2
    exit 1
  fi
done

"$KIO_BIN" check >/dev/null || exit
"$KIO_BIN" test >/dev/null || exit
"$KIO_BIN" build "$KIO_TARGET" || exit
"$KIO_RUNNER" --protocol testapi-dyn-load out/"$KIO_TARGET"
