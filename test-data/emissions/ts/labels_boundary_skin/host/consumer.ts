// An independently authored TypeScript host reading the public surface at
// its runtime shape. These types deliberately do not derive from the
// generated declarations: assigning the manual and generated functions in
// both directions checks the input and return relationships independently.
import { createBuildTsLabelsBoundarySkin } from "../workdir/out/ts/build_ts_labels_boundary_skin.js";

const pkg = createBuildTsLabelsBoundarySkin();

type HostPair = { _0: { Name: string }; _1: { Counter: number } };
type HostChoice =
  | { _0: { Accepted: string } }
  | { _1: { _0: { Retry: number } } | { _1: { Rejected: string } } };

const generatedEchoPair = pkg.api.KioModule_main.echoPair;
const generatedPairAsHost: (value: HostPair) => HostPair = generatedEchoPair;
const hostPairAsGenerated: typeof generatedEchoPair =
  (value: HostPair): HostPair => value;

const constructedPair: HostPair = { _0: { Name: "host" }, _1: { Counter: 7 } };
const echoedPair: HostPair = generatedPairAsHost(constructedPair);
const madePair: HostPair = pkg.api.KioModule_main.make("kio", 3);
const name: string = echoedPair._0.Name;
const counter: number = madePair._1.Counter;

const generatedEchoChoice = pkg.api.KioModule_main.echoChoice;
const generatedChoiceAsHost: (value: HostChoice) => HostChoice = generatedEchoChoice;
const hostChoiceAsGenerated: typeof generatedEchoChoice =
  (value: HostChoice): HostChoice => value;

const accepted: HostChoice = { _0: { Accepted: "ready" } };
const retry: HostChoice = { _1: { _0: { Retry: 17 } } };
const rejected: HostChoice = { _1: { _1: { Rejected: "invalid" } } };

function readChoice(value: HostChoice): string {
  if ("_0" in value) {
    return value._0.Accepted;
  }
  const tail = value._1;
  if ("_0" in tail) {
    return String(tail._0.Retry);
  }
  if ("_1" in tail) {
    return tail._1.Rejected;
  }
  const exhausted: never = tail;
  return exhausted;
}

const acceptedResult: string = readChoice(generatedChoiceAsHost(accepted));
const retryResult: string = readChoice(generatedChoiceAsHost(retry));
const rejectedResult: string = readChoice(generatedChoiceAsHost(rejected));

void hostPairAsGenerated;
void hostChoiceAsGenerated;
void name;
void counter;
void acceptedResult;
void retryResult;
void rejectedResult;
