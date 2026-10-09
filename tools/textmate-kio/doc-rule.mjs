// TextMate's physical /// container and line-local projection of shared Kiodoc.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { rules, lexicalPatterns, textmateInlinePatterns, markupPatterns } from "../kiodoc-grammar/generate.mjs";

export function docRule() {
  const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const { marker, minWidth, maxIndent, hiddenOpen, hiddenClose } = rules.fence;
  const p = lexicalPatterns();
  const markup = markupPatterns();
  const ws = p.textmateWhitespace;
  const fence = "keyword.control.fence.kiodoc.kio";
  const code = "string.unquoted.code.kiodoc.kio";
  const nonspace = `(?:(?!${ws})[^${escape(marker)}\\n])`;
  const inline = [
    ...textmateInlinePatterns(),
    ...rules.emphasis.flatMap((mark, index) => [
      {
        begin: `(?=${markup.strong[index]})${escape(mark)}${escape(mark)}`,
        end: `${escape(mark)}${escape(mark)}`,
        name: "markup.bold.kiodoc.kio",
        patterns: [{ include: "#doc-inline" }],
      },
      {
        begin: `(?=${markup.emphasis[index]})${escape(mark)}`,
        end: escape(mark),
        name: "markup.italic.kiodoc.kio",
        patterns: [{ include: "#doc-inline" }],
      },
    ]),
  ];
  const visible = (kio) => ({
    begin: `\\G {0,${maxIndent}}(${escape(marker)}{${minWidth},})(${ws}*${kio ? `${escape(markup.kioLanguage)}(?=${ws}|$)` : `${nonspace}+`})${kio ? "" : "?"}${ws}*(${nonspace}(?:[^${escape(marker)}\\n]*?${nonspace})?)?${ws}*$`,
    end: `\\G {0,${maxIndent}}\\1${escape(marker)}*${ws}*$`,
    beginCaptures: { 0: { name: fence }, 3: { name: "keyword.other.attribute.kiodoc.kio" } },
    endCaptures: { 0: { name: fence } },
    contentName: kio ? "meta.embedded.block.kio" : code,
    ...(kio ? {
      patterns: [{
        // Capture retokenization ends at the physical payload boundary. Nested
        // strings/comments cannot retain a rule stack past the owning fence.
        match: "\\G(.+)$",
        captures: { 1: { patterns: [
          { match: "///(?:[ \\t\\r].*|$)", name: "comment.line.documentation.kio" },
          { include: "#source-tokens" },
        ] } },
      }],
    } : {}),
  });
  const line = [
    { begin: `\\G${ws}*(${markup.quote})`, end: "$", beginCaptures: { 1: { name: "punctuation.definition.marker.kiodoc.kio" } }, patterns: [{ include: "#doc-line" }] },
    { match: `\\G${ws}*(${markup.thematic})${ws}*$`, captures: { 1: { name: "punctuation.definition.marker.kiodoc.kio" } } },
    { match: `\\G${ws}*(${markup.list})`, captures: { 1: { name: "punctuation.definition.marker.kiodoc.kio" } } },
    { begin: `\\G(${ws}*)${markup.heading}`, end: "$", name: "markup.heading.kiodoc.kio", beginCaptures: { 1: { name: "comment.line.documentation.kio" } }, patterns: [{ include: "#doc-inline" }] },
    { include: "#doc-inline" },
  ];
  return {
    begin: "///(?: |(?=[\\t\\r]|$))",
    while: "^[ \\t]*///(?: |(?=[\\t\\r]|$))",
    name: "comment.line.documentation.kio",
    patterns: [{
      repository: { "doc-inline": { patterns: inline }, "doc-line": { patterns: line } },
      patterns: [
      visible(true),
      visible(false),
      {
        begin: `\\G${escape(hiddenOpen)}[A-Za-z](?!.*${escape(hiddenClose)}).*$`,
        end: `\\G${ws}*${escape(hiddenClose)}${ws}*$`,
        name: "markup.raw.block.kiodoc.kio",
      },
      { match: `\\G${p.definition.replaceAll(/\\u([0-9a-f]{4})/g, "\\x{$1}")}.*$`, name: "markup.underline.link.kiodoc.kio" },
      { include: "#doc-line" },
    ] }],
  };
}

const self = fileURLToPath(import.meta.url);
if (process.argv[1] && path.resolve(process.argv[1]) === self) {
  const target = new URL("./kio.tmLanguage.json", import.meta.url);
  const source = fs.readFileSync(target, "utf8");
  const expected = docRule();
  if (process.argv[2] === "--check") {
    if (JSON.stringify(JSON.parse(source).repository["comment-doc"]) !== JSON.stringify(expected)) {
      throw new Error("TextMate Kiodoc rules are stale; run node tools/textmate-kio/doc-rule.mjs");
    }
  } else if (process.argv.length === 2) {
    const start = source.indexOf('    "comment-doc": {');
    const end = source.indexOf('    "comment": {', start);
    if (start < 0 || end < 0) throw new Error("missing TextMate comment rule boundaries");
    const body = JSON.stringify(expected, null, 2).split("\n").map((line, index) => index ? `    ${line}` : `    "comment-doc": ${line}`).join("\n");
    fs.writeFileSync(target, source.slice(0, start) + body + ",\n" + source.slice(end));
  } else throw new Error("usage: node tools/textmate-kio/doc-rule.mjs [--check]");
}
