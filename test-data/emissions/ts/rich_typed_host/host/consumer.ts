import {
  createRichTypedHost,
  type RichTypedHostApply,
  type RichTypedHostHost,
  type RichTypedHostTypeLambda,
} from "../workdir/out/ts/rich_typed_host.js";

type NativeBox<A> = { readonly Box: A };
interface NativeBoxLambda extends RichTypedHostTypeLambda<readonly [unknown]> {
  readonly type: NativeBox<this["arguments"][0]>;
}
type NativeShadow<A> = {
  readonly Shadow: {
    readonly _0: A;
    readonly _1: <T>(value: T) => T;
  };
};

interface NativeHost {
  readonly api: {
    readonly applyPoly: (f: <T>(value: T) => T) => string;
    readonly roundHkt: <F extends RichTypedHostTypeLambda<readonly [unknown]>>(
      value: RichTypedHostApply<F, readonly [string]>,
    ) => RichTypedHostApply<F, readonly [string]>;
    readonly roundShadow: (
      value: NativeShadow<NativeBox<string>>,
    ) => NativeShadow<NativeBox<string>>;
  };
}

const nativeHost: NativeHost = {
  api: {
    applyPoly: f => f("rank-N"),
    roundHkt: value => value,
    roundShadow: value => value,
  },
};

const generatedHost: RichTypedHostHost = nativeHost;
const generatedAsNative: NativeHost = generatedHost;
const pkg = createRichTypedHost(generatedHost);

if (pkg.api.poly() !== "rank-N") {
  throw new Error("rank-N host callback lost its result");
}

const boxed = pkg.api.KioType_Box.makeBox("hkt");
const hktRoundTrip: RichTypedHostApply<NativeBoxLambda, readonly [string]> =
  pkg.api.hkt(boxed);
if (pkg.api.KioType_Box.readBox(hktRoundTrip) !== "hkt") {
  throw new Error("constructor-kinded host relation lost its payload");
}
const shadow = pkg.api.KioType_Shadow.makeShadow({
  _0: boxed,
  _1: <T>(value: T): T => value,
});
const shadowPayload = pkg.api.KioType_Shadow.readShadow(pkg.api.shadow(shadow));
if (pkg.api.KioType_Box.readBox(shadowPayload._0) !== "hkt") {
  throw new Error("nested generic application lost its payload");
}
if (shadowPayload._1(37) !== 37) {
  throw new Error("nested rank-N payload lost its relation");
}

const packed = pkg.api.pack({ witness: "existential" });
const opened = pkg.api.KioType_Pack.openPack(packed)(value =>
  typeof value === "object" && value !== null ? "opened" : "wrong payload",
);
if (opened !== "opened") {
  throw new Error("existential CPS projector lost its payload");
}

const recursiveBase = pkg.api.KioType_Recursive.makeRecursive({ _0: null });
const recursive = pkg.api.KioType_Recursive.makeRecursive({ Recursive: recursiveBase });
const recursiveView = pkg.api.KioType_Recursive.readRecursive(recursive);
if (!("Recursive" in recursiveView)) {
  throw new Error("recursive nominal value lost its recursive arm");
}

void generatedAsNative;
console.log("rich typed host ok");
