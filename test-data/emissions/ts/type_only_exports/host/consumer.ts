import { createTypeOnlyExports, type TypeOnlyExportsHost } from "../workdir/out/ts/type_only_exports.js";

const host: TypeOnlyExportsHost = {
  testapi_main: {
    observe: () => null,
    consume: () => null,
  },
};
const main = createTypeOnlyExports(host).testapi.KioModule_main;

const direct: <A>() => (value: A) => A = main.direct;
const delayed: <A>() => (value: A) => A = main.delayed;
const ordinary: <A>(value: A) => A = main.ordinary;
const poly: <A>() => <B>(value: B) => B = main.poly;

const stringIdentity = main.direct<string>();
const text: string = stringIdentity("direct-string");
const numberIdentity = main.direct<number>();
const number: number = numberIdentity(42);
const unitControl: string = main.delayed<string>()("unit-string");
const ordinaryControl: string = main.ordinary<string>("ordinary-string");
const polymorphicIdentity = main.poly<string>();
const polyText: string = polymorphicIdentity<string>("poly-string");
const polyNumber: number = polymorphicIdentity<number>(73);
main.phantom<string>();
main.phantomUnit<string>();

// @ts-expect-error specialization retains the string parameter.
stringIdentity(42);
// @ts-expect-error specialization retains the number result.
const wrongResult: string = numberIdentity(42);

void text;
void number;
void unitControl;
void ordinaryControl;
void polyText;
void polyNumber;
void wrongResult;
void direct;
void delayed;
void ordinary;
void poly;
