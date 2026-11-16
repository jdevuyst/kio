use wasm_bindgen::prelude::*;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function exerciseLiteralInference(repl) {
  const check = (ok, message) => { if (!ok) throw new Error(message); };
  const turn = input => repl.eval(input).output.replace(/\x1b\[[0-9;]*m/g, '').trim();
  try {
    check(turn(':load app').includes('loaded app'), 'load literal types');
    const ordinary = repl.complete('def', 3).candidates.find(item => item.label === 'default');
    check(ordinary !== undefined && ordinary.kind === 'fn', 'ordinary default completion');
    const queries = ['42[Int]', '42.0e0[Float]', '42.0E+3[Float]', '42.0[Float]', '42.0e-3[Float]', '.t[Bool]', 'default()',
      'same(42, default())', 'same(42.0, floating())', 'choose(.t)'];
    for (const query of queries) {
      const output = turn(':type ' + query);
      check(output.startsWith(query + ' : '), `${query}: ${output}`);
    }
    for (const query of ['42', '42.0', '.t', 'same(42.0, 42)', 'same(42, 42.0)', '42[Float]']) {
      const output = turn(':type ' + query);
      check(output.includes('the expression does not type-check'), `${query}: ${output}`);
    }
    for (const literal of ['1e12', '42e0', '42E+3', '-4_2E-0']) {
      check(turn(':type ' + literal).includes('exponent notation is not supported for integer literals'), `integer exponent: ${literal}`);
    }
    check(turn(':normalize same(42, default())') === '42[Int]', 'integer literal category');
    check(turn(':normalize same(42.0, floating())').startsWith('42.0'), 'floating literal point');
    check(/^42(?:\[|$)/.test(turn(':normalize choose(.t)')), 'inferred Boolean true branch');
    check(/^0(?:\[|$)/.test(turn(':normalize choose(.f)')), 'inferred Boolean false branch');
    check(turn(':sig default').startsWith('pub fn default() -> Int'), 'ordinary default value name');
    turn(':reset');
    check(!repl.complete('def', 3).candidates.some(item => item.label === 'default'), 'reset ordinary default name');
    return queries.length;
  } finally { repl.free(); }
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = exerciseLiteralInference)]
    fn exercise_literal_inference(repl: JsValue) -> Result<u32, JsValue>;
}

#[wasm_bindgen_test]
pub fn literal_inference_crosses_the_public_javascript_boundary() {
    assert_eq!(
        exercise_literal_inference(super::literal_types_repl().into()).unwrap(),
        10
    );
}

#[wasm_bindgen(inline_js = r#"
export function exerciseReferenceOwnership(repl) {
  const check = (ok, label) => { if (!ok) throw new Error(label); };
  const turn = input => repl.eval(input).output.replace(/\x1b\[[0-9;]*m/g, '').trim();
  try {
    check(turn(':load demo/main').includes('loaded demo/main'), 'load');
    for (const [query, present, absent] of [
      ['truth', ['demo/a:', 'in fn literal_use'], ['demo/b:', 'shadow']],
      ['first.proof', ['demo/a:'], ['demo/b:']],
      ['build', ['in macro build', 'in fn bang', 'in fn splice'], ['demo/b:']],
      ['Field', ['demo/a:', 'in fn alias_use', 'in fn update', 'in fn access', 'in fn binding'], ['demo/b:', 'in fn qualified', 'in fn value_use']],
      ['second.Field', ['demo/b:', 'in fn qualified'], ['demo/a:', 'in fn alias_use']],
    ]) {
      const refs = turn(':refs ' + query);
      for (const text of present) check(refs.includes(text), `${query}: missing ${text}: ${refs}`);
      for (const text of absent) check(!refs.includes(text), `${query}: foreign ${text}: ${refs}`);
    }
    check(turn(':refs proof') === 'no references to `proof` in loaded modules', 'unresolved scope');
    check(turn(':which proof').includes('demo/b.proof'), 'discovery');
    turn(':reset');
    check(turn(':refs truth') === 'no references to `truth` in loaded modules', 'reset ownership');
    return 5;
  } finally { repl.free(); }
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = exerciseReferenceOwnership)]
    fn exercise_reference_ownership(repl: JsValue) -> Result<u32, JsValue>;
}

#[wasm_bindgen_test]
pub fn source_reference_ownership_crosses_public_javascript_boundary() {
    assert_eq!(
        exercise_reference_ownership(super::reference_ownership_repl().into()).unwrap(),
        5
    );
}

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
    const macroColor = '\x1b[38;2;86;182;194m';
    check(repl.highlight(':normalize packet { () }', 0).includes(macroColor + 'packet'), 'retained macro highlight');
    check(!repl.highlight(':normalize .(packet: . -> .) { packet(()) }', 0).includes(macroColor + 'packet'), 'ordinary parameter shadows macro highlight');
    for (const [input, expected] of [
      [':normalize packet { () }', '()'],
      [':normalize enter { let value = (); value }', '()'],
      [':normalize .() { let .x. = ((), ()); x2 }()', '()'],
      [':normalize sequence(box_bind) { let .x. <- box_pure(()); box_pure(x1) }', 'Box.mk(())'],
      [':normalize sequence(box_bind) { let .(value: .) <- box_pure(()); box_pure(value) }', 'Box.mk(())'],
      [':normalize box_bind.>sequence { box_pure(()) }', 'Box.mk(())'],
      [':normalize box_bind.>>sequence { box_pure(()) }', 'Box.mk(())'],
      [':normalize sequence { box_pure(()) }.<<box_bind', 'Box.mk(())'],
      [':normalize sequence { box_pure(()) }.<box_bind', 'Box.mk(())'],
    ]) {
      const actual = turn(input);
      check(actual === expected, `${input}: ${actual}`);
    }
    for (const [input, present, absent, prefix] of [
      ['enter { let local = (); loc', 'local', 'other', 'loc'],
      ['enter { let other = (); ', 'other', 'local', ''],
      ['.() { let .x. = (); x3; let .x. = (); x', 'x1', 'x3', 'x'],
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
    const continuation = 'two { () } fall';
    const labels = repl.complete(continuation, continuation.length);
    check(labels.replaceStart === continuation.length - 4 && labels.replaceEnd === continuation.length, 'label replacement');
    check(labels.candidates.length === 1 && labels.candidates[0].label === 'fallback' && labels.candidates[0].kind === 'keyword', 'selected label');
    for (const input of ['recur(', 'recur(cont, ', '.() { recur(', ':normalize recur(']) {
      check(repl.complete(input, input.length).candidates.length === 0, `${input}: recursive group required`);
    }
    turn(':reset');
    check(!repl.highlight(':normalize packet { () }', 0).includes(macroColor + 'packet'), 'reset macro highlight');
    check(!repl.complete('packet', 6).candidates.some(item => item.label === 'packet'), 'reset imports');
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

#[wasm_bindgen(inline_js = r#"
export function exerciseSelectors(repl) {
  const check = (ok, label) => { if (!ok) throw new Error(label); };
  const clean = value => value.replace(/\x1b\[[0-9;]*m/g, '').trim();
  try {
    check(clean(repl.eval(':load demo/main').output).includes('loaded demo/main'), 'load');
    const listing = repl.eval(':ls demo/ops').output;
    for (const keyword of ['op', 'op_var']) {
      check(listing.includes(`\x1b[38;2;229;192;123mpub\x1b[0m \x1b[38;2;229;192;123m${keyword}\x1b[0m`), `${keyword} listing colors`);
    }
    for (const [input, expected] of [
      [':sig arithmetic.(op _ +', 'pub op _ + _ { impl choose }'],
      [':sig arithmetic.(op_var [*', 'pub op_var [* *] { foldr choose empty }'],
      [':sig arithmetic.(op_var []', 'pub op_var [] { foldr choose empty }'],
      [':sig arithmetic.(op _ ?', 'pub op _ ? __ : ___ { impl choose_three }'],
    ]) {
      const result = repl.complete(input, input.length);
      check(result.replaceStart === 5 && result.replaceEnd === input.length, 'whole selector range');
      const selected = result.candidates.find(item => item.kind === 'op' && item.label.startsWith(input.slice(5)));
      check(selected !== undefined && selected.label.endsWith(')'), 'qualified selector');
      const inserted = input.slice(0, result.replaceStart) + selected.label + input.slice(result.replaceEnd);
      check(clean(repl.eval(inserted).output) === expected, 'accepted insertion');
    }
    check(clean(repl.eval(':sig arithmetic.(op _ (=) _)').output) === 'pub op _ (=) _ { impl choose }', 'inner parentheses');
    check(clean(repl.eval(':sig arithmetic.(op_var [**])').output) === 'pub op_var [* *] { foldr choose empty }', 'compact marked selector');
    const refs = clean(repl.eval(':refs arithmetic.(op _ + _)').output);
    check(refs.includes('in fn use') && !refs.includes('demo/other'), 'operator reference identity');
    for (const selector of ['arithmetic.(op_var [* *])', 'op_var [% %]']) {
      const refs = clean(repl.eval(':refs ' + selector).output);
      check(refs.includes('in fn use'), `${selector}: imported marked frame use: ${refs}`);
      check(refs.includes('in fn browser_alias_use'), `${selector}: aliased marked frame use: ${refs}`);
      check(refs.includes('in op_var [* *]'), `${selector}: provider declaration: ${refs}`);
      check(!refs.includes('demo/other'), `${selector}: foreign operator identity: ${refs}`);
    }
    const foreignRefs = clean(repl.eval(':refs other.(op_var [* *])').output);
    check(foreignRefs.includes('in fn foreign_marked_use'), `competing provider use: ${foreignRefs}`);
    check(!foreignRefs.includes('in fn use') && !foreignRefs.includes('in fn browser_alias_use'), `competing provider identity: ${foreignRefs}`);
    repl.eval(':reset');
    const input = ':sig arithmetic.(op _ +';
    check(repl.complete(input, input.length).candidates.length === 0, 'reset candidates');
    return 4;
  } finally { repl.free(); }
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = exerciseSelectors)]
    fn exercise_selectors(repl: JsValue) -> Result<u32, JsValue>;
}

#[wasm_bindgen_test]
pub fn declaration_selectors_cross_public_javascript_boundaries() {
    assert_eq!(
        exercise_selectors(super::declaration_selectors_repl().into()).unwrap(),
        4
    );
}

#[wasm_bindgen_test]
pub fn trailing_blocks_cross_public_javascript_boundaries() {
    let repl = super::trailing_blocks_repl();
    assert_eq!(exercise_trailing_blocks(repl.into()).unwrap(), 6);
}

#[wasm_bindgen(inline_js = r#"
export function exerciseTypeOnlyValues(repl) {
  const check = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const turn = input => {
    const result = repl.eval(input);
    check(result.keep_running === true && result.is_error === false, `${input}: turn status`);
    return result.output.replace(/\x1b\[[0-9;]*m/g, '').trim();
  };
  try {
    check(turn(':load app').includes('loaded app'), 'fixture load');
    for (const [input, expected] of [
      [':type mark', 'mark : [A] .'],
      [':type .[B] { () }', '.[B] { () } : [B] .'],
      [':normalize mark[.]', '()'],
      [':normalize .[B] { () }[.]', '()'],
      [':type delayed[.]', 'delayed[.] : . -> .'],
      [':normalize delayed[.](())', '()'],
    ]) {
      const actual = turn(input);
      check(actual === expected, `${input}: ${actual}`);
    }
    for (const input of [':normalize mark', ':normalize delayed[.]']) {
      const actual = turn(input);
      check(actual.startsWith('<closure ') && actual.endsWith('>') && actual.length > '<closure >'.length, `${input}: ${actual}`);
    }
    for (const input of [':normalize .[', ':normalize .[Sco']) {
      check(repl.complete(input, input.length).candidates.length === 0, `${input}: binder introduction`);
    }
    for (const [binder, prefix] of [['Scoped', 'Sco'], ['Other', 'Ot']]) {
      const input = `:normalize .[${binder}] { .(value: ${prefix}`;
      const result = repl.complete(input, input.length);
      check(result.replaceStart === input.length - prefix.length && result.replaceEnd === input.length, 'binder replacement');
      check(result.candidates.length === 1, `${binder}: exact candidates`);
      const selected = result.candidates[0];
      check(selected.label === binder && selected.kind === 'type-param', `${binder}: selected binding`);
      const outside = `:normalize .(value: ${prefix}`;
      check(repl.complete(outside, outside.length).candidates.length === 0, `${binder}: binder scope exit`);
      const inserted = input.slice(0, result.replaceStart) + selected.label + input.slice(result.replaceEnd) + ') { value } }[.](())';
      check(turn(inserted) === '()', `${binder}: accepted insertion`);
    }
    return 2;
  } finally {
    repl.free();
  }
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = exerciseTypeOnlyValues)]
    fn exercise_type_only_values(repl: JsValue) -> Result<u32, JsValue>;
}

#[wasm_bindgen_test]
pub fn type_only_values_cross_public_javascript_boundaries() {
    assert_eq!(
        exercise_type_only_values(super::type_only_values_repl().into()).unwrap(),
        2
    );
}

#[wasm_bindgen(inline_js = r#"
export function exerciseInspection(repl) {
  try {
    repl.eval(':load app');
    for (const [input, kind] of [[':sig identity', 'pure fn'], [':sig emit', 'host fn'], [':sig Text', 'host type'], [':sig inspect', 'macro']]) {
      const item = repl.complete(input, input.length).candidates.find(item => item.label === input.slice(5));
      if (!item || item.kind !== kind || !item.detail.includes(kind)) throw new Error(`${input}: ${JSON.stringify(item)}`);
    }
    const expression = repl.complete('ide', 3).candidates.find(item => item.label === 'identity');
    if (!expression || !expression.detail.includes('pure fn identity')) throw new Error(`expression: ${JSON.stringify(expression)}`);
    const normalized = ':normalize ide';
    const inNormalize = repl.complete(normalized, normalized.length).candidates.find(item => item.label === 'identity');
    if (!inNormalize || !inNormalize.detail.includes('pure fn identity')) throw new Error(`normalize: ${JSON.stringify(inNormalize)}`);
    const shadow = '.(identity: . -> .) { ide';
    const local = repl.complete(shadow, shadow.length).candidates.find(item => item.label === 'identity');
    if (!local || local.detail.includes('pure fn')) throw new Error(`shadow: ${JSON.stringify(local)}`);
    const signature = repl.eval(':sig identity').output.replace(/\x1b\[[0-9;]*m/g, '');
    if (!signature.includes('pure fn identity')) throw new Error(`signature: ${signature}`);
  } finally { repl.free(); }
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = exerciseInspection)]
    fn exercise_inspection(repl: JsValue) -> Result<(), JsValue>;
}

#[wasm_bindgen_test]
pub fn inspection_crosses_the_public_javascript_boundary() {
    exercise_inspection(super::inspection_repl().into()).unwrap();
}
