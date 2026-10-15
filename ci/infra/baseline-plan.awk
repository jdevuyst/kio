# Input rows retain run-tests.sh's ten-field applicability relation. Append
# the baseline, selected environment, and whether its copied manifest changes.
function hash(text,    i, c, value) {
  value = 5381
  for (i = 1; i <= length(text); i++) {
    c = ord[substr(text, i, 1)]
    if (c == "") c = 1
    value = (value * 33 + c) % 2147483647
  }
  return value
}
function emit(row, baseline, environment, rewrite) {
  print row "\t" baseline "\t" environment "\t" rewrite
}
BEGIN { for (i = 32; i < 127; i++) ord[sprintf("%c", i)] = i }
FILENAME == table {
  if ($0 ~ /^#/ || $0 == "") next
  split($0, fields, / +/)
  backend = fields[1]
  index_ = ++count[backend]
  baselines[backend, index_] = fields[2]
  minimum[backend, index_] = fields[3]
  current[backend, index_] = fields[4]
  role[backend] = fields[5]
  next
}
FILENAME == policies { policy[$1] = $2; next }
{
  backend = $7
  # Kio' is a phase artifact, and preserved fixtures may deliberately have no
  # valid baseline. Their written selectors remain their own test subject.
  if (!(backend in count)) { emit($0, "phase-artifact", "none", 0); next }
  if (policy[$3] != "parametric" && policy[$3] != "current-only") {
    emit($0, "source-preserved", current[backend, 1], 0)
    next
  }
  name = substr($3, length(cases_dir) + 2)
  key = scope SUBSEP seed SUBSEP name SUBSEP backend
  first = 1
  last = count[backend]
  if (mode == "sample") first = last = (hash(key SUBSEP "baseline") % last) + 1
  for (index_ = first; index_ <= last; index_++) {
    environment = current[backend, index_]
    # JS's fixed runner embeds Cargo-locked QuickJS. Node installations do not
    # supply ordinary JS runtime evidence. Historical runtime selection is
    # restricted to trusted ordinary fixtures; custom and contrib stay current.
    if (mode == "sample" && policy[$3] == "parametric" &&
        role[backend] != "locked-quickjs" &&
        minimum[backend, index_] != environment &&
        hash(key SUBSEP baselines[backend, index_] SUBSEP "environment") % 2 == 0)
      environment = minimum[backend, index_]
    emit($0, baselines[backend, index_], environment, 1)
  }
}
