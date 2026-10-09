import { createOpaqueSubstitution } from "../workdir/out/js/opaque_substitution.js";

const pair = { _0: 7, _1: true };
const boxed = { value: pair };
let calls = 0;
const pkg = createOpaqueSubstitution({
  api: {
    unbox: received => {
      if (received !== boxed) throw new Error("opaque container identity changed");
      calls++;
      return received.value;
    },
  },
});

const failures = [];
function checkPair(label, value) {
  if (value?._0 !== 7) failures.push(`${label}: number payload changed`);
  if (value?._1 !== true) failures.push(`${label}: flag payload changed`);
}
checkPair("direct extraction", pkg.api.direct(boxed));
checkPair("generic relay", pkg.api.throughRelay(boxed));
if (calls !== 2) failures.push("both host extractions must execute");
if (failures.length !== 0) throw new Error(failures.join("\n"));
console.log("opaque product payloads preserved");
