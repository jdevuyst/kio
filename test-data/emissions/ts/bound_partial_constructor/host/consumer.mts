import {
  createBoundPartial,
  type BoundPartialApply,
  type BoundPartialBind,
  type BoundPartialTypeLambda,
} from "../workdir/out/ts/bound_partial.js";

type Pair<A, B> = { _0: A; _1: B };
interface PairLambda extends BoundPartialTypeLambda<readonly [unknown, unknown]> {
  readonly type: Pair<this["arguments"][0], this["arguments"][1]>;
}
type Wrapped<G extends BoundPartialTypeLambda<readonly [unknown]>> = {
  Wrap: BoundPartialApply<G, readonly [null]>;
};
const pkg = createBoundPartial();
type StringPair = BoundPartialBind<PairLambda, readonly [string]>;
const input: Wrapped<StringPair> = pkg.api.KioType_Wrap.wrap<StringPair>({ _0: "head", _1: null });
const output: Wrapped<StringPair> = pkg.api.keep<PairLambda, string>(input);
const payload: Pair<string, null> = pkg.api.KioType_Wrap.unwrap<StringPair>(output);
if (payload._0 !== "head" || payload._1 !== null) {
  throw new Error("partial constructor lost its selected index or payload");
}
const full: Pair<string, boolean> = pkg.api.saturated<PairLambda, string, boolean>({ _0: "head", _1: true });
if (full._0 !== "head" || full._1 !== true) throw new Error("saturated constructor changed");
// @ts-expect-error the captured prefix remains string, not number.
const wrong: Wrapped<BoundPartialBind<PairLambda, readonly [number]>> = output;
void wrong;
console.log("bound partial constructor preserved");
