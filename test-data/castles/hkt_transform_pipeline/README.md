# hkt_transform_pipeline

This castle models a fixed transform pipeline for form events. Three checked-in
events are normalized, scored, routed, and summarized with the higher-kinded
instance dictionaries from the `hkt` POC package. The package has no stdin; the
fixture is the source-level event set in `pipeline/stages.kio`.

The program prints two summaries. The first runs the pipeline through the HKT
`Box` brand as an audited path, and the second runs the same staged
transformations through `Identity` as a baseline path. Each summary reports the
transformed samples, the total score, and the number of approved events.

What this adds to the corpus: this is a dependency-integrated castle that uses
a POC package as a library rather than copying its demonstration. It stresses
kind-`*->*` instance dictionaries, polymorphic newtype payloads, dependency
imports, do-block lowering, labels/records, host arithmetic, boolean routing,
and string rendering across the js, ts, rust, go, swift, and haskell backends.
