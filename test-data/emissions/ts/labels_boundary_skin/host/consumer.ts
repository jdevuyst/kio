// An independently authored TypeScript host reading the public surface at
// its runtime shape. These types deliberately do not derive from the
// generated declarations: assigning the manual and generated functions in
// both directions checks the input and return relationships independently.
import { createBuildTsLabelsBoundarySkin } from "../workdir/out/ts/build_ts_labels_boundary_skin.js";

const pkg = createBuildTsLabelsBoundarySkin();

type HostPair = { Name: string; Counter: number };
type HostChoice =
  | { Accepted: string }
  | { Retry: number }
  | { Rejected: string };

const generatedEchoPair = pkg.api.KioModule_main.echoPair;
const generatedPairAsHost: (value: HostPair) => HostPair = generatedEchoPair;
const hostPairAsGenerated: typeof generatedEchoPair =
  (value: HostPair): HostPair => value;

const constructedPair: HostPair = { Name: "host", Counter: 7 };
const echoedPair: HostPair = generatedPairAsHost(constructedPair);
const madePair: HostPair = pkg.api.KioModule_main.make("kio", 3);
const name: string = echoedPair.Name;
const counter: number = madePair.Counter;

const generatedEchoChoice = pkg.api.KioModule_main.echoChoice;
const generatedChoiceAsHost: (value: HostChoice) => HostChoice = generatedEchoChoice;
const hostChoiceAsGenerated: typeof generatedEchoChoice =
  (value: HostChoice): HostChoice => value;

const accepted: HostChoice = { Accepted: "ready" };
const retry: HostChoice = { Retry: 17 };
const rejected: HostChoice = { Rejected: "invalid" };

function readChoice(value: HostChoice): string {
  if ("Accepted" in value) {
    return value.Accepted;
  }
  if ("Retry" in value) {
    return String(value.Retry);
  }
  if ("Rejected" in value) {
    return value.Rejected;
  }
  const exhausted: never = value;
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
