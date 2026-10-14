import KioRuntime
import CallbackA
import CallbackB

typealias Fn<A, R> = KioRuntime.KioFn<A, R>
typealias Unit = KioRuntime.KioUnit

final class State {
    var events: [Int] = []
    var packs = [0, 0]
    var unpacks = [0, 0]
    var nativeStages = 0
    var nativeValues = 0
}

struct HostBox<T> {
    let value: T
}

final class FirstHost: CallbackAHost {
    let state: State
    init(_ state: State) { self.state = state }
    func api__mark() { state.events.append(0) }
    func api__pack<T>(_ value: T) -> KioHostType_api__Box<T> {
        state.packs[0] += 1
        return .init(HostBox(value: value))
    }
    func api__unpack<T>(_ value: KioHostType_api__Box<T>) -> T {
        state.unpacks[0] += 1
        return value.value(as: HostBox<T>.self).value
    }
}

final class SecondHost: CallbackBHost {
    let state: State
    init(_ state: State) { self.state = state }
    func peer__mark() { state.events.append(1) }
    func peer__pack<T>(_ value: T) -> KioHostType_peer__Box<T> {
        state.packs[1] += 1
        return .init(HostBox(value: value))
    }
    func peer__unpack<T>(_ value: KioHostType_peer__Box<T>) -> T {
        state.unpacks[1] += 1
        return value.value(as: HostBox<T>.self).value
    }
}

struct NativeIdentity: Exp_api__typeLink_arg0Implementation {
    typealias H = FirstHost
    let state: State
    func specialize<T>(_ type: T.Type) -> Fn<T, T> {
        state.nativeStages += 1
        return Fn { value in state.nativeValues += 1; return value }
    }
}

let state = State()
let first = createCallbackA(host: FirstHost(state))
let second = createCallbackB(host: SecondHost(state))
let depth = 1024

let firstSecret: KioNewtype_api__Secret<FirstHost, String> = first.api.secret("first")
let secondSecret: KioNewtype_peer__Secret<SecondHost, String> = second.peer.secret("second")
_ = (firstSecret, secondSecret)

// The only shared callable/control implementation is linked once. The two
// artifact modules retain distinct hosts and opaque nominal Box constructors.
let root: Fn<Unit, String> = first.api.root("captured")
var value = root
var retained = [value]
for index in 0..<depth {
    value = index % 2 == 0 ? first.api.link(value) : second.peer.link(value)
    value = index % 2 == 0 ? second.peer.keep(value) : first.api.keep(value)
    retained.append(value)
}
precondition(state.events.isEmpty)
precondition(state.packs == [depth / 2, depth / 2] && state.unpacks == state.packs)
let expected = (0..<depth).reversed().map { $0 % 2 } + [0]
for _ in 0..<2 {
    precondition(value(Unit()) == "captured")
    precondition(state.events == expected)
    state.events.removeAll()
}

// Each separately compiled module resumes its own work after its child returns.
var pending = root
var retainedPending = [pending]
for index in 0..<depth {
    pending = index % 2 == 0 ? first.api.pendingLink(pending) : second.peer.pendingLink(pending)
    pending = index % 2 == 0 ? second.peer.keep(pending) : first.api.keep(pending)
    retainedPending.append(pending)
}
precondition(state.events.isEmpty)
precondition(state.packs == [depth, depth] && state.unpacks == state.packs)
let pendingExpected = expected + (0..<depth).map { $0 % 2 }
for _ in 0..<2 {
    precondition(pending(Unit()) == "captured")
    precondition(state.events == pendingExpected)
    state.events.removeAll()
}

// Native input is explicitly lifted. Nested callback arguments and results
// have the same shared Swift type across separately compiled artifact modules.
var nativeCalls = 0
let nativeRoot = Fn<Unit, String> { _ in nativeCalls += 1; return "native" }
let nativeLinked = second.peer.link(first.api.keep(nativeRoot))
precondition(nativeLinked(Unit()) == "native" && nativeCalls == 1)
precondition(state.events == [1])
state.events.removeAll()
let nativeHigher = Fn<Fn<Unit, String>, Fn<Unit, String>> { callback in callback }
let higher = second.peer.higher(first.api.higher(nativeHigher))
let aggregate = second.peer.keep(first.api.pair(higher))
let selected = aggregate._0(nativeLinked)
precondition(nativeCalls == 1 && state.events == [1, 0])
state.events.removeAll()
precondition(selected(Unit()) == "native" && nativeCalls == 2)
precondition(state.events == [1])
state.events.removeAll()
let other = aggregate._1(root)
precondition(state.events == [1, 0])
state.events.removeAll()
precondition(other(Unit()) == "captured" && state.events == [0])
state.events.removeAll()

// Method-reference extraction is an ordinary native closure bridge, not a
// promise of tail transfer through arbitrary host code.
let nativeView: (Unit) -> String = selected.callAsFunction
precondition([Unit(), Unit()].map(nativeView) == ["native", "native"])
precondition(nativeCalls == 4 && state.events == [1, 1])
state.events.removeAll()

// Forall carriers keep their package's nominal identity. Selection executes
// the stage once; its typed callable result can then cross artifact modules.
let nativePoly = Exp_api__typeLink_arg0<FirstHost>(NativeIdentity(state: state))
var poly = nativePoly
var retainedPoly = [poly]
for _ in 0..<depth {
    poly = first.api.keep(first.api.typeLink(poly))
    retainedPoly.append(poly)
}
precondition(state.nativeStages == 0 && state.nativeValues == 0)
precondition(state.events.isEmpty)
let integer = poly.specialize(asKioType: Int.self)
precondition(state.nativeStages == 1 && state.nativeValues == 0)
precondition(state.events == Array(repeating: 0, count: depth))
state.events.removeAll()
let text = poly.specialize(asKioType: String.self)
precondition(state.nativeStages == 2 && state.nativeValues == 0)
precondition(state.events == Array(repeating: 0, count: depth))
state.events.removeAll()
let integerPair = second.peer.keep(first.api.pair(integer))
precondition(integerPair._0(37) == 37 && integerPair._1(41) == 41)
precondition(second.peer.keep(text)("selected") == "selected")
precondition(state.nativeStages == 2 && state.nativeValues == 3)
precondition(state.events.isEmpty)

let guestPoly = second.peer.keep(second.peer.typeLink(second.peer.typeRoot()))
precondition(state.events.isEmpty)
let guestIdentity = guestPoly.specialize(asKioType: String.self)
precondition(state.events == [1, 1])
state.events.removeAll()
precondition(first.api.keep(guestIdentity)("guest") == "guest")
precondition(state.events == [1])
state.events.removeAll()

// Retained-prefix release keeps ARC teardown separate from invocation depth.
value = root
while !retained.isEmpty { _ = retained.popLast() }
pending = root
while !retainedPending.isEmpty { _ = retainedPending.popLast() }
poly = nativePoly
while !retainedPoly.isEmpty { _ = retainedPoly.popLast() }
print("independent Swift callbacks ok")
