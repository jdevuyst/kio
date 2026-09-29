import FfiSwiftExactHostBindings

private func requireExactHostContract<H: FfiSwiftExactHostBindingsHost>(
    _: H.Type,
    flag: H.main__Flag,
    unused: H.unused__Unused,
    token: H.main__Token
) {
    let left: H.left__Shared = 0
    let right: H.right__Shared = 0
    let text: H.main__Text = ""
    let signed: H.main__WideSigned = 0
    let unsigned: H.main__WideUnsigned = 0
    let float: H.main__FloatBox = 0.0
    let falseFlag: H.main__Flag = false
    let falseUnused: H.unused__Unused = false

    precondition(flag == falseFlag)
    precondition(unused == falseUnused)
    _ = (left, right, text, signed, unsigned, float, token)
}

struct LeftShared: ExpressibleByIntegerLiteral, Equatable {
    typealias IntegerLiteralType = Int64
    let raw: Int64

    init(integerLiteral value: Int64) {
        raw = value
    }
}

struct RightShared: ExpressibleByIntegerLiteral, Equatable {
    typealias IntegerLiteralType = Int128
    let raw: Int128

    init(integerLiteral value: Int128) {
        raw = value
    }
}

struct TextValue: ExpressibleByStringLiteral, Equatable {
    typealias StringLiteralType = StaticString
    let raw: String

    init(stringLiteral value: StaticString) {
        raw = value.description
    }

    init(raw: String) {
        self.raw = raw
    }
}

struct Token: Equatable {
    let raw: UInt8
}

struct Flag: ExpressibleByBooleanLiteral, Equatable {
    typealias BooleanLiteralType = Bool
    let raw: Bool

    init(booleanLiteral value: Bool) {
        raw = value
    }
}

struct WideSigned: ExpressibleByIntegerLiteral, Equatable {
    typealias IntegerLiteralType = Int128
    let raw: Int128

    init(integerLiteral value: Int128) {
        raw = value
    }
}

struct WideUnsigned: ExpressibleByIntegerLiteral, Equatable {
    typealias IntegerLiteralType = UInt128
    let raw: UInt128

    init(integerLiteral value: UInt128) {
        raw = value
    }
}

struct FloatBox: ExpressibleByFloatLiteral {
    typealias FloatLiteralType = Double
    let raw: Double

    init(floatLiteral value: Double) {
        raw = value
    }
}

struct ExactHost: FfiSwiftExactHostBindingsHost {
    typealias left__Shared = LeftShared
    typealias right__Shared = RightShared
    typealias main__Text = TextValue
    typealias main__Token = Token
    typealias main__Flag = Flag
    typealias main__WideSigned = WideSigned
    typealias main__WideUnsigned = WideUnsigned
    typealias main__FloatBox = FloatBox
    typealias unused__Unused = Bool

    func main__echoText(_ arg0: TextValue) -> TextValue {
        arg0
    }

    func main__echoToken(_ arg0: Token) -> Token {
        arg0
    }

    func main__echoFlag(_ arg0: Flag) -> Flag {
        arg0
    }

    func main__makeChoice(_ arg0: TextValue) -> Env_main__makeChoice_ret<ExactHost> {
        ._0(arg0)
    }
}

let package = createFfiSwiftExactHostBindings(host: ExactHost())
requireExactHostContract(ExactHost.self, flag: false, unused: false, token: Token(raw: 0))

precondition(package.left.keep(41).raw == 41)
precondition(package.left.literal().raw == 41)
precondition(package.left.apply({ value in LeftShared(integerLiteral: value.raw + 1) }, 41).raw == 42)
precondition(package.right.keep(42).raw == 42)
precondition(package.right.literal().raw == 42)
let duplicated = package.right.duplicate(43)
precondition(duplicated._0.raw == 43)
precondition(duplicated._1.raw == 43)

precondition(package.main.roundtripText("borrowed").raw == "borrowed")
precondition(package.main.roundtripTextLiteral().raw == "direct literal")
precondition(package.main.keepToken(Token(raw: 7)) == Token(raw: 7))
precondition(package.main.applyText(
    { value in TextValue(raw: value.raw + " callback") },
    "direct"
).raw == "direct callback")

let pair = package.main.pair("pair", Token(raw: 8))
precondition(pair._0.raw == "pair")
precondition(pair._1 == Token(raw: 8))
let bundled = package.main.KioType_Bundle.bundle(pair)
let unbundled = package.main.KioType_Bundle.unbundle(bundled)
precondition(unbundled._0.raw == "pair")
precondition(unbundled._1 == Token(raw: 8))
_ = package.main.KioType_AB.c(pair)
_ = package.main.KioType_A.bC(pair)

switch package.main.textOrToken("sum") {
case ._0(let value):
    precondition(value.raw == "sum")
case ._1:
    preconditionFailure("unexpected token arm")
}

precondition(package.main.wideSignedMin().raw == Int128.min)
precondition(package.main.wideUnsignedMax().raw == UInt128.max)
precondition(package.main.floatFromInteger().raw == 16_777_217)
precondition(package.main.floatLiteral().raw == 1.25)
precondition(package.main.flagLiteral().raw)

precondition(package.main.choose(true, Token(raw: 1), Token(raw: 2)) == Token(raw: 1))
precondition(package.main.choose(false, Token(raw: 1), Token(raw: 2)) == Token(raw: 2))
precondition(package.main.chooseCall(true, Token(raw: 3), Token(raw: 4)) == Token(raw: 3))
precondition(package.main.chooseHostCall(false, Token(raw: 5), Token(raw: 6)) == Token(raw: 6))
precondition(package.main.chooseLet(false, Token(raw: 7), Token(raw: 8)) == Token(raw: 8))
precondition(package.main.chooseLiteral(Token(raw: 9), Token(raw: 10)) == Token(raw: 9))
precondition(package.main.chooseClosure(false, Token(raw: 11), Token(raw: 12)) == Token(raw: 12))
precondition(package.main.chooseInferredClosure(true, Token(raw: 15), Token(raw: 16)) == Token(raw: 15))
let destructured = package.main.flagPair(true, Token(raw: 13))
precondition(package.main.chooseDestructured(destructured, Token(raw: 14)) == Token(raw: 13))
