import {
  createTypeOnlyExports,
  type TypeOnlyExportsCallable,
  type TypeOnlyExportsHost,
  type TypeOnlyExportsPoly,
} from "../workdir/out/ts/type_only_exports.js";

const host: TypeOnlyExportsHost = {
  testapi_main: {
    observe: () => null,
    consume: () => null,
  },
};
const main = createTypeOnlyExports(host).testapi.KioModule_main;

const direct: <A>() => TypeOnlyExportsCallable<A, A> = main.direct;
const delayed: <A>() => TypeOnlyExportsCallable<A, A> = main.delayed;
const ordinary: <A>(value: A) => A = main.ordinary;
const poly: <A>() => TypeOnlyExportsPoly<<B>() => TypeOnlyExportsCallable<B, B>> = main.poly;

const stringIdentity = main.direct<string>();
const text: string = stringIdentity("direct-string");
const numberIdentity = main.direct<number>();
const number: number = numberIdentity(42);
const unitControl: string = main.delayed<string>()("unit-string");
const ordinaryControl: string = main.ordinary<string>("ordinary-string");
const polymorphicIdentity = main.poly<string>();
const polyText: string = polymorphicIdentity.advance<string>()("poly-string");
const polyNumber: number = polymorphicIdentity.advance<number>()(73);
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

const captured: TypeOnlyExportsPoly<<A>() => TypeOnlyExportsPoly<<B>() => TypeOnlyExportsCallable<{ _0: A; _1: B }, A>>> =
  {
    advance: <A>() => ({
      advance: <B>() => (value: { _0: A; _1: B }): A => value._0,
    }),
  };
const selected = captured.advance<string>().advance<number>();
const selectedText: string = selected({ _0: "outer", _1: 5 });
// @ts-expect-error the outer binder remains string after advancing the inner binder.
selected({ _0: 5, _1: 5 });
// @ts-expect-error the independently selected inner binder remains number.
selected({ _0: "outer", _1: "inner" });
// @ts-expect-error a fixed string stage cannot implement a universally selected identity stage.
const fixedStage: TypeOnlyExportsPoly<<A>() => TypeOnlyExportsCallable<A, A>> = { advance: () => (value: string) => value };
const unit: TypeOnlyExportsCallable<null, null> = (value: null): null => value;
unit(null);
// @ts-expect-error first-class Unit retains its explicit semantic domain.
unit();
void selectedText;
void fixedStage;
