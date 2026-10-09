import { createBuildTsLabelsBoundarySkin } from "../workdir/out/ts/build_ts_labels_boundary_skin.js";

const pkg = createBuildTsLabelsBoundarySkin();
type GeneratedChoice = ReturnType<typeof pkg.api.KioModule_main.echoChoice>;

function readChoiceNonexhaustively(value: GeneratedChoice): string {
  if ("_0" in value) {
    return value._0.Accepted;
  }
  const tail = value._1;
  if ("_0" in tail) {
    return String(tail._0.Retry);
  }
  const exhausted: never = tail;
  return exhausted;
}

void readChoiceNonexhaustively;
