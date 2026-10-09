import CarrierControls

struct One: CarrierControlsHost {}
let pkg = createCarrierControls(host: One())

let opaque = pkg.api.opaque(42, "opaque")
let transported: KioNewtype_api__Opaque<One, Int, String> = opaque

let input = pkg.api.KioType_Both.make(Product(_0: KioUnit(), _1: "partial"))
let flat: KioApply2<KioNewtypeMk_api__Both<One>, KioUnit, String> = input
let grouped: KioApply1<KioApply1<KioNewtypeMk_api__Both<One>, KioUnit>, String> = flat
let mapping = pkg.api.KioType_Using.read(pkg.api.partial())
let output = mapping.specialize(asKioType: String.self)(grouped)
precondition(pkg.api.KioType_Both.read(output)._1 == "partial")

let nativeConstructor = KioHostTypeMk_api__Native.constructor.applying(Int.self)
let native: KioApply2<KioHostTypeMk_api__Native, Int, String> =
    nativeConstructor.lift([1, 2, 3])
precondition(nativeConstructor.project(native, as: [Int].self) == [1, 2, 3])
let carrier = KioHostType_api__Native<Int, String>([4, 5])
precondition(pkg.api.echoNative(carrier).value(as: [Int].self) == [4, 5])

typealias Atom = KioHostType_api__Native<KioUnit, KioUnit>

struct ProductStorage {
    let value: Exp_api__productValue_ret
}

struct SumStorage {
    let value: Exp_api__sumValue_ret
}

func checkProduct(_ value: Exp_api__productValue_ret) {
    precondition(value._0.value(as: Int.self) == 11)
    precondition(value._1._0.value(as: Int.self) == 22)
    precondition(value._1._1.value(as: Int.self) == 33)
}

func checkSum(_ value: Exp_api__sumValue_ret, _ index: Int) {
    switch value {
    case ._0(let atom): precondition(index == 0 && atom.value(as: Int.self) == 11)
    case ._1(._0(let atom)): precondition(index == 1 && atom.value(as: Int.self) == 22)
    case ._1(._1(let atom)): precondition(index == 2 && atom.value(as: Int.self) == 33)
    }
}

var events: [String] = []
let payload = pkg.api.productValue(Atom(11), Atom(22), Atom(33))
let packProduct: (Exp_api__productValue_ret) -> Exp_api__concreteProductBox_arg0 = { value in
    checkProduct(value)
    events.append("pack-product")
    return .init(ProductStorage(value: value))
}
let readProduct: (Exp_api__concreteProductBox_arg0) -> Exp_api__productValue_ret = { value in
    let stored = value.value(as: ProductStorage.self)
    checkProduct(stored.value)
    events.append("read-product")
    return stored.value
}
let productResult: Exp_api__productValue_ret =
    pkg.api.concreteProductRoundtrip(packProduct, readProduct, payload)
checkProduct(productResult)
precondition(events == ["pack-product", "read-product"])

let boxConstructor = KioHostTypeMk_api__Box.constructor
let applied: KioApply1<KioHostTypeMk_api__Box, Exp_api__productValue_ret> = boxConstructor.lift(payload)
let projected = boxConstructor.project(pkg.api.abstractBox(applied), as: Exp_api__productValue_ret.self)
checkProduct(projected)

let sums: [Exp_api__sumValue_arg0] = [._0(Atom(11)), ._1(._0(Atom(22))), ._1(._1(Atom(33)))]
for (index, payload) in sums.enumerated() {
    let packSum: (Exp_api__sumValue_arg0) -> Exp_api__concreteSumBox_arg0 = { value in
        checkSum(value, index)
        events.append("pack-sum-\(index)")
        return .init(SumStorage(value: value))
    }
    let readSum: (Exp_api__concreteSumBox_arg0) -> Exp_api__sumValue_ret = { value in
        let stored = value.value(as: SumStorage.self)
        checkSum(stored.value, index)
        events.append("read-sum-\(index)")
        return stored.value
    }
    let result: Exp_api__sumValue_ret = pkg.api.concreteSumRoundtrip(packSum, readSum, payload)
    checkSum(result, index)
}
precondition(events == [
    "pack-product", "read-product",
    "pack-sum-0", "read-sum-0",
    "pack-sum-1", "read-sum-1",
    "pack-sum-2", "read-sum-2",
])
print("application carriers ok")
