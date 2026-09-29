#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
const LOGICAL = "kio-rs/src/normalization.rs";
const REMAPPED = "src/normalization.rs";
const PARITY = "00_success/repl_normalize_discharges_elaborators";
const SAFE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const bad = (message) => { throw new Error(message); };
const need = (condition, message) => { if (!condition) bad(message); };
const text = (file) => fs.readFileSync(file, "utf8");
const write = (file, value) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, value);
};
const json = (file, value) => write(file, `${JSON.stringify(value, null, 2)}\n`);
const integer = (value, label, positive = false) => {
  need(Number.isSafeInteger(value) && value >= (positive ? 1 : 0), `${label} is not a safe ${positive ? "positive" : "nonnegative"} integer`);
  return value;
};
const regular = (file, label) => {
  const stat = fs.lstatSync(file);
  need(stat.isFile() && !stat.isSymbolicLink(), `${label} is not a regular file: ${file}`);
  return stat;
};
const directory = (dir, label) => {
  const stat = fs.lstatSync(dir);
  need(stat.isDirectory() && !stat.isSymbolicLink(), `${label} is not a directory: ${dir}`);
};
const dirNames = (dir, label) => {
  directory(dir, label);
  return fs.readdirSync(dir, { withFileTypes: true }).map((entry) => {
    need(!entry.isSymbolicLink(), `${label} contains a symlink: ${entry.name}`);
    return entry;
  }).filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort();
};

function containsEquiv(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.isSymbolicLink() || entry.name === "out" || entry.name === ".kio-cache") continue;
    const file = path.join(dir, entry.name);
    if (entry.isDirectory() && containsEquiv(file)) return true;
    if (entry.isFile() && entry.name.endsWith(".kio")) {
      const source = text(file).replace(/\/\/.*$/gm, "");
      if (/^[ \t]*equiv(?:[^A-Za-z0-9_]|$)/m.test(source)) return true;
    }
  }
  return false;
}

function caseFile(root, relative) {
  const parts = relative.split("/");
  need(parts.length === 2 && parts.every((part) => SAFE.test(part)), `unsafe golden path: ${relative}`);
  const dir = path.join(root, ...parts);
  directory(dir, `golden ${relative}`);
  regular(path.join(dir, "expected.exit"), `${relative}/expected.exit`);
  return dir;
}

function inventory([goldens, manifestFile, pocs, out]) {
  need(out, "inventory requires GOLDENS MANIFEST POCS OUT");
  const discovered = [];
  for (const bucket of dirNames(goldens, "golden corpus")) {
    need(SAFE.test(bucket), `unsafe golden bucket: ${bucket}`);
    for (const name of dirNames(path.join(goldens, bucket), `golden bucket ${bucket}`)) {
      need(SAFE.test(name), `unsafe golden case: ${bucket}/${name}`);
      const relative = `${bucket}/${name}`;
      const dir = path.join(goldens, bucket, name);
      if (name.includes("equiv") || containsEquiv(dir) || relative === PARITY) {
        caseFile(goldens, relative);
        discovered.push(relative);
      }
    }
  }
  discovered.sort();
  regular(manifestFile, "golden inventory");
  const selected = text(manifestFile).replace(/\r/g, "").trimEnd().split("\n");
  need(selected.length && selected[0], "golden inventory is empty");
  selected.forEach((entry) => caseFile(goldens, entry));
  need(new Set(selected).size === selected.length, "golden inventory contains duplicates");
  need(selected.join("\n") === [...selected].sort().join("\n"), "golden inventory is not sorted");
  need(selected.join("\n") === discovered.join("\n"), "golden inventory differs from fresh discovery");
  const pocNames = dirNames(pocs, "POC corpus");
  need(pocNames.length > 0, "POC inventory is empty");
  for (const name of pocNames) {
    need(SAFE.test(name), `unsafe POC name: ${name}`);
    const dir = path.join(pocs, name);
    need(fs.readdirSync(dir).some((entry) => entry !== "expected.exit"), `POC is empty: ${name}`);
    const exitFile = path.join(dir, "expected.exit");
    regular(exitFile, `${name}/expected.exit`);
    need(text(exitFile).replace(/\s/g, "") === "0", `POC expected.exit is not 0: ${name}`);
  }
  const selectors = (entries) => entries.map((entry) => `^${entry.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`).join("\n") + "\n";
  write(path.join(out, "discovered-goldens.txt"), discovered.join("\n") + "\n");
  write(path.join(out, "selected-goldens.txt"), selected.join("\n") + "\n");
  write(path.join(out, "selected-pocs.txt"), pocNames.join("\n") + "\n");
  write(path.join(out, "golden-selectors.txt"), selectors(selected));
  write(path.join(out, "poc-selectors.txt"), selectors(pocNames));
  process.stdout.write(`${selected.length} ${pocNames.length}\n`);
}

function summary([file, expectedText, label]) {
  const expected = Number(expectedText);
  integer(expected, "expected summary count", true);
  const log = text(file).replace(/\r/g, "");
  const rows = log.split("\n").filter((line) => line.startsWith("  kio@js:"));
  need(rows.length === 1, `${label}: expected one kio@js summary, got ${rows.length}`);
  const match = /^  kio@js: ([0-9]+) passed, ([0-9]+) failed(?:, ([0-9]+) known-failing)?$/.exec(rows[0]);
  need(match, `${label}: malformed kio@js summary`);
  need(Number(match[1]) === expected && match[2] === "0" && !match[3] && !/^warn \[/m.test(log),
    `${label}: expected ${expected} passed, 0 failed, 0 warnings`);
}

function profiles([dir, phase]) {
  directory(dir, "coverage target");
  const found = { unit: 0, golden: 0, poc: 0 };
  for (const name of fs.readdirSync(dir).filter((entry) => entry.endsWith(".profraw"))) {
    const match = /^(unit|golden|poc)-.+\.profraw$/.exec(name);
    need(match, `stray raw profile: ${name}`);
    need(regular(path.join(dir, name), `raw profile ${name}`).size > 0, `empty raw profile: ${name}`);
    found[match[1]] += 1;
  }
  if (phase === "empty") need(Object.values(found).every((count) => count === 0), "coverage clean left raw profiles");
  else {
    need(phase === "complete", `unknown profile phase: ${phase}`);
    for (const [prefix, count] of Object.entries(found)) need(count > 0, `missing ${prefix} raw profile`);
    process.stdout.write(Object.entries(found).map(([name, count]) => `${name} ${count}`).join("\n") + "\n");
  }
}

function normalize(input) {
  need(typeof input === "string" && input && !input.includes("\0"), "empty coverage path");
  let value = input.replace(/\\/g, "/");
  let prefix = value.startsWith("//") ? "//" : value.startsWith("/") ? "/" : "";
  if (/^[A-Za-z]:\//.test(value)) { prefix = `${value[0].toLowerCase()}:/`; value = value.slice(3); }
  else if (prefix === "//") value = value.slice(2);
  else if (prefix) value = value.slice(1);
  const parts = value.split("/").filter((part) => part && part !== ".");
  need(parts.length && !parts.includes(".."), `invalid coverage path: ${input}`);
  return prefix + parts.join("/");
}

function sourceInfo(source) {
  const lines = source.replace(/\r/g, "").split("\n");
  const fnPattern = /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe)\s+)*(?:extern(?:\s+"[^"]*")?\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\b/;
  const markers = [];
  for (let i = 0; i + 1 < lines.length; i++) if (lines[i] === "#[cfg(test)]" && lines[i + 1] === "mod tests {") markers.push(i + 1);
  const last = lines.findLastIndex((line) => line.trim());
  need(markers.length === 1 && lines[last] === "}" && lines.slice(markers[0] + 1, last).every((line) => !line.trim() || /^\s/.test(line)), "missing or non-terminal #[cfg(test)] mod tests");
  const cutoff = markers[0], exclusions = [], declarations = new Map();
  const functionEnd = (start) => {
    let depth = 0, opened = false;
    for (let i = start - 1; i < cutoff - 1; i++) {
      const code = lines[i].replace(/"(?:\\.|[^"\\])*"/g, (match) => " ".repeat(match.length)).replace(/\/\/.*$/, "");
      for (let column = 0; column < code.length; column++) {
        const char = code[column]; if (char === "{") { depth++; opened = true; }
        else if (char === "}") { depth--; if (opened && depth === 0) return { endLine: i + 1, endColumn: column + 2 }; }
      }
    }
    bad(`cannot bound source function at line ${start}`);
  };
  const cfgConstructEnd = (start) => {
    let depth = 0, opened = false;
    for (let i = start - 1; i < cutoff - 1; i++) {
      const code = lines[i].replace(/"(?:\\.|[^"\\])*"/g, (match) => " ".repeat(match.length)).replace(/\/\/.*$/, "");
      for (let column = 0; column < code.length; column++) {
        const char = code[column];
        if (char === "{") { depth++; opened = true; }
        else if (char === "}") { depth--; if (opened && depth === 0) return i + 1; }
      }
      if (!opened && /[;,]\s*$/.test(code)) return i + 1;
    }
    bad(`cannot bound cfg(test) construct at line ${start}`);
  };
  lines.slice(0, cutoff - 1).forEach((line, index) => {
    const match = fnPattern.exec(line); if (!match) return;
    let brace = index; while (brace < cutoff && !lines[brace].includes("{")) brace++;
    for (let signature = index; signature <= brace; signature++) declarations.set(signature + 1, { name: match[1], startLine: index + 1, ...functionEnd(index + 1) });
  });
  let replCfg = 0;
  for (let i = 0; i < cutoff - 1; i++) {
    const line = lines[i].trim();
    if (!/^#\[cfg\(.*\btest\b/.test(line)) continue;
    if (line === '#[cfg(any(feature = "repl-core", test))]') { replCfg++; continue; }
    const all = /^#\[cfg\(all\((.*)\)\)\]$/.exec(line);
    const atoms = all?.[1].split(",").map((atom) => atom.trim());
    const testOnly = line === "#[cfg(test)]" || atoms?.includes("test") && atoms.every((atom) =>
      /^[A-Za-z_][A-Za-z0-9_]*(?:\s*=\s*"[^",]*")?$/.test(atom));
    need(testOnly, `unrecognized pre-terminal test cfg at line ${i + 1}`);
    let next = i + 1; while (next < cutoff - 1 && !lines[next].trim()) next++;
    const end = fnPattern.test(lines[next]) ? functionEnd(next + 1).endLine : cfgConstructEnd(next + 1);
    need(end < cutoff, `cannot bound cfg(test) construct at line ${i + 1}`);
    exclusions.push({ startLine: i + 1, endLine: end });
  }
  exclusions.push({ startLine: cutoff, endLine: lines.length });
  return { lines, cutoff, exclusions, declarations, replCfg };
}

const excluded = (line, info) => info.exclusions.some((range) => line >= range.startLine && line <= range.endLine);
function acceptedPath(filename, aliases, label) {
  need(typeof filename === "string" && filename && !filename.includes("\0"), `${label}: invalid coverage path`);
  if (filename.replace(/\\/g, "/").split("/").at(-1) !== "normalization.rs") return false;
  const value = normalize(filename);
  if (aliases.has(value)) return true;
  need(!value.endsWith("/normalization.rs") && value !== "normalization.rs", `${label}: ambiguous target-like path ${filename}`);
  return false;
}

function lcovLines(lcov, aliases, info) {
  need(lcov.trim().endsWith("end_of_record"), "malformed or empty LCOV export");
  let targets = 0; const hits = new Map();
  for (const block of lcov.split("end_of_record")) {
    if (!block.trim()) continue;
    const rows = block.replace(/\r/g, "").trim().split("\n");
    const sf = rows.filter((line) => line.startsWith("SF:"));
    need(sf.length === 1, "LCOV record does not have exactly one SF entry");
    if (!acceptedPath(sf[0].slice(3), aliases, "LCOV")) continue;
    targets++;
    for (const row of rows.filter((line) => line.startsWith("DA:"))) {
      const match = /^DA:([0-9]+),([0-9]+)(?:,.*)?$/.exec(row);
      need(match, `malformed LCOV line record: ${row}`);
      const line = Number(match[1]), count = Number(match[2]); integer(line, "LCOV line", true); integer(count, "LCOV count");
      need(line <= info.lines.length, `LCOV line ${line} exceeds source length`);
      if (!excluded(line, info)) hits.set(line, (hits.get(line) || false) || count > 0);
    }
  }
  need(targets > 0 && hits.size > 0, `expected a nonempty target LCOV record, got ${targets}`);
  const ranges = [];
  for (const line of [...hits].filter(([, hit]) => !hit).map(([line]) => line).sort((a, b) => a - b)) {
    const last = ranges.at(-1);
    if (last && line === last.endLine + 1) { last.endLine = line; last.lineCount++; }
    else ranges.push({ startLine: line, endLine: line, lineCount: 1 });
  }
  return ranges;
}

function jsonFunctions(raw, aliases, info) {
  need(raw?.type === "llvm.coverage.json.export" && typeof raw.version === "string" && /^(?:2|3)\.[0-9]+\.[0-9]+$/.test(raw.version) && Array.isArray(raw.data) && raw.data.length, "empty or unsupported LLVM JSON export");
  const groups = new Map(), mappedSpans = [], unmapped = [];
  for (const [di, data] of raw.data.entries()) {
    need(Array.isArray(data?.functions), `data[${di}].functions is missing`);
    for (const [fi, fn] of data.functions.entries()) {
      need(typeof fn?.name === "string" && Array.isArray(fn.filenames) && Array.isArray(fn.regions), `malformed function data[${di}][${fi}]`);
      const ids = new Set();
      fn.filenames.forEach((name, id) => { if (acceptedPath(name, aliases, `function ${fn.name}`)) ids.add(id); });
      if (!ids.size) continue;
      const targetCode = [];
      for (const [ri, tuple] of fn.regions.entries()) {
        need(Array.isArray(tuple) && tuple.length === 8, `function ${fn.name} region ${ri} is malformed (${JSON.stringify(tuple)})`);
        tuple.forEach((value, index) => integer(value, `function ${fn.name} region ${ri}[${index}]`));
        const [sl, sc, el, ec, count, fileId, expanded, kind] = tuple;
        need(fileId < fn.filenames.length && expanded < fn.filenames.length && sl > 0 && sc > 0 && el > 0 && ec > 0 && (sl < el || sl === el && sc <= ec), `function ${fn.name} region ${ri} has invalid bounds`);
        if (!ids.has(fileId)) continue;
        need(el <= info.lines.length && sc <= Buffer.byteLength(info.lines[sl - 1]) + 1 && ec <= Buffer.byteLength(info.lines[el - 1]) + 1, `function ${fn.name} region ${ri} exceeds target source bounds`);
        if (kind === 0) targetCode.push({ sl, sc, el, ec, count });
      }
      if (!targetCode.length) continue;
      const code = targetCode.filter(({ sl, el }) => !info.exclusions.some((range) => sl >= range.startLine && el <= range.endLine));
      if (!code.length) continue;
      const roots = code.filter((region) => info.declarations.has(region.sl)).sort((a, b) => a.sl - b.sl || a.sc - b.sc || b.el - a.el || b.ec - a.ec);
      if (!roots.length) { unmapped.push({ name: fn.name, code }); continue; }
      const root = roots[0], declaration = info.declarations.get(root.sl), key = `${declaration.startLine}:${declaration.name}`;
      need(code.every((region) => region.sl > root.sl || region.sl === root.sl && region.sc >= root.sc), `mapped function ${fn.name} has code before its declaration region`);
      need(code.every((region) => region.el < declaration.endLine || region.el === declaration.endLine && region.ec <= declaration.endColumn), `mapped function ${fn.name} has code beyond its source function`);
      const end = code.reduce((best, region) => region.el > best.el || region.el === best.el && region.ec > best.ec ? region : best, root);
      const span = { ...root, el: end.el, ec: end.ec };
      const group = groups.get(key) || { name: declaration.name, startLine: declaration.startLine, endLine: root.el, llvmRecords: 0, hit: false };
      mappedSpans.push(span);
      group.endLine = Math.max(group.endLine, span.el); group.llvmRecords++; group.hit ||= code.some((region) => region.count > 0); groups.set(key, group);
    }
  }
  need(groups.size > 0, "no link-present production function mapped to the target");
  const contains = (outer, inner) => (outer.sl < inner.sl || outer.sl === inner.sl && outer.sc < inner.sc)
    && (inner.el < outer.el || inner.el === outer.el && inner.ec < outer.ec);
  for (const record of unmapped) need(mappedSpans.some((outer) => record.code.every((inner) => contains(outer, inner))),
    `unmapped target function ${record.name} is not strictly nested in a mapped production function`);
  return [...groups.values()].sort((a, b) => a.startLine - b.startLine);
}

function analyzeValues(lcov, raw, source, aliases) {
  const info = sourceInfo(source), functions = jsonFunctions(raw, aliases, info);
  const zeroFunctions = functions.filter((fn) => !fn.hit).map(({ hit, ...fn }) => fn);
  const zeroRanges = lcovLines(lcov, aliases, info).map((range) => ({
    ...range,
    functions: functions.filter((fn) => fn.startLine <= range.endLine && fn.endLine >= range.startLine).map((fn) => fn.name),
  }));
  const displayRanges = zeroRanges.filter((range) => range.lineCount >= 5 && !zeroFunctions.some((fn) => fn.startLine <= range.startLine && fn.endLine >= range.endLine));
  return { schemaVersion: 1, target: LOGICAL, cohorts: ["all-feature-lib-unit", "focused-equiv-goldens", "all-pocs"], claim: "informational union reachability locator; not a confidence gate", retainedReplCoreCfgSites: info.replCfg, zeroExecutedFunctions: zeroFunctions, zeroExecutableLineRanges: zeroRanges, displayedLargeRanges: displayRanges };
}

function analyze([lcovFile, jsonFile, sourceFile, nativeSource, out]) {
  need(out, "analyze requires LCOV JSON SOURCE NATIVE_SOURCE OUT");
  const aliases = new Set([sourceFile, nativeSource, LOGICAL, REMAPPED].map(normalize));
  const result = analyzeValues(text(lcovFile), JSON.parse(text(jsonFile)), text(sourceFile), aliases);
  json(out, result);
  process.stdout.write("equiv-eval production reachability locator (not a confidence gate)\n");
  process.stdout.write(`functions with no executed code region: ${result.zeroExecutedFunctions.length}\n`);
  result.zeroExecutedFunctions.forEach((fn) => process.stdout.write(`  ${fn.name} (${fn.startLine}-${fn.endLine})\n`));
  process.stdout.write(`zero-hit executable source-line ranges: ${result.zeroExecutableLineRanges.length}; showing ${result.displayedLargeRanges.length} non-whole-function ranges of at least 5 lines\n`);
  result.displayedLargeRanges.forEach((range) => process.stdout.write(`  ${range.startLine}-${range.endLine} [${range.functions.join(", ") || "module"}]\n`));
}

function selfTest([productionSource]) {
  const source = ["fn hit() {", "  work();", "}", "fn cold() {", "  a();", "  b();", "  c();", "  d();", "  e();", "} // padding", "#[cfg(test)]", "fn helper() {", "  test();", "}", '#[cfg(any(feature = "repl-core", test))]', "fn repl() {", "  work();", "}", "#[cfg(test)]", "mod tests {", "  fn ignored() {}", "}", ""].join("\n");
  const sf = "/repo/kio-rs/src/normalization.rs", aliases = new Set([normalize(sf), LOGICAL, REMAPPED]);
  const make = (cold) => ({ type: "llvm.coverage.json.export", version: "2.0.1", data: [{ functions: [
    { name: "crate::hit", filenames: [sf], regions: [[1,1,3,2,1,0,0,0]] },
    { name: "crate::cold", filenames: [sf], regions: [[4,1,10,2,cold,0,0,0]] },
    { name: "_RNCNvCs1234_3kio13normalization4cold0B3_", filenames: [sf], regions: [[5,3,9,4,cold,0,0,0]] },
    { name: "crate::helper", filenames: [sf], regions: [[12,1,14,2,0,0,0,0]] },
    { name: "crate::repl", filenames: [sf], regions: [[16,1,18,2,1,0,0,0]] },
  ] }] });
  const lcov = (cold) => `TN:\nSF:${sf}\nDA:1,1\nDA:2,1\nDA:3,1\n${[4,5,6,7,8,9,10].map((line) => `DA:${line},${cold}`).join("\n")}\nDA:12,0\nDA:13,0\nDA:14,0\nDA:16,1\nDA:17,1\nDA:18,1\nDA:21,0\nend_of_record\n`;
  const uncovered = analyzeValues(lcov(0), make(0), source, aliases);
  need(uncovered.zeroExecutedFunctions.length === 1 && uncovered.zeroExecutedFunctions[0].name === "cold" && uncovered.zeroExecutableLineRanges[0].lineCount === 7 && uncovered.retainedReplCoreCfgSites === 1, "uncovered/test-exclusion fixture failed");
  const clear = analyzeValues(lcov(1), make(1), source, aliases);
  need(!clear.zeroExecutedFunctions.length && !clear.zeroExecutableLineRanges.length, "no-findings fixture failed");
  const v3 = make(0); v3.version = "3.1.0"; v3.data[0].functions[1].regions = [[4,1,4,10,0,0,0,0], [5,3,10,2,0,0,0,0]]; need(analyzeValues(lcov(0), v3, source, aliases).zeroExecutedFunctions.length === 1, "LLVM JSON v3 fixture failed");
  const at = (file) => { const raw = make(0); raw.data[0].functions.forEach((fn) => { fn.filenames = [file]; }); return raw; };
  const windows = "C:\\repo\\kio-rs\\src\\normalization.rs"; aliases.add(normalize(windows)); need(analyzeValues(lcov(0).replace(sf, windows), at(windows), source, aliases).zeroExecutedFunctions.length === 1, "Windows path fixture failed");
  need(analyzeValues(lcov(0).replace(sf, REMAPPED), at(REMAPPED), source, aliases).zeroExecutedFunctions.length === 1, "remapped path fixture failed");
  const fails = (action) => { try { action(); } catch { return true; } return false; };
  const cfgSource = ["fn live() {}", '#[cfg(all(feature = "surface", test))]', "thread_local! {", "  static ONE: usize = 1;", "  static TWO: usize = 2;", "}", "fn after_macro() {}", '#[cfg(all(test, target_os = "linux"))]', "#[derive(Clone, Copy)]", "struct Helper {", "  field: usize,", "}", "fn after_struct() {}", "#[cfg(test)]", "mod tests {", "  fn ignored() {}", "}", ""].join("\n");
  const cfgInfo = sourceInfo(cfgSource);
  need([1,7,13].every((line) => !excluded(line, cfgInfo)) && [2,3,4,5,6,8,9,10,11,12].every((line) => excluded(line, cfgInfo)), "test-only cfg conjunction exclusion failed");
  need(fails(() => sourceInfo(cfgSource.replace('all(feature = "surface", test)', 'any(feature = "surface", test)'))), "production-capable cfg(test) conjunction was excluded");
  const cfgFieldSource = ["fn live() {}", "struct Mixed {", "  #[cfg(test)]", "  test_only: usize,", "  live: usize,", "}", "#[cfg(test)]", "mod tests {", "}", ""].join("\n");
  const cfgFieldInfo = sourceInfo(cfgFieldSource);
  need(excluded(3, cfgFieldInfo) && excluded(4, cfgFieldInfo) && !excluded(5, cfgFieldInfo) && !excluded(6, cfgFieldInfo), "cfg(test) field exclusion escaped its comma terminator");
  const cfgFnSource = ["#[cfg(test)]", "fn helper(", "  left: usize,", "  right: usize,", ") -> usize {", "  left + right", "}", "fn live() {}", "#[cfg(test)]", "mod tests {", "}", ""].join("\n");
  const cfgFnInfo = sourceInfo(cfgFnSource);
  need([1,2,3,4,5,6,7].every((line) => excluded(line, cfgFnInfo)) && !excluded(8, cfgFnInfo), "cfg(test) multiline function exclusion stopped at a signature comma");
  const unknown = make(0); unknown.version = "4.0.0"; need(fails(() => analyzeValues(lcov(0), unknown, source, aliases)), "unknown LLVM JSON version did not fail");
  const coerced = make(0); coerced.version = ["3.1.0"]; need(fails(() => analyzeValues(lcov(0), coerced, source, aliases)), "non-string LLVM JSON version did not fail");
  need(fails(() => sourceInfo(`${source}fn escaped() {\n}\n`)), "top-level item after test module did not fail");
  need(fails(() => analyzeValues("", make(0), source, aliases)), "empty LCOV did not fail");
  need(fails(() => analyzeValues(lcov(0), { type: "llvm.coverage.json.export", version: "2.0.1", data: [] }, source, aliases)), "empty JSON did not fail");
  const ambiguous = make(0); ambiguous.data[0].functions[0].filenames.push("/other/kio-rs/src/normalization.rs"); need(fails(() => analyzeValues(lcov(0), ambiguous, source, aliases)), "ambiguous target did not fail");
  const unrelated = make(0); unrelated.data[0].functions[0].filenames.push("/dep/src/backends/../util_libc.rs"); need(analyzeValues(lcov(0), unrelated, source, aliases).zeroExecutedFunctions.length === 1, "unrelated traversal fixture failed");
  const traversing = make(0); traversing.data[0].functions[0].filenames.push("/repo/kio-rs/src/other/../normalization.rs"); need(fails(() => analyzeValues(lcov(0), traversing, source, aliases)), "target traversal path did not fail");
  const bridge = make(0); bridge.data[0].functions[1].regions.push([16,1,18,2,1,0,0,0]); need(fails(() => analyzeValues(lcov(0), bridge, source, aliases)), "mapped record bridging beyond its source function did not fail");
  const before = make(0); before.data[0].functions[1].regions.push([3,1,3,2,1,0,0,0]); need(fails(() => analyzeValues(lcov(0), before, source, aliases)), "mapped record before its declaration did not fail");
  const sameLine = make(0); sameLine.data[0].functions[1].regions.push([10,3,10,8,1,0,0,0]); need(fails(() => analyzeValues(lcov(0), sameLine, source, aliases)), "mapped record beyond its same-line closing brace did not fail");
  const unmapped = make(0); unmapped.data[0].functions.push({ name: "_RNCNvCs5678_3kio13normalization7escaped0B3_", filenames: [sf], regions: [[15,1,15,2,0,0,0,0]] }); need(fails(() => analyzeValues(lcov(0), unmapped, source, aliases)), "uncontained raw-v0 target function did not fail");
  if (productionSource) sourceInfo(text(productionSource));
  process.stdout.write("equiv-eval-reachability synthetic self-test: OK\n");
}

try {
  const [command, ...args] = process.argv.slice(2);
  if (command === "inventory") inventory(args);
  else if (command === "summary") summary(args);
  else if (command === "profiles") profiles(args);
  else if (command === "analyze") analyze(args);
  else if (command === "self-test") selfTest(args);
  else bad("usage: equiv-eval-reachability.mjs inventory|summary|profiles|analyze|self-test ...");
} catch (error) {
  process.stderr.write(`error: equiv-eval-reachability: ${error.message}\n`);
  process.exitCode = 1;
}
