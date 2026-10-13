import NativeViews

final class Host: NativeViewsHost {
    var transfers = 0
    func api__transfer<T>(_ value: T) -> T {
        transfers += 1
        // This is an ordinary native callback stored through an opaque generic
        // operation; callers need no generated private representation knowledge.
        precondition(value is (KioUnit) -> String)
        return value
    }
}

let host = Host()
let pkg = createNativeViews(host: host)
var calls = 0
let native: (KioUnit) -> KioUnit = { _ in calls += 1; return KioUnit() }

// The same public nominal index is constructed under an external generic T
// and later consumed at the concrete function type inside the package.
let supplied = pkg.api.KioType_Saved.save(native)
pkg.api.invoke(supplied)
pkg.api.invoke(supplied)
precondition(calls == 2)

// Reverse direction: a guest closure behind the generic nominal projector is
// still an ordinary Swift closure, including after generic forwarding.
let captured = pkg.api.captured("captured")
let projected: (KioUnit) -> String = pkg.api.KioType_Saved.get(captured)
let forwarded: (KioUnit) -> String = pkg.api.recover(captured)
precondition(projected(KioUnit()) == "captured")
precondition(forwarded(KioUnit()) == "captured")

let aggregate: Product<(KioUnit) -> String, String> =
    pkg.api.KioType_Saved.get(pkg.api.structured("aggregate"))
precondition(aggregate._0(KioUnit()) == "aggregate" && aggregate._1 == "aggregate")
let returned: (KioUnit) -> String = pkg.api.returned("returned")
precondition(returned(KioUnit()) == "returned")

// Unknown externally supplied T is not inspected to choose a guest witness.
let opaque: Any = native
let opaqueResult: Any = pkg.api.identity(opaque)
let opaqueCall = opaqueResult as! (KioUnit) -> KioUnit
_ = opaqueCall(KioUnit())
let nativeProduct = Product(_0: native, _1: "native")
let productResult = pkg.api.identity(nativeProduct)
_ = productResult._0(KioUnit())
precondition(productResult._1 == "native" && calls == 4)

let roundtrip: (KioUnit) -> String =
    pkg.api.KioType_Saved.get(pkg.api.throughHost("roundtrip"))
precondition(roundtrip(KioUnit()) == "roundtrip" && host.transfers == 1)
print("native callable views ok")
