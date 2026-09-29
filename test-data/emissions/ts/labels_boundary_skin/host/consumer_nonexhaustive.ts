import { createBuildTsLabelsBoundarySkin } from "../workdir/out/ts/build_ts_labels_boundary_skin.js";

const pkg = createBuildTsLabelsBoundarySkin();
type GeneratedChoice = ReturnType<typeof pkg.api.KioModule_main.echoChoice>;

function readChoiceNonexhaustively(value: GeneratedChoice): string {
  if ("Accepted" in value) {
    return value.Accepted;
  }
  if ("Retry" in value) {
    return String(value.Retry);
  }
  const exhausted: never = value;
  return exhausted;
}

void readChoiceNonexhaustively;
