use wasm_bindgen::prelude::*;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function exerciseTrailingBlocks(repl) {
  const check = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const clean = value => value.replace(/\x1b\[[0-9;]*m/g, '').trim();
  const turn = input => {
    const result = repl.eval(input);
    check(Object.keys(result).sort().join(',') === 'is_error,keep_running,output', 'turn keys');
    check(typeof result.output === 'string', 'output string');
    check(result.keep_running === true && result.is_error === false, 'turn status');
    return clean(result.output);
  };
  try {
    check(turn(':load app').includes('loaded app'), 'fixture load');
    for (const [input, expected] of [
      [':normalize packet! { () }', '()'],
      [':normalize enter! { let value = (); value }', '()'],
      [':normalize sequence! box_bind { let .(value: .) <- box_pure(()); box_pure(value) }', 'Box.box(())'],
    ]) {
      const actual = turn(input);
      check(actual === expected, `${input}: ${actual}`);
    }
    for (const [input, present, absent, prefix] of [
      ['enter! { let local = (); loc', 'local', 'other', 'loc'],
      ['enter! { let other = (); ', 'other', 'local', ''],
    ]) {
      const result = repl.complete(input, input.length);
      check(Object.keys(result).sort().join(',') === 'candidates,replaceEnd,replaceStart', 'completion keys');
      check(result.replaceStart === input.length - prefix.length, 'replacement start');
      check(result.replaceEnd === input.length, 'replacement end');
      check(Array.isArray(result.candidates), 'candidate array');
      const selected = result.candidates.find(item => item.label === present);
      check(selected !== undefined && selected.kind === 'let', `${present}: binding candidate`);
      check(typeof selected.detail === 'string', `${present}: binding detail`);
      check(!result.candidates.some(item => item.label === absent), `${absent}: stale binding`);
    }
    const continuation = 'two! { () } fall';
    const labels = repl.complete(continuation, continuation.length);
    check(labels.replaceStart === continuation.length - 4 && labels.replaceEnd === continuation.length, 'label replacement');
    check(labels.candidates.length === 1 && labels.candidates[0].label === 'fallback' && labels.candidates[0].kind === 'keyword', 'selected label');
    turn(':reset');
    check(!repl.complete('packet', 6).candidates.some(item => item.label === 'packet!'), 'reset imports');
    check(repl.complete(continuation, continuation.length).candidates.length === 0, 'reset label provider');
    return 6;
  } finally {
    repl.free();
  }
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = exerciseTrailingBlocks)]
    fn exercise_trailing_blocks(repl: JsValue) -> Result<u32, JsValue>;
}

#[wasm_bindgen_test]
pub fn trailing_blocks_cross_public_javascript_boundaries() {
    let repl = super::trailing_blocks_repl();
    assert_eq!(exercise_trailing_blocks(repl.into()).unwrap(), 6);
}
