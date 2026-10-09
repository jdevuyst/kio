import {
  createCanonicalValues,
  liftCanonicalValuesCallable as lift,
} from "../workdir/out/js/canonical_values.js";
import { createForeignValues } from "../workdir/foreign/out/js/foreign_values.js";

const trace = [];
const host = {
  api: {
    pack: value => ({ value }),
    hostHold: value => lift(unit => {
      if (unit !== null) throw new Error("first-class Unit domain omitted");
      return value;
    }),
    mark: code => { trace.push(code); return null; },
    argument: () => { trace.push(3); return 37; },
  },
};
const pkg = createCanonicalValues(host);
const second = createCanonicalValues(host);
const foreign = createForeignValues({});
function check(condition, message) {
  if (!condition) throw new Error(message);
}
function checkPairThunk(make, label) {
  const input = { _0: 7, _1: true };
  const thunk = make(input);
  input._0 = 99;
  const first = thunk(null);
  check(first._0 === 7 && first._1 === true, `${label}: retained input snapshot`);
  first._0 = 88;
  const next = thunk(null);
  check(next !== first && next._0 === 7 && next._1 === true,
    `${label}: fresh result from escaped closure`);
  return thunk;
}
const direct = checkPairThunk(pkg.api.pairHold, "Kio generic capture");
checkPairThunk(value => pkg.api.boxedHold(value).value, "opaque Box escape");
checkPairThunk(pkg.api.pairHostHold, "host generic retaining factory");
checkPairThunk(value => foreign.api.keep(pkg.api.pairHold(value)), "independent artifact adapter");
const atom = { count: 1 };
const atomThunk = pkg.api.atomHold(atom);
atom.count = 2;
check(atomThunk(null) === atom && atomThunk(null).count === 2, "opaque Atom identity");
const generic = pkg.api.hold(atom);
check(generic(null) === atom, "erased T retains its reference");

trace.length = 0;
check(pkg.api.invoke() === 37, "computed staged result");
check(trace.join() === "0,1,3,2", "computed type stage precedes operand and leaf");
trace.length = 0;
let poly = pkg.api.computed();
check(trace.join() === "0", "transport cannot enter a type stage");
poly = pkg.api.relay(poly);
for (let i = 0; i < 20000; i++) poly = poly.adapt(value => value);
check(trace.join() === "0", "adaptation cannot enter a type stage");
const firstStage = poly.advance();
const secondStage = poly.advance();
check(trace.join() === "0,1,1", "each advance executes exactly once");
check(firstStage(17) === 17 && secondStage(19) === 19, "staged leaves");
check(trace.join() === "0,1,1,2,2", "one leaf call per invocation");

let inputs = 0, outputs = 0;
let deep = direct;
for (let i = 0; i < 20000; i++) {
  deep = deep.adapt(value => { inputs++; return value; }, value => { outputs++; return value; });
}
check(deep(null)._0 === 7 && inputs === 20000 && outputs === 20000, "iterative production adapters");
let returning = lift(() => direct);
for (let i = 0; i < 20000; i++) {
  returning = returning.adapt(value => value, value => value.adapt(x => x, x => x));
}
check(returning(null)(null)._0 === 7, "returned callable adaptation stays iterative");

let crossedCalls = 0;
const retainedPair = { _0: 23, _1: true };
let crossed = lift(unit => {
  check(unit === null, "cross-package Unit domain");
  crossedCalls++;
  return retainedPair;
});
for (let i = 0; i < 20000; i++) {
  crossed = pkg.api.keepThunk(foreign.api.keep(crossed));
}
const crossedFirst = crossed(null);
crossedFirst._0 = 91;
const crossedSecond = crossed(null);
check(crossedCalls === 2 && crossedSecond._0 === 23 && crossedSecond._1 === true &&
  crossedFirst !== crossedSecond && retainedPair._0 === 23,
  "alternating concrete exports retain iterative copying and one leaf call");

trace.length = 0;
const observer = pkg.api.boxedObserver().value;
const local = pkg.api.makeSeal(5);
check(observer(local) === local && trace.join() === "7", "local nominal callable result");
trace.length = 0;
let rejected = false;
try { observer(second.api.makeSeal(5)); } catch (error) {
  if (!(error instanceof TypeError)) throw error;
  rejected = true;
}
check(rejected && trace.length === 0, "foreign nominal rejected before escaped leaf effects");

const boom = {};
let pendingAfter = 0;
const throwing = lift(() => { throw boom; }).adapt(x => x, x => { pendingAfter++; return x; });
let caught;
try { throwing(null); } catch (error) { caught = error; }
check(caught === boom && pendingAfter === 0, "exception identity and abandoned after-work");
let nested = 0;
const reentrant = direct.adapt(unit => {
  check(foreign.api.keep(direct)(null)._0 === 7, "reentrant foreign protection");
  nested++;
  return unit;
}, value => value);
check(reentrant(null)._0 === 7 && nested === 1, "invocation-local adapter state");
console.log("canonical callable values preserved");
