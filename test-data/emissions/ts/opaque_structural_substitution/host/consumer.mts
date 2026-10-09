import {
  createOpaqueSubstitution,
  type OpaqueSubstitutionHost,
  type OpaqueSubstitutionTypeLambda,
} from "../workdir/out/ts/opaque_substitution.js";

type Box<T> = { readonly value: T; readonly same: (value: T) => T };
interface BoxLambda extends OpaqueSubstitutionTypeLambda<readonly [unknown]> {
  readonly type: Box<this["arguments"][0]>;
}
interface Bindings {
  readonly api: { readonly Box: BoxLambda };
}
function box<T>(value: T): Box<T> {
  return { value, same: value => value };
}
const host: OpaqueSubstitutionHost<Bindings> = {
  api: { unbox: <T,>(value: Box<T>): T => value.value },
};
const pkg = createOpaqueSubstitution<Bindings>(host);

type Pair = { _0: number; _1: boolean };
type Product = { _0: string; _1: Pair };
type Choice = { _0: number } | { _1: boolean };
type Sum = { _0: string } | { _1: Choice };

const failures: string[] = [];
function checkPair(label: string, value: Pair): void {
  if (value._0 !== 7) failures.push(`${label}: number payload changed`);
  if (value._1 !== true) failures.push(`${label}: flag payload changed`);
}
checkPair("generic host extraction", pkg.api.readProduct(box<Pair>({ _0: 7, _1: true })));

const product = box<Product>({ _0: "head", _1: { _0: 7, _1: true } });
const genericProduct: Box<Product> = pkg.api.product<string, Pair>(pkg.api.directProduct(product));
const directProduct: Box<Product> = pkg.api.directProduct(genericProduct);
if (genericProduct !== product || directProduct !== product) {
  failures.push("product container identity changed");
}
if (directProduct.value._0 !== "head") failures.push("product head changed");
checkPair("product right tail", directProduct.value._1);

const alternatives: readonly Sum[] = [
  { _0: "head" },
  { _1: { _0: 7 } },
  { _1: { _1: true } },
];
for (const [index, value] of alternatives.entries()) {
  const input = box<Sum>(value);
  const generic: Box<Sum> = pkg.api.sum<string, Choice>(pkg.api.directSum(input));
  const direct: Box<Sum> = pkg.api.directSum(generic);
  if (generic !== input || direct !== input) failures.push(`sum ${index}: container identity changed`);
  const result = direct.value;
  if (index === 0) {
    if (!("_0" in result) || result._0 !== "head") failures.push("sum head changed");
  } else if (!("_1" in result)) {
    failures.push(`sum ${index}: right tail lost`);
  } else if (index === 1) {
    if (!("_0" in result._1) || result._1._0 !== 7) failures.push("sum number changed");
  } else if (!("_1" in result._1) || result._1._1 !== true) {
    failures.push("sum flag changed");
  }
}
if (failures.length !== 0) throw new Error(failures.join("\n"));
console.log("opaque product and sum substitution preserved");
