#!/bin/sh
# SUBJECT: Pure call operands, product fields and facade reconstruction carry explicit value demand, including a selected sum result.
# CONTRACT: Call-by-value demand; measured selected-sum payload retention at the ordinary Haskell profile.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.haskell-demand.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

# Check the binding-relative demand edges, not just an unrelated seq in the
# same equation. Names are discovered from the emitted subject, not pinned.
awk '
  function fail(message) { print message; failed = 1 }
  function occurrences(line, token, count, at) {
    count = 0
    while ((at = index(line, token)) != 0) {
      count++
      line = substr(line, at + length(token))
    }
    return count
  }
  function demanded(line, binder, after, before, binding, force) {
    binding = index(line, "let { " binder " = ")
    force = index(line, binder " `seq`")
    return binding > after && force > binding && force < before
  }
  function fields(line, after, label, tail, pair, slots, at) {
    tail = line
    pair = ""
    while (match(tail, /[[:alnum:]_]+, [[:alnum:]_]+/)) {
      pair = substr(tail, RSTART, RLENGTH)
      tail = substr(tail, RSTART + RLENGTH)
    }
    if (split(pair, slots, ", ") != 2 || slots[1] == slots[2]) {
      fail(label ": missing distinct output field bindings")
      return
    }
    at = index(line, "(" pair ")")
    if (!demanded(line, slots[1], after, at) || !demanded(line, slots[2], after, at))
      fail(label ": result field is not demanded before construction")
  }
  function operand(label, producer, line, token, at, rest, binder, binding, rhs) {
    line = equation[label]
    token = name["duplicate"] " _pkg "
    at = index(line, token)
    rest = substr(line, at + length(token))
    match(rest, /^[[:alnum:]_]+/)
    binder = substr(rest, RSTART, RLENGTH)
    binding = index(line, "let { " binder " = ")
    rhs = substr(line, binding, index(line, binder " `seq`") - binding)
    if (!at || !demanded(line, binder, 0, at) ||
        occurrences(line, token) != 1 || occurrences(rhs, name[producer] " _pkg ") != 1)
      fail(label ": computed operand is not demanded once before duplicate")
  }
  /-- Module fn `duplicate`/ { wanted = "duplicate"; next }
  /-- Module fn `choice`/ { wanted = "choice"; next }
  /-- Module fn `nested`/ { wanted = "nested"; next }
  /-- Module fn `select`/ { wanted = "select"; next }
  /-- Module fn `first`/ { wanted = "first"; next }
  wanted != "" && /_pkg/ && /=/ {
    equation[wanted] = $0
    name[wanted] = $1
    seen++
    wanted = ""
  }
  /^export__api__choice _pkg/ { facade = $0; seen++ }
  END {
    if (seen != 6) fail("missing subject equations")
    fields(equation["duplicate"], 0, "product producer")
    operand("choice", "select")
    operand("nested", "first")
    call = name["choice"] " _pkg "
    if (occurrences(facade, call) != 1) fail("result facade: missing single source call")
    fields(facade, index(facade, call), "result facade")
    if (failed) exit 1
  }
' out/haskell/PureDemand.hs
