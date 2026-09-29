import App

struct LiveOnlyHost: AppHost {
    typealias api__Live = String

    func api__open() -> String {
        "live"
    }
}

struct LegacyText: ExpressibleByStringLiteral, Equatable {
    typealias StringLiteralType = String
    let value: String

    init(stringLiteral value: String) {
        self.value = value
    }
}

struct UnchangedOldHost: AppHost {
    typealias api__Live = String

    func api__open() -> String {
        "legacy"
    }

    func api__archived(_ arg0: LegacyText) -> LegacyText {
        arg0
    }
}

func assertCurrentDefault<H: AppHost>(_: H.Type)
where H.api__Retired == String {}

func assertLegacyWitnessInference<H: AppHost>(_: H.Type)
where H.api__Retired == LegacyText {}

assertCurrentDefault(LiveOnlyHost.self)
assertLegacyWitnessInference(UnchangedOldHost.self)

let current = createApp(host: LiveOnlyHost())
precondition(current.api.echo("current") == "current")

let legacy = createApp(host: UnchangedOldHost())
precondition(legacy.api.echo("old host") == "old host")

print("deprecated Swift host history")
