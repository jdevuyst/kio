import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import * as onigurumaMod from "vscode-oniguruma";
import * as vsctmMod from "vscode-textmate";

const oniguruma = onigurumaMod.default ?? onigurumaMod;
const vsctm = vsctmMod.default ?? vsctmMod;
const positive = /^(?:keyword|entity\.name|variable|constant|support|punctuation)\./;
const keyword = "keyword.declaration.kio";
const identifier = "variable.other.kio";
const functionName = "entity.name.function.kio";
const fixtures = [];

function fixture(name, source, targets) {
  assert.ok(!fixtures.some((fixture) => fixture.name === name), `${name}: unique fixture name`);
  const spans = new Set();
  fixtures.push({ name, source, targets: targets.map(([needle, word, scope]) => {
    const index = source.indexOf(needle);
    assert.ok(index >= 0 && source.indexOf(needle, index + 1) < 0, `${name}: unique ${needle}`);
    assert.ok(word.length > 0 && needle.includes(word), `${name}: target must be inside its needle`);
    const start = index + needle.indexOf(word);
    const end = start + word.length;
    assert.ok(start >= 0 && end <= source.length && !word.includes("\n"), `${name}: single-line target span`);
    assert.equal(source.slice(start, end), word, `${name}: target source bytes`);
    assert.ok(!spans.has(`${start}:${end}`), `${name}: unique target span`);
    spans.add(`${start}:${end}`);
    return { start, end, text: word, scope };
  }) });
}

fixture("marked blocks use generic heads and labels", "module controls; fn f() { outer! value { a() } fallback inner! next { b() } otherwise { c() } }", [
  ["outer! value", "outer!", "entity.name.function.macro.elaborator.kio"],
  ["fallback inner!", "fallback", "keyword.control.kio"],
  ["inner! next", "inner!", "entity.name.function.macro.elaborator.kio"],
  ["otherwise {", "otherwise", "keyword.control.kio"],
]);
fixture("marked blocks preserve prefix and ordinary names", "module controls; fn f(if: ., do: ., fallback: .) { outer! convert!(if) { do; fallback } else { if } }", [
  ["if: .", "if", identifier], ["do: .", "do", identifier],
  ["convert!(if)", "convert!", "entity.name.function.macro.elaborator.kio"],
  ["{ do;", "do", identifier], ["; fallback }", "fallback", identifier],
  ["else {", "else", "keyword.control.kio"], ["{ if }", "if", identifier],
]);
fixture("marked blocks preserve grouped child ownership", "module controls; fn f() { outer!(inner! value { a() } fallback { b() }) { c() } cleanup { d() } }", [
  ["inner! value", "inner!", "entity.name.function.macro.elaborator.kio"],
  ["fallback {", "fallback", "keyword.control.kio"],
  ["cleanup {", "cleanup", "keyword.control.kio"],
]);
fixture("marked blocks multiline and comments", "module controls; fn f() { outer!\n value\n { a() } // edge\n fallback // label\n { b() } }", [
  ["outer!\n", "outer!", "entity.name.function.macro.elaborator.kio"],
  ["fallback //", "fallback", "keyword.control.kio"],
]);
fixture("trailing descriptors are declaration local", "module controls; elab form: . -> . { trailing thunk; impl helper; trailing product fallback; trailing sequence cleanup } fn f(trailing: ., sequence: .) { trailing; sequence }", [
  ["trailing thunk", "trailing", keyword], ["thunk;", "thunk", keyword],
  ["trailing product", "product", keyword], ["fallback;", "fallback", "keyword.control.kio"],
  ["trailing sequence", "sequence", keyword], ["cleanup }", "cleanup", "keyword.control.kio"],
  ["trailing: .", "trailing", identifier], ["sequence: .", "sequence", identifier],
  ["{ trailing;", "trailing", identifier], ["; sequence }", "sequence", identifier],
]);
fixture("unmarked control spellings are ordinary", "module controls; fn f() { if(value); do(value); else(value) }", [
  ["if(value)", "if", functionName], ["do(value)", "do", functionName],
  ["else(value)", "else", functionName],
]);

fixture("newtype members and names", "module members;\nnewtype Cell : . { constructor constructor; pub projector projector; };", [
  ["constructor constructor", "constructor", keyword], ["constructor;", "constructor", functionName],
  ["projector projector", "projector", keyword], ["projector;", "projector", functionName],
]);
fixture("multiline scoped members", "module members;\nrec newtype Cell : . | Cell {\n pub(api)\n projector // member\n get;\n constructor\n make;\n};", [
  ["projector //", "projector", keyword], ["constructor\n", "constructor", keyword],
  ["get;", "get", functionName], ["make;", "make", functionName],
]);
fixture("same-line declaration owners", "module source; import helper as h; host type Text { owned }; op _ + _ { impl add; }; fn keep(owned: .) -> . { (owned) }", [
  ["import helper", "import", keyword], ["op _", "op", keyword], ["impl add", "impl", keyword],
  ["{ owned }", "owned", keyword], ["owned:", "owned", identifier], ["(owned)", "owned", identifier],
]);
fixture("same-line signature import", "signature app v(1); v(1) { with { module imports { import helper as h }; module empty {} }; breaking { remove { api.old } } }", [
  ["import helper", "import", keyword], ["helper as", "helper", "entity.name.module.kio"],
]);
fixture("host role and role name", "module roles;\nhost type Number\n role // annotation\n (i32);\nfn role(role: .) -> . { role }", [
  ["role //", "role", keyword], ["fn role", "role", functionName],
  ["role:", "role", identifier], ["{ role }", "role", identifier],
]);
fixture("incomplete role retained", "module roles;\nhost type Number role(\nfn after(role: .) -> . { role }", [
  ["role(", "role", keyword], ["role:", "role", identifier], ["{ role }", "role", identifier],
]);
fixture("incomplete member recovery", "module members;\nnewtype Cell : . { constructor\nfn after(projector: .) -> . { projector }", [
  ["constructor\n", "constructor", keyword], ["projector:", "projector", identifier], ["{ projector }", "projector", identifier],
]);
fixture("newtype wrong parents", "module members;\nfn constructor(projector: .) -> . { role(projector); constructor }", [
  ["fn constructor", "constructor", functionName], ["projector:", "projector", identifier],
  ["role(", "role", functionName], ["; constructor", "constructor", identifier],
]);
fixture("package fields", 'package cache;\nbuild { cache (); docs { md "a.md"; } target js { target "out"; docs "d"; cache "c"; } }\nbridge { cache/docs/target; }', [
  ["package cache", "cache", identifier], ["cache ()", "cache", keyword], ["docs {", "docs", keyword],
  ["target js", "target", keyword], ['target "out"', "target", identifier],
  ['docs "d"', "docs", identifier], ['cache "c"', "cache", identifier],
  ["cache/docs", "cache", identifier], ["/docs/", "docs", identifier], ["/target;", "target", identifier],
]);
fixture("config field owner and value roles", 'package palette; build { docs { md html; support md; md_out "guide"; html "html"; }; target js { md "data"; path "data"; } }', [
  ['md html', 'md', keyword], ['md html', 'html', identifier],
  ['support md', 'support', keyword], ['support md', 'md', identifier],
  ['md_out "guide"', 'md_out', keyword], ['html "html"', 'html', keyword],
  ['md "data"', 'md', identifier], ['path "data"', 'path', identifier],
]);
fixture("resolved field owner and value roles", 'lock helper; resolved { ;; git ref; ref git; path ref; commit sig; sig commit;; };', [
  ['git ref', 'git', keyword], ['git ref', 'ref', identifier],
  ['ref git', 'ref', keyword], ['ref git', 'git', identifier],
  ['path ref', 'path', keyword], ['path ref', 'ref', identifier],
  ['commit sig', 'commit', keyword], ['commit sig', 'sig', identifier],
  ['sig commit', 'sig', keyword], ['sig commit', 'commit', identifier],
]);
fixture("resolved names outside lock owner", 'module source; fn resolved(git: ., ref: ., path: ., commit: ., sig: .) -> . { (git, ref, path, commit, sig) }', [
  ['fn resolved', 'resolved', functionName],
  ...['git', 'ref', 'path', 'commit', 'sig'].map(name => [name+':', name, identifier]),
]);
fixture("multiline lock field owner", 'lock // header\nhelper; resolved { git "url"; ref "main"; commit "abc"; sig "digest"; }', [
  ['lock //', 'lock', null], ['helper;', 'helper', identifier],
  ...['git', 'ref', 'commit', 'sig'].map(name => [name+' "', name, keyword]),
]);
fixture("package multiline and incomplete", 'package docs;\nbuild\n{\n cache // value\n "cache";\n docs // body\n { md "index.md"; }\n target // id\n js { output "out"; }\n target rust {\n', [
  ["cache //", "cache", keyword], ["docs //", "docs", keyword], ["target //", "target", keyword], ["target rust", "target", keyword],
]);
fixture("malformed package neighbor recovery", 'package p;\nbuild { cache ; docs { md "index.md"; } target js { output "out"; } }\nbridge { target; }', [
  ["cache ;", "cache", keyword], ["docs {", "docs", keyword], ["target js", "target", keyword], ["{ target;", "target", identifier],
]);
fixture("package field wrong parent", "module source;\nfn build(cache: ., docs: ., target: .) -> . { cache; docs; target }", [
  ["fn build", "build", functionName], ["cache:", "cache", identifier], ["docs:", "docs", identifier],
  ["target:", "target", identifier], ["{ cache;", "cache", identifier], ["; docs;", "docs", identifier], ["; target }", "target", identifier],
]);

const recursive = (body) => `module recursive;\nrec(loop) fn again(value: .) -> . {\n  ${body}\n}`;
fixture("monomorphic recursive call", recursive("rec again(value)"), [["rec again", "rec", keyword]]);
fixture("marked recursive callee", recursive("rec _again(value)"), [["rec _again", "rec", keyword]]);
fixture("recursive argument opener on next line", recursive("rec again\n  (value)"), [["rec again", "rec", keyword]]);
fixture("incomplete recursive call", recursive("rec again("), [["rec again", "rec", keyword]]);
fixture("recursive spelling parameter", "module source;\nfn again(\n rec // parameter\n : .\n) -> . { other.rec }", [
  ["rec //", "rec", identifier], ["other.rec", "rec", identifier],
]);
fixture("recursive declaration name and ordinary call", "module source;\nfn rec(value: .) -> . { value }\nfn caller(value: .) -> . { rec(value) }", [
  ["fn rec", "rec", functionName], ["{ rec(value)", "rec", functionName],
]);
fixture("annotated recursive call", recursive("rec(poly, cont) again(value)"), [
  ["rec(poly", "rec", keyword], ["poly,", "poly", "keyword.control.kio"], ["cont)", "cont", "keyword.control.kio"],
]);
fixture("incomplete recursive sibling", recursive("rec again(") + "\nfn sibling(\n rec\n : .\n) -> . { other.rec }", [
  ["rec again", "rec", keyword], ["\n rec\n", "rec", identifier], ["other.rec", "rec", identifier],
]);
fixture("qualified recursive callee rejected", recursive("rec other.again(value)"), [["rec other", "rec", keyword]]);
for (const comment of ["", " // shared trivia"]) {
  const prefix = recursive(`rec${comment}`).slice(0, -2);
  fixture(`recursive later-line keyword${comment}`, `${prefix}\n  again(value)\n}`, [[`rec${comment}\n  again`, "rec", null]]);
  fixture(`recursive later-line ordinary${comment}`, `${prefix}\n}`, [[`rec${comment}\n}`, "rec", null]]);
}
fixture("multiline recursive annotations", recursive("rec(\n poly, // annotation\n cont,\n) again(value)"), [
  ["rec(\n", "rec", null], ["poly,", "poly", null], ["cont,", "cont", null], ["again(value)", "again", functionName],
]);
for (const body of ["rec", "rec; rec", "(rec)", "rec.field", "other.rec", "(other.rec)"]) {
  fixture(`ordinary recursive spelling value ${body}`, `module m; fn f(rec: .) { ${body} }`, [
    ["rec:", "rec", identifier], [`${body} }`, "rec", identifier],
  ]);
}
for (const args of ["()", "(())", "(poly, cont)", '("rec(poly)")', "(other.rec)"]) {
  fixture(`ordinary recursive spelling call ${args}`, `module m; fn f() { rec${args}; () }`, [[`rec${args};`, "rec", functionName]]);
}
fixture("ordinary recursive annotation names are arguments", "module m; fn f(poly: ., cont: ., escape: .) { rec(poly, cont, escape) }", [
  ["rec(poly", "rec", functionName], ["poly, cont, escape)", "poly", identifier],
  ["cont, escape)", "cont", identifier], ["escape) }", "escape", identifier],
]);
for (const fragment of ["rec(", "rec(poly)", "rec( // shared\n poly", "rec(\n poly,", "rec(value, poly)", "rec(value, poly"] ) {
  const prefix = `module m; fn f(poly: ., cont: .) {\n ${fragment}`;
  const close = fragment.endsWith(")") ? "" : fragment.endsWith(",") ? " cont)" : ")";
  const target = fragment.includes("poly") ? "poly" : "rec";
  const needle = fragment;
  fixture(`recursive annotation later-line owner ${fragment}`, `${prefix}\n ${close} again(()) }`, [[needle, target, null]]);
  fixture(`recursive annotation later-line ordinary ${fragment}`, `${prefix}\n ${close}; () }`, [[needle, target, null]]);
}
fixture("recursive annotation closer proves local modes", "module m; fn f() { rec(\n poly, cont) again(()) }", [
  ["rec(\n", "rec", null], ["poly,", "poly", "keyword.control.kio"], ["cont)", "cont", "keyword.control.kio"],
]);
fixture("ordinary recursive annotation closer proves arguments", "module m; fn f() { rec(\n poly, cont); () }", [
  ["rec(\n", "rec", null], ["poly,", "poly", identifier], ["cont)", "cont", identifier],
]);
fixture("ordinary recursive spelling chained call", "module m; fn f() { rec(\n poly)(cont); () }", [
  ["rec(\n", "rec", null], ["poly)", "poly", identifier], ["cont)", "cont", identifier],
]);
fixture("ordinary recursive spelling later chained call", "module m; fn f() { rec(\n poly)\n (cont); () }", [
  ["rec(\n", "rec", null], ["poly)", "poly", null], ["cont)", "cont", identifier],
]);
fixture("recursive callee after split annotation group", "module m; fn f() { rec // annotation\n (poly) again\n (()) }", [
  ["rec //", "rec", null], ["poly)", "poly", "keyword.control.kio"], ["again\n", "again", functionName],
]);
for (const lead of ["", "\n "]) {
  for (const tail of [") again() }", "\n ) again() }", ""]) {
    fixture(`invalid recursive annotation name ${JSON.stringify([lead, tail])}`, `module m; fn f() { rec(${lead}value, poly${tail}`, [
      ["value,", "value", identifier], ["poly", "poly", tail.startsWith(")") ? "keyword.control.kio" : null],
      ...(tail ? [["again()", "again", functionName]] : []),
    ]);
  }
  fixture(`valid recursive mode before invalid name ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}poly, value, cont) again() }`, [
    ["poly,", "poly", "keyword.control.kio"], ["value,", "value", identifier], ["cont)", "cont", "keyword.control.kio"], ["again()", "again", functionName],
  ]);
  fixture(`ordinary call with annotation-shaped names ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}poly, value, cont); () }`, [
    ["poly,", "poly", identifier], ["value,", "value", identifier], ["cont)", "cont", identifier],
  ]);
  fixture(`invalid recursive annotation nested tail ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}value(poly, (cont)), escape) again() }`, [
    [`rec(${lead}`, "rec", lead ? null : keyword],
  ]);
}
fixture("invalid recursive annotation comment continuation", "module m; fn f() { rec(\n value, // invalid mode\n poly, cont) again() }", [
  ["value,", "value", identifier], ["poly,", "poly", "keyword.control.kio"], ["cont)", "cont", "keyword.control.kio"],
]);
fixture("invalid recursive annotation unfinished parent", "module m; fn f() { rec(value, poly\nfn sibling(cont: .) { cont }", [
  ["value,", "value", identifier], ["poly\n", "poly", null], ["cont:", "cont", identifier],
]);
for (const lead of ["", "\n "]) {
  fixture(`ordinary recursive call unknown mode name ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}value, poly); () }`, [
    ["value,", "value", identifier], ["poly)", "poly", identifier],
  ]);
  fixture(`recursive duplicate mode keeps syntactic roles ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}poly, poly) again() }`, [
    ["poly,", "poly", "keyword.control.kio"], ["poly)", "poly", "keyword.control.kio"], ["again()", "again", functionName],
  ]);
  fixture(`ordinary recursive call duplicate mode names ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}poly, poly); () }`, [
    ["poly,", "poly", identifier], ["poly)", "poly", identifier],
  ]);
}
fixture("ordinary recursive call nested argument callee", "module m; fn f() { rec(\n value(poly), escape); () }", [
  ["value(", "value", functionName], ["poly)", "poly", identifier], ["escape)", "escape", identifier],
]);
for (const lead of ["", "\n "]) {
  for (const content of ["", "(poly)", "poly, 123", "poly, (cont), escape", 'poly, "cont", escape', "poly, {cont}, escape", "poly, [cont], escape"]) {
    const targets = [["again()", "again", functionName]];
    if (content.includes("poly")) targets.push(["poly", "poly", content.startsWith("(") ? identifier : "keyword.control.kio"]);
    if (content.includes("cont") && !content.includes('"')) targets.push(["cont", "cont", identifier]);
    if (content.includes("escape")) targets.push(["escape", "escape", "keyword.control.kio"]);
    fixture(`recursive balanced non-identifier annotation ${JSON.stringify([lead, content])}`, `module m; fn f() { rec(${lead}${content}) again() }`, targets);
  }
}
for (const lead of ["", "\n "]) {
  fixture(`recursive annotation returns to parent slot ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}(poly), cont) again() }`, [
    ["poly", "poly", identifier], ["cont)", "cont", "keyword.control.kio"], ["again()", "again", functionName],
  ]);
  fixture(`ordinary recursive call parent slot inverse ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}(poly), cont); () }`, [
    ["poly", "poly", identifier], ["cont)", "cont", identifier],
  ]);
  fixture(`ordinary recursive call nested group callee ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}(value(poly)), cont); () }`, [
    ["value(", "value", functionName], ["poly)", "poly", identifier], ["cont)", "cont", identifier],
  ]);
  fixture(`recursive annotation comma runs ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead},,poly,, cont,,) again() }`, [
    ["poly,", "poly", "keyword.control.kio"], ["cont,", "cont", "keyword.control.kio"], ["again()", "again", functionName],
  ]);
  fixture(`ordinary recursive call comma runs ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead},,poly,, cont,,); () }`, [
    ["poly,", "poly", identifier], ["cont,", "cont", identifier],
  ]);
  fixture(`recursive comma-only annotation envelope ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead},,,) again() }`, [
    ["again()", "again", functionName],
  ]);
  fixture(`recursive missing comma retains item boundary ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}value poly, cont) again() }`, [
    ["value poly", "value", identifier], ["poly,", "poly", identifier], ["cont)", "cont", "keyword.control.kio"], ["again()", "again", functionName],
  ]);
}
for (const word of ["poly", "cont"]) {
  for (const tail of [") again() }", "); () }"]) {
    fixture(`recursive comma runs later-line ${word} ${tail}`, `module m; fn f() { rec(\n ,,poly,, cont,, // shared\n ${tail}`, [
      [`${word},`, word, null],
    ]);
  }
}
fixture("recursive item boundary survives comment newline", "module m; fn f() { rec(\n value // same item\n poly, cont) again() }", [
  ["value //", "value", identifier], ["poly,", "poly", identifier], ["cont)", "cont", "keyword.control.kio"], ["again()", "again", functionName],
]);
for (const lead of ["", "\n "]) {
  fixture(`recursive annotation variadic item ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}[* poly, poly *], cont) again() }`, [
    ["poly,", "poly", identifier], ["poly *]", "poly", identifier], ["cont)", "cont", "keyword.control.kio"], ["again()", "again", functionName],
  ]);
  fixture(`recursive annotation nested variadic item ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}[* [% poly, cont %], escape *], poly) again() }`, [
    ["poly,", "poly", identifier], ["cont %]", "cont", identifier], ["escape *]", "escape", identifier], ["poly)", "poly", "keyword.control.kio"], ["again()", "again", functionName],
  ]);
  fixture(`recursive annotation unclosed variadic item ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}[* poly, poly, cont) again() }`, [
    ["[* poly,", "poly", identifier], ["poly, cont", "poly", identifier], ["cont)", "cont", identifier], ["again()", "again", functionName],
  ]);
  fixture(`recursive annotation mismatched variadic item ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}[* poly, poly +], cont) again() }`, [
    ["poly,", "poly", identifier], ["poly +]", "poly", identifier], ["again()", "again", functionName],
  ]);
  fixture(`recursive annotation non-ASCII item boundary ${JSON.stringify(lead)}`, `module m; fn f() { rec(${lead}\u00a0poly, cont) again() }`, [
    ["poly,", "poly", identifier], ["cont)", "cont", "keyword.control.kio"],
  ]);
}
fixture("raw ordinary recursive spelling closed value", "rec;", [["rec;", "rec", identifier]]);
fixture("raw ordinary recursive spelling closed call", "rec(poly);", [["rec(poly)", "rec", functionName], ["poly)", "poly", identifier]]);
fixture("raw annotated recursive call", "rec(poly) again()", [["rec(poly)", "rec", keyword], ["poly)", "poly", "keyword.control.kio"]]);
fixture("raw recursive declaration callable mode name", "rec(poly) fn again() { () }", [["rec(poly)", "rec", keyword], ["poly)", "poly", identifier], ["fn again", "fn", keyword]]);
fixture("raw recursive declaration split callable", "rec(\n poly) fn again() { () }", [["rec(\n", "rec", null], ["poly)", "poly", identifier], ["fn again", "fn", keyword]]);
fixture("raw ordinary recursive call terminator retains expression root", "rec(\n poly);\nfn later() { () }", [["rec(\n", "rec", null], ["poly)", "poly", identifier], ["fn later", "fn", identifier]]);
fixture("raw split recursive keyword callee remains a name", "rec\n fn(())", [["rec\n", "rec", null], ["fn(())", "fn", functionName]]);
fixture("raw split recursive type group", "rec\n { type A = B; type B = A; }", [["rec\n", "rec", null], ["type A", "type", keyword], ["type B", "type", keyword]]);
for (const name of ["newtype", "labels"]) {
  fixture(`raw split annotated recursive callee ${name}`, `rec(poly)\n ${name}(())`, [["poly)", "poly", null], [`${name}(())`, name, functionName]]);
  fixture(`raw multiline annotated recursive callee ${name}`, `rec(\n poly) ${name}(())`, [["poly)", "poly", "keyword.control.kio"], [`${name}(())`, name, functionName]]);
}
fixture("raw split recursive data declaration", "rec\n newtype N : . { constructor make; projector get; };", [["newtype N", "newtype", keyword], ["constructor make", "constructor", keyword]]);
for (const source of ["rec(value, poly) again()", "rec(\n value, poly) again()", "rec(value,\n poly"]) {
  fixture(`raw invalid recursive annotation name ${source}`, source, [["value,", "value", identifier], ["poly", "poly", source.endsWith("poly") ? null : "keyword.control.kio"]]);
}
for (const prefix of ["rec", "rec(poly)", "rec(\n poly"]) {
  const suffix = prefix.endsWith("poly") ? ")" : "";
  const target = prefix.includes("poly") ? "poly" : "rec";
  const declaration = prefix === "rec" ? "newtype N : . { constructor make; projector get; };" : "fn again() { () }";
  fixture(`raw recursion later-line declaration ${prefix}`, `${prefix}\n ${suffix} ${declaration}`, [[prefix, target, null]]);
  fixture(`raw recursion later-line ordinary ${prefix}`, `${prefix}\n ${suffix};`, [[prefix, target, null]]);
}
fixture("annotation inverse", "module m;\nfn f(poly: ., cont: .) -> . { poly(cont) }", [
  ["poly:", "poly", identifier], ["cont:", "cont", identifier], ["poly(cont)", "poly", functionName], ["(cont)", "cont", identifier],
]);
fixture("recursive group callable", "module m;\nrec(\n loop\n) fn f(loop: .) -> . { loop }", [
  ["rec(\n", "rec", keyword], ["\n loop\n", "loop", identifier], ["loop:", "loop", identifier], ["{ loop }", "loop", identifier],
]);
fixture("recursive group qualified callable", "module m;\nrec(helper.rec) fn f() -> . { () }", [["helper.rec", "rec", identifier]]);
fixture("multiline declaration-word parameter", "module m;\nfn f(\n fn\n : .,\n newtype\n : .,\n host\n : .\n) -> . { fn; newtype; host }", [
  ["\n fn\n", "fn", identifier], ["\n newtype\n", "newtype", identifier], ["\n host\n", "host", identifier],
  ["{ fn;", "fn", identifier], ["; newtype;", "newtype", identifier], ["; host }", "host", identifier],
]);

fixture("split varop declaration", "module ops;\nvarop [* *] { foldl join empty; finalize finish; };", [
  ["varop [", "varop", keyword], ["foldl join", "foldl", keyword], ["finalize finish", "finalize", keyword],
]);
fixture("split varop import", "module ops;\nimport collect(varop [* *], varop);", [
  ["varop [", "varop", keyword], ["varop);", "varop", identifier],
]);
fixture("varop multiline head", "module ops;\nvarop [+\n +] { foldr join empty; };", [
  ["varop [+", "varop", keyword], ["foldr join", "foldr", keyword],
]);
fixture("varop inverse", "module ops;\nfn varop(varop: .) -> . { varop }", [
  ["fn varop", "varop", functionName], ["varop:", "varop", identifier], ["{ varop }", "varop", identifier],
]);
fixture("maximal bracket runs and rejected marker", "module ops;\nfn apply() -> . { #[* #1, #2 *]; .+[+ 1, 2 +] }", [
  ["#[*", "#[*", "keyword.operator.user.kio"], [".+[+", ".+[+", "keyword.operator.user.kio"],
]);
fixture("multiline target keys", 'package p;\nbuild { target js {\n target "a";\n docs "b";\n cache "c";\n} }', [
  ['target "a"', "target", identifier], ['docs "b"', "docs", identifier], ['cache "c"', "cache", identifier],
]);
fixture("wrong host role parent", "module h;\nhost type N { role };\nfn next(role: .) -> . { role }", [
  ["{ role };", "role", identifier], ["role:", "role", identifier], ["-> . { role }", "role", identifier],
]);
fixture("member missing terminator", "module m;\nnewtype N : . { constructor make projector; };", [["make projector", "projector", identifier]]);
fixture("multiline function name", "module m;\nfn // declaration\n again(value: .) -> . { rec again(value) }", [
  ["again(value:", "again", functionName], ["rec again", "rec", keyword],
]);
fixture("multiline host head", "module m;\nhost // declaration\n type Number role(i32);", [["role(i32)", "role", keyword]]);
fixture("recursive name in member path", "module m;\nfn f() -> . { other.rec again(); other.\n rec again() }", [
  ["other.rec", "rec", identifier], ["\n rec again", "rec", identifier],
]);
fixture("newtype missing payload", "module m;\nnewtype N { constructor make; };", [["constructor make", "constructor", identifier]]);
fixture("newtype empty payload", "module m;\nnewtype N : { projector get; };", [["projector get", "projector", identifier]]);
fixture("newtype polymorphic payload", "module m;\nnewtype Cell[A]<B> : Pair(A, B) { constructor make; projector get; };", [
  ["constructor make", "constructor", keyword], ["projector get", "projector", keyword],
]);
for (const payload of ["{field}", "Box({field})", "T | {field}", "T -> {field}", "{\n field\n}"]) {
  fixture(`rejected newtype braced payload ${payload}`, `module m;\nnewtype N : ${payload} { constructor make; projector get; };\nfn after(projector: .) -> . { projector }`, [
    ["constructor make", "constructor", identifier], ["projector get", "projector", identifier],
    ["fn after", "after", functionName], ["projector:", "projector", identifier], ["{ projector }", "projector", identifier],
  ]);
}
for (const payload of ["Box", "Box(.)", "(T & U)", "[A] A -> A", "T | U", "T -> U", "(\n T\n)", "!"]) {
  fixture(`admitted newtype payload ${payload}`, `module m;\nnewtype N : ${payload} { constructor make; projector get; };`, [
    ["constructor make", "constructor", keyword], ["projector get", "projector", keyword],
  ]);
}
for (const [sibling, targets] of [
  ["type Next = .;", [["type Next", "type", keyword]]],
  ["literal unit = .t;", [["literal unit", "literal", keyword]]],
  ["labels { tag: . };", [["labels {", "labels", keyword]]],
  ["elab macro : . -> . { impl handler; };", [["elab macro", "elab", keyword], ["impl handler", "impl", keyword]]],
  ["varop [* *] { foldl join empty; };", [["varop [*", "varop", keyword], ["foldl join", "foldl", keyword]]],
  ["op _ + _ { impl add; };", [["op _", "op", keyword], ["impl add", "impl", keyword]]],
  ["import app as local;", [["import app", "import", keyword]]],
  ["equiv same() { (); () }", [["equiv same", "equiv", keyword], ["same()", "same", functionName]]],
  ["rec(loop) fn again() { rec again() }", [["rec(loop)", "rec", keyword], ["rec again", "rec", keyword]]],
]) {
  fixture(`invalid newtype sibling ${sibling}`, `module m;\nnewtype N : Box({field}) { constructor make; };\n${sibling}`, [
    ["constructor make", "constructor", identifier], ...targets,
  ]);
  for (const head of ["varop(())", "varop", "varop [+"]) {
    fixture(`invalid varop sibling ${head} ${sibling}`, `module m;\n${head}\n${sibling}`, targets);
  }
}
fixture("comment keywords", "// rec again() constructor projector role cache docs target", [["// rec again() constructor projector role cache docs target", "// rec again() constructor projector role cache docs target", "comment.line.double-slash.kio"]]);
fixture("invalid newtype nested-group recovery", "module m;\nnewtype N : Box({field(} { constructor make; };\ntype Next = .;", [
  ["constructor make", "constructor", identifier], ["type Next", "type", keyword],
]);
fixture("string keywords", '"rec again() constructor projector role cache docs target"', [
  ['"rec', '"', "string.quoted.double.kio"],
  ["rec again() constructor projector role cache docs target", "rec again() constructor projector role cache docs target", "string.quoted.double.kio"],
  ['target"', '"', "string.quoted.double.kio"],
]);
fixture("marker before comment", "module m;\nfn f() -> . { .+// trailing\n () }", [
  [".+//", ".+", "keyword.operator.user.kio"], ["// trailing", "// trailing", "comment.line.double-slash.kio"],
]);
fixture("marker second slash", "module m;\nfn f() -> . { .+/+/ }", [[".+/+/", ".+/+", "keyword.operator.user.kio"]]);
for (const word of ["if", "do"]) {
  fixture(`${word} ordinary body value`, `module m;\nfn f(${word}: .) -> . { ${word} }`, [
    [`${word}:`, word, identifier], [`{ ${word} }`, word, identifier],
  ]);
  fixture(`${word} ordinary call`, `module m;\nfn f() -> . { ${word}(()) }`, [[`${word}(()`, word, functionName]]);
  fixture(`${word} parenthesized control`, `module m;\nfn f() -> . { ${word}!(pred(())) { () }${word === "if" ? " else { () }" : ""} }`, [[`${word}!(pred`, `${word}!`, "entity.name.function.macro.elaborator.kio"]]);
  fixture(`${word} multiline receiver`, `module m;\nfn f() -> . { ${word}! pred(())\n { () }${word === "if" ? " else { () }" : ""}\n}`, [[`${word}! pred`, `${word}!`, "entity.name.function.macro.elaborator.kio"]]);
  for (const suffix of ["", " // shared trivia", "(pred())"]) {
    const prefix = `module m;\nfn f() -> . {\n ${word}!${suffix}`;
    fixture(`${word} later-line construct ${suffix}`, `${prefix}\n ${suffix.startsWith("(") ? "" : "pred(()) "}{ () }${word === "if" ? " else { () }" : ""}\n}`, [[`${word}!${suffix}\n`, `${word}!`, "entity.name.function.macro.elaborator.kio"]]);
    fixture(`${word} later-line ordinary ${suffix}`, `${prefix}\n}`, [[`${word}!${suffix}\n`, `${word}!`, "entity.name.function.macro.elaborator.kio"]]);
  }
}
fixture("conditional else owner", "module m;\nfn f(else: .) -> . { if! .t { else } else { else } }", [
  ["else:", "else", identifier], ["{ else } else", "else", identifier],
  ["else { else", "else", "keyword.control.kio"], ["{ else } }", "else", identifier],
]);
fixture("else multiline owner", "module m;\nfn f() -> . {\n if! .t { () }\n // branch\n else\n { () }\n}", [["else\n", "else", "keyword.control.kio"]]);
fixture("else identifier condition", "module m;\nfn f(else: .) -> . { if! else { () } else { () } }", [
  ["if! else", "else", identifier], ["} else", "else", "keyword.control.kio"],
]);
fixture("else outside conditional", "module m;\nfn f(else: .) -> . { scope! { () }; else(else); else }", [
  ["else(else", "else", functionName], ["(else)", "else", identifier], ["; else }", "else", identifier],
]);
fixture("nested if condition", "module m;\nfn f() -> . { if!(if! .t { .t } else { .f }) { () } else { () } }", [
  ["if!(if!", "if!", "entity.name.function.macro.elaborator.kio"], ["if! .t", "if!", "entity.name.function.macro.elaborator.kio"],
  ["else { .f", "else", "keyword.control.kio"], ["else { ()", "else", "keyword.control.kio"],
]);
fixture("conditional label condition", "module m;\nfn f() -> . { if!({flag}) { () } else { () } }", [["else {", "else", "keyword.control.kio"]]);
fixture("conditional do condition", "module m;\nfn f() -> . { if!(scope! { .t }) { () } else { () } }", [
  ["scope! {", "scope!", "entity.name.function.macro.elaborator.kio"], ["else {", "else", "keyword.control.kio"],
]);
fixture("else if chain", "module m;\nfn f() -> . { if! .t { () } else if! .f { () } else { () } }", [
  ["else if!", "else", "keyword.control.kio"], ["if! .f", "if!", "entity.name.function.macro.elaborator.kio"], ["else {", "else", "keyword.control.kio"],
]);
fixture("conditional lambda condition", "module m;\nfn f() -> . { if!(.(if: .) { if }) { () } else { () } }", [
  ["if:", "if", identifier], ["{ if }", "if", identifier], ["else {", "else", "keyword.control.kio"],
]);
fixture("recursive conditional condition", "module m;\nfn f() -> . { if! rec again() { () } else { () } }", [
  ["rec again", "rec", keyword], ["else {", "else", "keyword.control.kio"],
]);
fixture("elaborator conditional condition", "module m;\nfn f() -> . { if! pred!() { () } else { () } }", [
  ["pred!", "pred!", "entity.name.function.macro.elaborator.kio"], ["else {", "else", "keyword.control.kio"],
]);
for (const [open, close] of [["[*", "*]"], ["*[", "]*"]]) {
  fixture(`varop conditional condition ${open}`, `module m;\nfn f() -> . { if!(${open} {flag}, .t ${close}) { () } else { () } }`, [["else {", "else", "keyword.control.kio"]]);
  fixture(`varop contextual element ${open}`, `module m;\nfn f() -> . { ${open} if, do ${close} }`, [["if,", "if", identifier], [`do ${close}`, "do", identifier]]);
}
fixture("local binding keyword", "module m;\nfn f(let: .) -> . { let x = let; x }", [
  ["let:", "let", identifier], ["let x", "let", keyword], ["= let;", "let", identifier],
]);
fixture("binding is statement only", "module m;\nfn f() -> . { call(let x = ()); let y = call(let z = ()); y }", [
  ["let x", "let", identifier], ["let y", "let", keyword], ["let z", "let", identifier],
]);
fixture("binding contextual names", "module m;\nfn f() -> . { let .(if: .) = (); let .(do: ., else: .) = pair; let .(<Type> rec) = packed; rec }", [
  ["let .(if", "let", keyword], ["if:", "if", identifier], ["let .(do", "let", keyword],
  ["do:", "do", identifier], ["else:", "else", identifier], ["let .(<", "let", keyword], ["rec)", "rec", identifier],
]);
fixture("ordinary let calls and operators", "module m;\nfn f() -> . { let(value); let(value) == other; let x == other }", [
  ["let(value);", "let", functionName], ["let(value) ==", "let", functionName], ["let x", "let", keyword],
]);
for (const suffix of ["", " // shared trivia", " ."]) {
  const prefix = `module m;\nfn f() -> . {\n let${suffix}`;
  fixture(`binding later-line keyword ${suffix}`, `${prefix}\n ${suffix === " ." ? "(x)" : "x"} = (); x\n}`, [[`let${suffix}\n`, "let", null]]);
  fixture(`binding later-line identifier ${suffix}`, `${prefix}\n}`, [[`let${suffix}\n`, "let", null]]);
}
fixture("do statement siblings", "module m;\nfn f() -> . { do! receiver { let if <- action(); let do = if; do }; let else = (); else }", [
  ["do! receiver", "do!", "entity.name.function.macro.elaborator.kio"], ["let if", "let", keyword], ["if <-", "if", identifier],
  ["let do", "let", keyword], ["do =", "do", identifier], ["if;", "if", identifier], ["do };", "do", identifier],
  ["let else", "let", keyword], ["else =", "else", identifier], ["else }", "else", identifier],
]);
fixture("conditional statement siblings", "module m;\nfn f() -> . { if! .t { let else = (); else } else { let if = (); if }; let do = (); do }", [
  ["let else", "let", keyword], ["else =", "else", identifier], ["else } else", "else", identifier],
  ["} else", "else", "keyword.control.kio"], ["let if", "let", keyword], ["if =", "if", identifier],
  ["if };", "if", identifier], ["let do", "let", keyword], ["do }", "do", identifier],
]);
fixture("incomplete control sibling", "module m;\nfn broken() -> . { if! value(\nfn after(if: ., do: ., else: .) -> . { if; do; else }", [
  ["if! value", "if!", "entity.name.function.macro.elaborator.kio"], ["if:", "if", identifier], ["do:", "do", identifier], ["else:", "else", identifier],
  ["{ if;", "if", identifier], ["; do;", "do", identifier], ["; else }", "else", identifier],
]);
fixture("labels shield control names", "module m;\nfn f() -> . { { if = (), do = (), else = (), let = () } }", [
  ["if =", "if", "entity.name.label.kio"], ["do =", "do", "entity.name.label.kio"],
  ["else =", "else", "entity.name.label.kio"], ["let =", "let", "entity.name.label.kio"],
]);
fixture("declaration payload parents", "module m;\nliteral if = .t;\ntype T = if.T;\nlabels { do: else.T, let: rec.T };\nelab let: if.T { captures do.else; impl(fills) if.let; };\nequiv do() -> . { if(()) }", [
  ["literal if", "if", identifier], ["= if.T", "if", identifier], ["{ do:", "do", identifier], ["do: else", "else", identifier],
  ["elab let", "let", identifier], ["let: if", "if", identifier], ["captures do", "captures", keyword], ["do.else", "do", identifier],
  ["impl(fills)", "impl", keyword], ["fills)", "fills", keyword], ["if.let", "if", identifier],
  ["equiv do", "do", functionName], ["{ if(())", "if", functionName],
]);
fixture("fixed operator body parents", "module m;\nop _ + _ { impl if.let; };", [
  ["op _", "op", keyword], ["impl if", "impl", keyword], ["if.let", "if", identifier], ["let;", "let", identifier],
]);
for (const head of ["[* *]", "[+ +]", "[[ ]]", "/[ ]/", "[/ /]", "*[ ]*"]) {
  fixture(`varop delimiter ${head}`, `module ops;\nvarop ${head} { foldl join empty; };`, [
    [`varop ${head}`, "varop", keyword], ["foldl join", "foldl", keyword],
  ]);
}
for (const head of ["[**]", "[ _ ]", "[ ]"]) {
  fixture(`invalid varop head ${head}`, `module ops;\nvarop ${head} { foldl join empty; };`, [
    [`varop ${head}`, "varop", keyword], ["foldl join", "foldl", null],
  ]);
}
for (const head of ["[+ *]", "+[< >]+", "[+\n *]", "[+ + ]", "[+ +// split\n ]", "[+ +] extra"]) {
  fixture(`mismatched varop head ${head}`, `module ops;\nvarop ${head} { foldl join empty; };\nfn after(foldl: .) -> . { foldl }`, [
    ["foldl join", "foldl", null], ["foldl:", "foldl", identifier], ["{ foldl }", "foldl", identifier],
  ]);
}
for (const head of ["+[< <]+", "+[<\n <]+", "**[ ]**", "[[\n ]]", "[+ // split\n +]"]) {
  fixture(`mirrored varop head ${head}`, `module ops;\nvarop ${head} { foldl join empty; };`, [["foldl join", "foldl", keyword]]);
}

fixture("slots in type and scalar binding positions", "module m;\nlabels Shared = { field: _ };\nfn run() -> . { let _ = (); let scalar = (); scalar }", [
  ["field: _", "_", "variable.parameter.slot.kio"], ["let _", "_", "variable.parameter.slot.kio"],
  ["let scalar", "scalar", identifier], ["; scalar }", "scalar", identifier],
]);
fixture("monadic binding arrow taxonomy", "module m;\nfn run() -> . { do! bind { let scalar <- value; scalar } }", [
  ["scalar <-", "<-", "keyword.operator.user.kio"], ["let scalar", "scalar", identifier],
]);
fixture("placeholder lambda owns executable body", "module m;\nfn run() -> . { .x. { print(x1) } }", [["print(x1)", "print", functionName]]);
fixture("placeholder lambda missing reference buffer", "module m;\nfn run() -> . { .x. // stem\n { let item = (); print(item) } }", [
  ["let item", "let", keyword], ["print(item)", "print", functionName],
]);
fixture("multiline placeholder lambda body", "module m;\nfn run() -> . { .x. // stem\n { let item = x1; print(item) } }", [
  ["let item", "let", keyword], ["print(item)", "print", functionName],
]);
fixture("placeholder lambda in do receiver", "module m;\nfn run() -> . { do!(.x. { bind(x1) }) { let item <- value; item } }", [
  ["do!(.x.", "do!", "entity.name.function.macro.elaborator.kio"], ["bind(x1)", "bind", functionName], ["let item", "let", keyword],
]);
for (const stem of ["x", "t", "f", "_arg", "x1_y"]) {
  fixture(`placeholder stem and ordinary references ${stem}`, `module m;\nfn run() { .${stem}. { (${stem}1, ${stem}2()) } }`, [
    [`.${stem}.`, stem, "variable.parameter.kio"],
    [`${stem}1,`, `${stem}1`, identifier], [`${stem}2()`, `${stem}2`, functionName],
  ]);
}
fixture("booleans retain their ordinary role", "module m;\nfn run() { (.t, .f) }", [
  [".t,", ".t", "constant.language.boolean.kio"], [".f)", ".f", "constant.language.boolean.kio"],
]);
fixture("explicit row and existential let patterns", "module m;\nfn run() { let .({field as x, other}) = row; let .(<A> packed) = value; packed }", [
  ["let .({", "let", keyword], ["field as", "as", keyword], ["as x", "x", identifier],
  ["let .(<", "let", keyword], ["packed)", "packed", identifier],
]);
fixture("ordinary contextual let call before binding arrow", "module m;\nfn run() { let(f) <- x }", [
  ["let(f)", "let", functionName], ["<-", "<-", "keyword.operator.user.kio"],
]);
fixture("reserved names keep ordinary roles", "module m;\nfn run(value: __Type__) { (__term__, __call__()) }", [
  ["__term__,", "__term__", identifier], ["__call__()", "__call__", functionName],
]);
fixture("qualified members are not placeholder stems", "module m;\nfn run() { provider.item.call(value) }", [
  ["provider.item", "provider", identifier], [".item.", "item", identifier], [".call(", "call", identifier],
]);
fixture("bang references follow identifier word grammar", "module m;\nfn run() { (__helper2_name__!(), helper2_name!(), bad1word!()) }", [
  ["__helper2_name__!", "__helper2_name__!", "entity.name.function.macro.elaborator.kio"],
  [" helper2_name!", "helper2_name!", "entity.name.function.macro.elaborator.kio"],
  ["bad1word!", "bad1word", identifier],
]);
fixture("forall argument compact unit body", "module m;\nfn run() -> . { consume([A]., value) }", [
  ["[A]", "[", "punctuation.bracket.kio"], ["[A]", "]", "punctuation.bracket.kio"],
  ["].,", ".", "punctuation.separator.kio"],
]);
for (const head of ["[_A]", "[,A]", "[ // binder\n A]", "[\n A]", "[* F]", "[ * F]", "[*F,]", "[* F,,G,]", "[ * // kind\n F]"]) {
  fixture(`forall binder domain ${head}`, `module m;\nfn run() -> . { consume(${head}., value) }`, [
    [head, "[", "punctuation.bracket.kio"], [head, "]", "punctuation.bracket.kio"],
    [`${head}.`, ".", "punctuation.separator.kio"],
  ]);
  fixture(`lambda binder domain ${head}`, `module m;\nfn run() -> . { .${head}(x: .) { let item = x; item } }`, [
    [head, "[", "punctuation.bracket.kio"], [head, "]", "punctuation.bracket.kio"], ["let item", "let", keyword],
  ]);
}
const starPrefix = "module m;\nvarop [* *] { foldl join empty; };\nfn f() { consume([* // shared";
fixture("star argument later-line forall", `${starPrefix}\n F] F(.), value) }`, [
  ["consume([* // shared", "[*", null], [" F] F", "]", "punctuation.bracket.kio"],
]);
fixture("star argument later-line varop", `${starPrefix}\n F.make() *], value) }`, [
  ["consume([* // shared", "[*", null], ["*], value", "*]", "keyword.operator.user.kio"],
]);
for (const [partial, binderTail, valueTail] of [
  [" F", " ] F(.)", " .make() *]"],
  ["F // shared", " ] F(.)", " .make() *]"],
  [" _F // shared", " ] _F(.)", " .make() *]"],
  [" F, G // shared", " ] F(.)", " .make() *]"],
  [" F, // shared", " G] F(.)", " G.make() *]"],
  [" F, , G // shared", " ] F(.)", " .make() *]"],
  [" F, * G // shared", " ] F(.)", " .make() *]"],
  [" F, ** // shared", " G] F(.)", " G.make() *]"],
]) {
  const prefix = `module m;\nvarop [* *] { foldl join empty; };\nop * _ { impl star; };\nop ** _ { impl double_star; };\nfn f() { consume([*${partial}`;
  fixture(`star partial binder later-line forall ${partial}`, `${prefix}\n${binderTail}, value) }`, [
    [`consume([*${partial}`, "[*", null], [`${binderTail}, value`, "]", "punctuation.bracket.kio"],
  ]);
  fixture(`star partial binder later-line varop ${partial}`, `${prefix}\n${valueTail}, value) }`, [
    [`consume([*${partial}`, "[*", null], [`${valueTail}, value`, "*]", "keyword.operator.user.kio"],
  ]);
}
fixture("star binder spaced commas resolve on the same line", "module m;\nfn f() { consume([* F, , * G, ]., value) }", [
  ["consume([*", "[", "punctuation.bracket.kio"], ["G, ].", "]", "punctuation.bracket.kio"],
]);
fixture("star literal written callable resolves before later line", "module m;\nvarop [* *] { foldl join empty; };\nfn f() { consume([* F.make() // value\n *], value) }", [
  ["consume([*", "[*", "keyword.operator.user.kio"], ["*], value", "*]", "keyword.operator.user.kio"],
]);
for (const name of ["value", "value1", "_value", "_1foo"]) {
  for (const prefix of [name, `F, ${name}`]) {
    fixture(`star value name fixes literal owner ${prefix}`, `module m;\nvarop [* *] { foldl join empty; };\nfn f(value: .) { consume([* ${prefix} // value\n *], value) }`, [
      ["consume([*", "[*", "keyword.operator.user.kio"], ["*], value", "*]", "keyword.operator.user.kio"],
      [`${prefix} // value`, name, identifier],
    ]);
  }
}
for (const names of ["value", "F, value"]) {
  fixture(`rejected value shaped star binder ${names}`, `module m;\nfn f() { consume([* ${names}]., value) }`, [
    ["consume([*", "[*", "keyword.operator.user.kio"], ["]., value", "].", "keyword.operator.user.kio"],
  ]);
}
fixture("star binder lambda keeps its unambiguous owner", "module m;\nfn f() { .[* // kind\n F](x: .) { let item = x; item } }", [
  [".[*", "[", "punctuation.bracket.kio"], ["let item", "let", keyword],
]);
fixture("star literal with uppercase callable is not a binder", "module m;\nvarop [* *] { foldl join empty; };\nfn f() { consume([* F.make() *], value) }", [
  ["consume([* F", "[*", "keyword.operator.user.kio"], ["*], value", "*]", "keyword.operator.user.kio"],
]);
fixture("signature version tag ownership", "signature app v(2);\nv(1) { with { module app { pub fn v() -> .; } } }\nv(2) { nonbreaking { remove { app.v; } } }", [
  ["app v(2)", "v", keyword], ["v(1)", "v", keyword], ["\nv(2)", "v", keyword],
  ["fn v()", "v", functionName], ["app.v;", "v", identifier],
]);
fixture("signature mixed tail has no new document owner", "signature app v(1);\nv(1) { with { module app { pub fn run() -> .; } } }\ndependency lib;\nsource { path \"lib\"; }\nlock lib;\nresolved { ref \"v\"; }\nv(2)", [
  ["fn run", "fn", keyword], ["dependency lib", "dependency", identifier], ["source {", "source", identifier],
  ["lock lib", "lock", identifier], ["resolved {", "resolved", identifier], ["v(2)", "v", identifier],
]);
fixture("signature name is not the version tag", "signature v(1);", [["v(1)", "v", identifier]]);
fixture("signature path leaf is not the version tag", "signature app/v(1);", [["/v(1)", "v", identifier]]);
fixture("signature multiline path and version", "signature // header\n app/ // path\n v // name\n v // version\n (1);\nv(1) {}", [
  ["v // name", "v", identifier], ["v // version", "v", keyword], ["v(1) {}", "v", keyword],
]);
fixture("raw expression does not own later declaration words", "if else match do\nfn newtype rec with", [["fn newtype", "fn", identifier], ["rec with", "rec", identifier]]);
fixture("headerless declaration retains owned fn", "fn run() -> . { () }", [["fn run", "fn", keyword]]);
fixture("raw bracket runs retain lexical operator role", "] [! ][*\n.[!/+ /\n[ ]", [
  ["] [!", "]", "keyword.operator.user.kio"], ["] [!", "[!", "keyword.operator.user.kio"],
  ["][*", "][*", "keyword.operator.user.kio"], [".[!/+", ".[!/+", "keyword.operator.user.kio"],
  ["[ ]", "[", "keyword.operator.user.kio"], ["[ ]", "]", "keyword.operator.user.kio"],
]);
fixture("module varop head begins on later line", "module m;\nvarop // declaration\n[-\n-]\n{ foldr join empty; finalize finish; };", [
  ["varop //", "varop", keyword], ["foldr join", "foldr", keyword], ["finalize finish", "finalize", keyword],
]);
fixture("multiline varop callable remains an expression", "module m;\nfn run() -> . { varop\n (()) }", [["varop\n", "varop", identifier]]);
for (const comment of ["", " // trivia"]) {
  fixture(`headerless varop later-line declaration${comment}`, `varop${comment}\n[* *] { foldl join empty; };\nfn next() -> . { () }`, [
    [`varop${comment}\n`, "varop", null], ["foldl join", "foldl", keyword], ["fn next", "fn", keyword],
  ]);
  fixture(`headerless varop later-line call${comment}`, `varop${comment}\n(())`, [[`varop${comment}\n`, "varop", null]]);
}
fixture("headerless varop same-line call", "varop(())", [["varop(())", "varop", functionName]]);
fixture("headerless incomplete varop sibling recovery", "varop\ntype Next = .;", [
  ["varop\n", "varop", null], ["type Next", "type", keyword],
]);
fixture("module varop commits before malformed parenthesized head", "module m;\nvarop(())\nfn after() -> . { () }", [
  ["varop(())", "varop", keyword], ["fn after", "fn", keyword],
]);
fixture("missing varop head does not own fold body", "module m;\nvarop { foldl join empty; };", [
  ["varop {", "varop", keyword], ["foldl join", "foldl", null],
]);
for (const prefix of ["", "module m;\n"]) {
  fixture(`incomplete import retains sibling declaration ${prefix.length}`, `${prefix}import app\nfn neighbor() -> . { () }`, [["fn neighbor", "fn", keyword]]);
}

const unorderedFixtures = JSON.parse(fs.readFileSync(new URL("../../../tools/tree-sitter-kio/test/structural.json", import.meta.url), "utf8"));
fixture("dependency source and mapping keyword owners", 'dependency dep;\nsource { ref "main"; git "repo"; }\nrehost old to fresh;\nretype old.T to fresh.T;', [
  ['ref "main"', "ref", keyword], ['git "repo"', "git", keyword],
  ["old to fresh;", "to", keyword], ["old.T to fresh.T;", "to", keyword],
]);
fixture("dependency mapping ordinary names", 'dependency rehost; source { path "lib"; } rehost to to source; retype retype.T to source.T;', [
  ["dependency rehost", "rehost", identifier], ['path "lib"', "path", keyword],
  ["rehost to", "to", identifier], [" to source;", "to", keyword], ["to source;", "source", identifier],
  ["retype retype", "retype", keyword], ["retype.T", "retype", identifier],
  ["T to source.T", "to", keyword], ["source.T", "source", identifier],
]);
fixture("dependency multiline mapping paths", 'dependency dep; source { path "lib"; }\nrehost // head\n old / // path\n to\n to // separator\n fresh;\nretype old // path\n . // type\n T\n to fresh.T;', [
  ["rehost //", "rehost", keyword], ["\n to\n", "to", identifier], ["to // separator", "to", keyword],
  ["retype old", "retype", keyword], ["to fresh.T", "to", keyword],
]);
fixture("dependency words in ordinary module body", "module m; fn rehost(retype: ., source: .) { rehost(retype); source }", [
  ["fn rehost", "rehost", functionName], ["retype:", "retype", identifier], ["source:", "source", identifier],
  ["rehost(retype)", "rehost", functionName], ["; source", "source", identifier],
]);
fixture("dependency bad mapping value does not own separator", 'dependency dep; source { path "lib"; } rehost Upper to fresh;', [["to fresh", "to", identifier]]);
for (const path of ["__private__", "_1", "old / Upper", "old / __private__", "old.T"]) {
  fixture(`dependency invalid module path ${path}`, `dependency dep; source { path "lib"; } rehost ${path} to fresh;`, [["to fresh", "to", identifier]]);
}
fixture("dependency bad retype suffix does not own separator", 'dependency dep; source { path "lib"; } retype old.lower to fresh.T;', [["to fresh", "to", identifier]]);
fixture("dependency field value is not a new field head", 'dependency dep; source { path git; }', [["path git", "path", keyword], ["git;", "git", identifier]]);
for (const header of ["Upper", "_1", "__private__", "dep extra", "dep()", "dep / extra", "\n Upper", "dep\n extra"]) {
  fixture(`dependency rejected header ${header}`, `dependency ${header}; source { path "lib"; } rehost old to fresh;`, [
    ["source {", "source", identifier], ['path "lib"', "path", identifier],
    ["rehost old", "rehost", identifier], ["to fresh", "to", identifier],
  ]);
}
fixture("dependency keyword name and header trivia", 'dependency source // header\n ; source { path "lib"; } rehost old to fresh;', [
  ["dependency source", "source", identifier], ["; source", "source", keyword],
  ['path "lib"', "path", keyword], ["rehost old", "rehost", keyword], ["to fresh", "to", keyword],
]);
fixture("dependency module-wide retype", 'dependency dep; source { path "lib"; } retype old to fresh;', [["retype old", "retype", keyword], ["to fresh", "to", keyword]]);
fixture("dependency ordinary raw call", "dependency(value)", [["dependency(value)", "dependency", functionName]]);
for (const trivia of ["", " // shared"]) {
  const prefix = `dependency${trivia}`;
  fixture(`dependency later-line document${trivia}`, `${prefix}\n dep; source { path "lib"; } rehost old to fresh;`, [
    [prefix, "dependency", null], ["rehost old", "rehost", keyword],
  ]);
  fixture(`dependency later-line call${trivia}`, `${prefix}\n (value)`, [[prefix, "dependency", null]]);
}
for (const [owner, count] of [
  ["varop", 8], ["elab", 2], ["package", 2], ["build", 7], ["dependency", 6],
  ["signature_sections", 6], ["signature_buckets", 6],
]) {
  const cases = unorderedFixtures.filter((item) => item.name.startsWith(`order_${owner}_`) && !item.error);
  assert.equal(cases.length, count, `${owner}: all shared positive order cases execute`);
  for (const item of cases) {
    assert.ok(item.targets.length > 0, `${item.name}: nonempty order targets`);
    fixture(`unordered ${item.name}`, item.source, item.targets.map((target) => {
      assert.equal(target.kind, "keyword.declaration", `${item.name}: owned clause target`);
      assert.equal(item.source.slice(target.start, target.end), target.text, `${item.name}: shared target span`);
      const needle = item.source.slice(Math.max(0, target.start - 1), target.end + 1);
      return [needle, target.text, keyword];
    }));
  }
}

export function checkTextMateHighlighting(grammar, observe, fixtureNames) {
  const selected = fixtureNames ? fixtures.filter((fixture) => fixtureNames.includes(fixture.name)) : fixtures;
  if (fixtureNames) assert.equal(selected.length, fixtureNames.length, "every selected fixture exists");
  const failures = [];
  const ambiguous = [];
  let targets = 0;
  for (const fixture of selected) {
    const visited = new Set();
    let state = vsctm.INITIAL;
    let offset = 0;
    for (const line of fixture.source.split("\n")) {
      const priorState = state.toString();
      const result = grammar.tokenizeLine(line, state);
      assert.ok(!result.stoppedEarly, fixture.name);
      for (const target of fixture.targets.filter((target) => target.start >= offset && target.start < offset + line.length)) {
        assert.ok(!visited.has(target), `${fixture.name}: target executes once`);
        visited.add(target);
        targets++;
        const actual = result.tokens.filter((token) => token.startIndex + offset < target.end && target.start < token.endIndex + offset)
          .map((token) => ({ start: token.startIndex + offset, end: token.endIndex + offset, scopes: token.scopes }));
        const passed = actual.length === 1 && actual[0].start === target.start && actual[0].end === target.end &&
          (target.scope === null ? actual[0].scopes.every((scope) => !positive.test(scope)) :
            actual[0].scopes.filter((scope) => positive.test(scope)).every((scope) => scope === target.scope) && actual[0].scopes.includes(target.scope));
        const row = { fixture: fixture.name, source: fixture.source, target, actual, passed, priorState, finalState: result.ruleStack.toString() };
        observe?.(row);
        if (!passed) failures.push(row);
        if (target.scope === null && fixture.name.includes("later-line")) ambiguous.push({ prefix: fixture.source.slice(0, offset + line.length), line, priorState, finalState: row.finalState, actual });
      }
      state = result.ruleStack;
      offset += line.length + 1;
    }
    assert.equal(visited.size, fixture.targets.length, `${fixture.name}: every authored target executes`);
  }
  assert.equal(targets, selected.reduce((count, fixture) => count + fixture.targets.length, 0), "all selected targets execute");
  assert.equal(ambiguous.length % 2, 0, "paired ambiguous continuations");
  for (let index = 0; index < ambiguous.length; index += 2) {
    assert.deepEqual(ambiguous[index], ambiguous[index + 1], `same-prefix continuations ${index / 2}`);
  }
  console.log(`textmate-highlighting: ${selected.length} fixtures, ${targets} exact targets, ${failures.length} failures`);
  assert.deepEqual(failures, [], JSON.stringify(failures, null, 2));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const require = createRequire(import.meta.url);
  await oniguruma.loadWASM(fs.readFileSync(require.resolve("vscode-oniguruma/release/onig.wasm")));
  const grammarPath = process.argv[2] ?? fileURLToPath(new URL("../../../tools/textmate-kio/kio.tmLanguage.json", import.meta.url));
  const registry = new vsctm.Registry({
    onigLib: Promise.resolve({ createOnigScanner: (patterns) => new oniguruma.OnigScanner(patterns), createOnigString: (source) => new oniguruma.OnigString(source) }),
    loadGrammar: async () => vsctm.parseRawGrammar(fs.readFileSync(grammarPath, "utf8"), grammarPath),
  });
  try {
    const grammar = await registry.loadGrammar("source.kio");
    assert.ok(grammar);
    checkTextMateHighlighting(grammar, process.env.KIO_DEBUG_TEXTMATE_ACTUALS ? (row) => console.log(JSON.stringify(row)) : undefined);
  } finally {
    registry.dispose();
  }
}
