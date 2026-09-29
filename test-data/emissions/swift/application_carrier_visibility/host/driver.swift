import CarrierControls

struct One: CarrierControlsHost {}
let pkg = createCarrierControls(host: One())

let opaque = pkg.api.opaque(42, "opaque")
let transported: KioNewtype_api__Opaque<One, Int, String> =
    pkg.KioNewtypeApplication_api__Opaque_project(
        pkg.KioNewtypeApplication_api__Opaque_lift(opaque))

let input = pkg.api.KioType_Both.make(Product(_0: KioUnit(), _1: "partial"))
let flat: KioApply2<KioNewtypeMk_api__Both<One>, KioUnit, String> =
    pkg.KioNewtypeApplication_api__Both_lift(input)
let grouped: KioApply1<KioApply1<KioNewtypeMk_api__Both<One>, KioUnit>, String> = flat
let mapping = pkg.api.KioType_Using.read(pkg.api.partial())
let output = pkg.KioNewtypeApplication_api__Both_project(mapping.call(grouped))
precondition(pkg.api.KioType_Both.read(output)._1 == "partial")

let nativeConstructor = KioHostTypeMk_api__Native.constructor.applying(Int.self)
let native: KioApply2<KioHostTypeMk_api__Native, Int, String> =
    nativeConstructor.lift([1, 2, 3])
precondition(nativeConstructor.project(native, as: [Int].self) == [1, 2, 3])
let carrier = KioHostType_api__Native<Int, String>([4, 5])
precondition(pkg.api.echoNative(carrier).value(as: [Int].self) == [4, 5])
print("application carriers ok")
