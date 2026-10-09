# Weighted choice audit

A finite weighted-choice algebra audits a toy game's loot and charm draws.
Each entry pairs an arbitrary outcome with an integer multiplicity. Mapping
preserves multiplicities, alternative choices concatenate entries, independent
choices multiply their weights, and conditioning retains matching outcomes
without rounding or normalizing. The reusable list package owns the list
representation and traversals.

The fixed loot bag contains two blades (attack 2), one bow (attack 3), and one
coin pouch (treasure 5). A separate charm draw has two plain charms (bonus 0)
and one bright charm (bonus 1). There is no stdin or random seed: the program
enumerates all weighted possibilities exactly.

The six loot/charm combinations have weights 4, 2, 2, 1, 2, and 1. Therefore
the attack distribution over 0 through 4 is `[0:2, 1:1, 2:4, 3:4, 4:1]`,
with total 12. Conditioning on attack at least 3 leaves four ways to score 3
and one way to score 4. Treasure is zero in nine ways and five in three ways.
The report also maps loadouts to Boolean readiness, exercises empty choices
on both sides of independent composition, and checks a guaranteed singleton
loadout and an impossible condition. All displayed counts are exact
multiplicities; the retained count is the numerator over the original total.

What this adds to the corpus: generic outcome-valued finite choice composition
across a labeled loot sum, loadout records, integers, and Boolean outcomes.
Nested list folds combine independent distributions; imported `match!` and
`if!` handle domain interpretation and reporting. Its subject is a weighted
choice algebra and conditioning audit rather than polynomial coefficients,
mutable-array simulation, or transaction replay.
