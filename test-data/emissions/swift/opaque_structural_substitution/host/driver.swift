import OpaqueSubstitution

struct HostBox<T> {
    fileprivate let payload: T
    init(_ payload: T) { self.payload = payload }
}

final class Host: OpaqueSubstitutionHost {
    typealias api__Number = Int32
    typealias api__Flag = Bool
    typealias elab_testapi__String = String
    typealias elab_testapi__I32 = Int32
    typealias elab_testapi__Int = Int32
    typealias elab_testapi__Bool = Bool

    var unboxCalls = 0
    var consumed: [(Int32, Bool)] = []
    var packCalls = 0
    var typeStages = 0
    var valueStages = 0

    func api__unbox<T>(_ boxed: KioHostType_api__Box<T>) -> T {
        unboxCalls += 1
        return boxed.value(as: HostBox<T>.self).payload
    }

    func api__consume(_ number: Int32, _ flag: Bool) -> Int32 {
        consumed.append((number, flag))
        return number
    }

    func api__pack<T>(_ value: T) -> KioHostType_api__Box<T> {
        packCalls += 1
        return .init(HostBox(value))
    }

    func api__typeStage() { typeStages += 1 }
    func api__valueStage() { valueStages += 1 }
}

let host = Host()
let pkg = createOpaqueSubstitution(host: host)
let direct: KioHostType_api__Box<Product<Int32, Bool>> =
    .init(HostBox(Product<Int32, Bool>(_0: 173, _1: true)))
let relayed: KioHostType_api__Box<Product<Int32, Bool>> =
    .init(HostBox(Product<Int32, Bool>(_0: 291, _1: false)))

precondition(pkg.api.direct(direct) == 173)
precondition(pkg.api.throughRelay(relayed) == 291)
precondition(host.unboxCalls == 2)
precondition(host.consumed.count == 2)
precondition(host.consumed[0].0 == 173 && host.consumed[0].1)
precondition(host.consumed[1].0 == 291 && !host.consumed[1].1)

let product: KioHostType_api__Box<Product<Int32, Bool>> = pkg.api.productOutput(413, true)
let productValue = product.value(as: HostBox<Product<Int32, Bool>>.self).payload
precondition(productValue._0 == 413 && productValue._1)

// The unselected alternative has no payload from which its native type could
// be guessed. Both exact host-owned boxes must still contain the right sum.
let absentBool: KioHostType_api__Box<Sum<Int32, Bool>> = pkg.api.leftBox(Int32(517))
let absentString: KioHostType_api__Box<Sum<Int32, String>> = pkg.api.leftBox(Int32(619))
switch absentBool.value(as: HostBox<Sum<Int32, Bool>>.self).payload {
case ._0(let value): precondition(value == 517)
case ._1: preconditionFailure("wrong Bool sum branch")
}
switch absentString.value(as: HostBox<Sum<Int32, String>>.self).payload {
case ._0(let value): precondition(value == 619)
case ._1: preconditionFailure("wrong String sum branch")
}

let boxedReader = pkg.api.capturedReader(Product(_0: Int32(723), _1: false))
let nativeReader = boxedReader.value(as: HostBox<Exp_api__unpackReader_ret<Host>>.self).payload
let reader = pkg.api.unpackReader(boxedReader)
precondition(host.typeStages == 0 && host.valueStages == 0)
let first = nativeReader.specialize(asKioType: Int32.self)
precondition(host.typeStages == 1 && host.valueStages == 0)
let firstResult = first(1)
precondition(firstResult._0 == 723 && !firstResult._1)
precondition(host.typeStages == 1 && host.valueStages == 1)
let second = reader.specialize(asKioType: Bool.self)
precondition(host.typeStages == 2 && host.valueStages == 1)
let secondResult = second(true)
precondition(secondResult._0 == 723 && !secondResult._1)
precondition(host.typeStages == 2 && host.valueStages == 2)
precondition(host.packCalls == 4 && host.unboxCalls == 3)
print("opaque structural substitution ok")
