import Foundation
import Dispatch
import BenchA
import BenchB

func require(_ condition: Bool, _ message: String) {
    precondition(condition, message)
}

func integer(_ text: String, _ lower: Int, _ upper: Int) -> Int {
    guard let value = Int(text), lower <= value && value <= upper else {
        preconditionFailure("out-of-range integer")
    }
    return value
}

func now() -> UInt64 { DispatchTime.now().uptimeNanoseconds }
func ownerForLabel(_ label: Int) -> Int { label == 0 ? 0 : (label - 1) % 2 }

struct Config {
    let arm: String
    let mode: String
    let depth: Int
    let repeatCount: Int
    let seed: Int
    let warmup: Int
    let purpose: String

    init(_ args: [String]) {
        require(args.count == 8, "usage: driver ARM MODE DEPTH REPEAT SEED WARMUP PURPOSE")
        arm = args[1]
        mode = args[2]
        depth = integer(args[3], 0, 10000)
        repeatCount = integer(args[4], 1, 1000000)
        seed = integer(args[5], 0, 65535)
        warmup = integer(args[6], 0, 1000000)
        purpose = args[7]
        require(acceptsArm(arm), "arm differs from compiled public callback adapter")
        require(["direct", "native", "host-loop", "callback-tail", "callback-pending", "callback-extract"].contains(mode), "unknown mode")
        require(purpose == "time" || purpose == "witness", "unknown purpose")
        require(!["direct", "native"].contains(mode) || depth == 0, "direct/native require depth zero")
        require(purpose != "witness" || (depth <= 8 && repeatCount == 1 && warmup == 0), "invalid witness bounds")
    }
}

final class Events {
    let witness: Bool
    var labels: [Int] = []
    var owners: [[Int]] = []

    init(_ witness: Bool) { self.witness = witness }

    func step(_ owner: Int, _ value: Int32, _ label: Int32) -> Int32 {
        if witness {
            labels.append(Int(label))
            owners.append([owner, Int(label)])
        }
        return value + label + 1
    }
}

final class FirstHost: BenchAHost {
    typealias bench__I32 = Int32
    let events: Events
    init(_ events: Events) { self.events = events }
    func bench__step(_ value: Int32, _ label: Int32) -> Int32 { events.step(0, value, label) }
}

final class SecondHost: BenchBHost {
    typealias bench__I32 = Int32
    let events: Events
    init(_ events: Events) { self.events = events }
    func bench__step(_ value: Int32, _ label: Int32) -> Int32 { events.step(1, value, label) }
}

final class Prepared {
    var first: BenchA<FirstHost>?
    var second: BenchB<SecondHost>?
    var roots: [ScalarCallback] = []
    var invoke: ((Int32) -> Int32)?

    init(_ events: Events) {
        first = createBenchA(host: FirstHost(events))
        second = createBenchB(host: SecondHost(events))
    }

    func construct(_ config: Config, _ events: Events) {
        let first = self.first!
        let second = self.second!
        switch config.mode {
        case "callback-tail", "callback-pending", "callback-extract":
            var value: ScalarCallback = first.bench.callbackSeed()
            roots.append(value)
            for label in 1..<(config.depth + 1) {
                if config.mode == "callback-pending" {
                    value = ownerForLabel(label) == 0
                        ? first.bench.callbackPending(value, Int32(label))
                        : second.bench.callbackPending(value, Int32(label))
                } else {
                    value = ownerForLabel(label) == 0
                        ? first.bench.callbackTail(value, Int32(label))
                        : second.bench.callbackTail(value, Int32(label))
                }
                // Store the carrier unchanged before passing it to the next module.
                roots.append(value)
            }
            if config.mode == "callback-extract" {
                invoke = extract(value)
            } else {
                invoke = { [value] input in call(value, input) }
            }
        case "native":
            let native = lift { input in events.step(0, input, 0) }
            invoke = { [first, native] input in first.bench.callNative(native, input) }
        case "direct":
            invoke = first.bench.direct
        case "host-loop":
            invoke = { input in
                var value = input
                for label in stride(from: config.depth, through: 0, by: -1) {
                    value = events.step(ownerForLabel(label), value, Int32(label))
                }
                return value
            }
        default:
            preconditionFailure("validated mode was lost")
        }
    }

    func releaseRoots() -> Int {
        invoke = nil
        var removed = 0
        while !roots.isEmpty {
            _ = roots.popLast()
            removed += 1
        }
        first = nil
        second = nil
        return removed
    }
}

let config = Config(CommandLine.arguments)
let events = Events(config.purpose == "witness")
var start = now()
let prepared = Prepared(events)
let packageNs = now() - start
start = now()
prepared.construct(config, events)
let constructNs = now() - start
require(events.labels.isEmpty && events.owners.isEmpty, "construction executed a host body")
let delta = Int32((config.depth + 1) * (config.depth + 2) / 2)
var expected: Int64 = 0
for index in 0..<config.repeatCount {
    expected += Int64((config.seed + index * 17) % 65536) + Int64(delta)
}
start = now()
for index in 0..<config.warmup {
    let value = Int32((config.seed + index * 17) % 65536)
    require(prepared.invoke!(value) == value + delta, "warmup checksum mismatch")
}
let warmupNs = now() - start
start = now()
var checksum: Int64 = 0
for index in 0..<config.repeatCount {
    checksum += Int64(prepared.invoke!(Int32((config.seed + index * 17) % 65536)))
}
let invokeNs = now() - start
require(checksum == expected, "measured checksum mismatch")
if events.witness {
    let labels = config.mode == "callback-pending"
        ? Array(0...config.depth) : Array((0...config.depth).reversed())
    require(events.labels == labels, "effect label order mismatch")
    require(events.owners == labels.map { [ownerForLabel($0), $0] }, "effect package owner mismatch")
}
withExtendedLifetime(prepared) {}
let rootsBefore = prepared.roots.count
start = now()
let releasedSlots = prepared.releaseRoots()
let releaseNs = now() - start
let row: [String: Any] = [
    "schema": "kio-swift-targeted-driver-v1", "backend": "swift", "arm": config.arm,
    "topology": "alternating-independent-modules", "mode": config.mode,
    "depth": config.depth, "repeat": config.repeatCount, "seed": config.seed,
    "warmup": config.warmup, "purpose": config.purpose,
    "checksum": checksum, "expected": expected, "trace": events.labels,
    "owner_trace": events.owners, "package_ns": packageNs, "construct_ns": constructNs,
    "warmup_ns": warmupNs, "invoke_ns": invokeNs, "release_roots_ns": releaseNs,
    "total_phases_ns": packageNs + constructNs + warmupNs + invokeNs + releaseNs,
    "root_slots_before_release": rootsBefore, "released_root_slots": releasedSlots,
    "root_slots_after_release": prepared.roots.count,
    "capture_destruction": NSNull(),
    "capture_destruction_reason": "not instrumented; cleared host roots do not prove all guest captures were destroyed"
]
let bytes = try JSONSerialization.data(withJSONObject: row, options: [.sortedKeys])
FileHandle.standardOutput.write(bytes)
FileHandle.standardOutput.write(Data([10]))
