# Term unification workbench

This program unifies first-order terms built from numbered variables and
function symbols with ordered argument lists. `v1` names a variable;
`f20(v1,f40())` is a binary application containing a variable and a constant.
The fixtures are constructed in Kio and require no stdin.

The solver processes a persistent worklist of equations. It substitutes known
bindings before each comparison, orients variable equations, rejects cyclic
bindings with an occurs check, and decomposes matching applications into child
equations. Adding a binding substitutes it through all earlier bindings, so
the final substitution has no unresolved references to another bound variable.

The transcript includes two successful problems, a repeated-variable symbol
conflict, direct and indirect occurs failures, and an arity conflict. The nested
problem first binds `v1` to `f30(v3)` and later binds `v3` to `f50()`, requiring
composition inside an existing term. For each success the workbench applies
the completed substitution separately to both original terms, prints both
results, and checks structural equality independently of the solver's worklist.
Domain failures are values in the solver's outcome sum and leave the program's
exit status at zero.

`terms` owns recursive syntax, rendering, occurrence traversal, and structural
equality. `substitutions` owns lookup and composition, rebuilding terms with
an explicit stack of parent applications; `unifier` owns the
equation worklist; `workbench` renders outcomes. The `list` dependency supplies
the term argument lists, equation queue, substitution table, folds, mapping,
and concatenation. Its host operations are rehosted through `list_host`.
The direct `elab` dependency supplies ordinary conditional, matching, and sum
construction forms.

What this adds to the corpus: a symbolic solver combining recursive terms,
persistent equation processing, substitution composition, explicit failure
sums, and a separate post-solve equality check. Nested substitutions and
indirect cycles make correctness depend on the interaction of these modules.
