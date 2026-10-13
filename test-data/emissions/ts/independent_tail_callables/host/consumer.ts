import { createTailOrigin, type TailOriginCallable } from "../workdir/out/ts/tail_origin.js";
import { createTailPeer } from "../workdir/peer/out/ts/tail_peer.js";

const depth = 2048;
const events: [number, number][] = [];
const stored = [0, 0];
const host = (owner: number) => ({api: {
  mark: (code: number) => { events.push([owner, code]); return null; },
  keep: <T>(value: T): T => { stored[owner]++; return {value}.value; },
}});
const instances = [createTailOrigin(host(0)).api, createTailPeer(host(1)).api];
function check(condition: boolean, message: string) {
  if (!condition) throw new Error(message);
}
function trace(link: number, root: number) {
  const expected = Array.from({length: depth}, (_, offset) =>
    [(depth - 1 - offset) % 2, link]);
  expected.push([0, root]);
  check(JSON.stringify(events) === JSON.stringify(expected), "owner or stage order");
  events.length = 0;
}
const values = [instances[0].valueRoot()];
for (let i = 0; i < depth; i++) {
  const linked = instances[i % 2].valueLink(values[values.length - 1]);
  values.push(linked, instances[1 - i % 2].keepValue(linked));
}
check(events.length === 0 && stored.every(n => n === depth / 2), "value transport effects");
for (let i = 0; i < 2; i++) {
  const result = values[values.length - 1](null);
  trace(11, 12);
  check(result._0 === 37 && result._1(null) === 41, "nested value result");
  check(JSON.stringify(events) === "[[0,13]]", "nested value callback owner");
  events.length = 0;
}
const stages = [instances[0].typeRoot()];
for (let i = 0; i < depth; i++) {
  const linked = instances[i % 2].typeLink(stages[stages.length - 1]);
  stages.push(linked, instances[1 - i % 2].keepType(linked));
}
check(events.length === 0 && stored.every(n => n === depth), "type transport effects");
const number = stages[stages.length - 1].advance<number>();
trace(21, 22);
const text = stages[stages.length - 1].advance<string>();
trace(21, 22);
function selected<A>(callback: TailOriginCallable<A, {_0: A; _1: TailOriginCallable<null, A>}>, input: A): void {
  const result = callback(input);
  check(result._0 === input && JSON.stringify(events) === "[[0,23]]", "selected result");
  events.length = 0;
  check(result._1(null) === input && JSON.stringify(events) === "[[0,24]]", "nested selected result");
  events.length = 0;
}
selected(number, 91);
selected(number, 93);
selected(text, "nested");
// The selected ordinary signatures remain exact across the foreign artifact.
if (false) {
  // @ts-expect-error a selected number callback does not accept text
  number("wrong");
  // @ts-expect-error raw functions are not cooperative callable carriers
  instances[0].keepValue(() => ({_0: 37, _1: () => 41}));
}
check(stored.every(n => n === depth), "invocation-time storage");
console.log("independent tail callables ok");
