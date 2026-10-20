import { createBench } from "../workload/out/ts/bench.js";
import { checkSurface } from "./surface.js";

export function checkStagedSurface(): number {
  return checkSurface((body) => createBench.lift(body));
}
