import {
  createDemo,
  type DemoApply,
  type DemoHost,
  type DemoHostTypes,
  type DemoTypeLambda,
} from "../workdir/out/ts/demo.js";
import { createB } from "../workdir/b/out/ts/b.js";
import {
  createPropertyKey,
  type PropertyKeyHost,
} from "../workdir/property_key/out/ts/property_key.js";
import {
  createNonNullable,
  type NonNullableHost,
} from "../workdir/non_nullable/out/ts/non_nullable.js";

interface Bindings extends DemoHostTypes {
  readonly api: { readonly Token: { readonly token: string } };
}

interface ArrayK extends DemoTypeLambda<readonly [unknown]> {
  readonly type: readonly this["arguments"][0][];
}

interface PairK extends DemoTypeLambda<readonly [unknown, unknown]> {
  readonly type: readonly [this["arguments"][0], this["arguments"][1]];
}

let calls = 0;
const host: DemoHost<Bindings> = {
  api: {
    use: (value, callback) => {
      if (callback(null) !== null) throw new Error("callback unit changed");
      calls += 1;
      return value;
    },
    useControl: (value, callback) => {
      if (callback(null) !== null) throw new Error("control unit changed");
      calls += 1;
      return value;
    },
  },
};
const pkg = createDemo(host);
const token = { token: "opaque" };
interface LegacyBindings {
  readonly api: { readonly Token: typeof token };
}
const propertyKeyHost: PropertyKeyHost<LegacyBindings> = { api: { old: value => value } };
const nonNullableHost: NonNullableHost<LegacyBindings> = { api: { old: value => value } };
if (
  createB().api.keep(token) !== token ||
  createPropertyKey().api.keep(token) !== token ||
  createNonNullable().api.keep(token) !== token ||
  propertyKeyHost.api?.old?.(token) !== token ||
  nonNullableHost.api?.old?.(token) !== token
) {
  throw new Error("package names captured factory or intrinsic types");
}
if (pkg.api.callable(token) !== token || pkg.api.control(token) !== token || calls !== 2) {
  throw new Error("alpha-renamed callables lost identity or invocation");
}

const identity = { advance: <A>() => (value: A): A => value };
if (pkg.api.nested(token, identity) !== token) {
  throw new Error("nested binders captured an outer type");
}

const values: DemoApply<ArrayK, readonly [string]> = ["higher"];
const returned: readonly string[] = pkg.api.higher<ArrayK, string>(values);
if (returned !== values || returned[0] !== "higher") {
  throw new Error("higher-kinded application lost its identity");
}
const pair = { Wrap: ["partial", null] as const };
const partial: { Wrap: readonly [string, null] } = pkg.api.partial<PairK, string>(pair);
if (partial !== pair || partial.Wrap[0] !== "partial") {
  throw new Error("partial constructor lost its relation");
}

const nested = pkg.api.KioType_Nested.makeNested({
  _0: token,
  _1: { advance: <A>() => (_value: A) => token },
});
const payload = pkg.api.KioType_Nested.readNested(pkg.api.round(nested));
if (payload._0 !== token || payload._1.advance<number>()(17) !== token) {
  throw new Error("newtype substitution captured a free outer binder");
}
const bindings = pkg.api.binding(token, "separate", token);
if (bindings._0 !== token || bindings._1 !== "separate") {
  throw new Error("source binders captured the host binding map");
}

function typeErrorsRemainChecked(): void {
  // @ts-expect-error An unrelated result type cannot replace the generic input.
  const wrong: string = pkg.api.callable(token);
  // @ts-expect-error Constructor arguments retain their selected element type.
  pkg.api.higher<ArrayK, number>(values);
  // @ts-expect-error The nested function retains its outer result type.
  const wrongNested: number = payload._1.advance<string>()("input");
  void wrong;
  void wrongNested;
}
void typeErrorsRemainChecked;
console.log("type binder hygiene ok");
