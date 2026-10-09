// Per-fence presentation input. Only scanner-authenticated payload leaves enter
// a body; synthetic context and neighbouring fences never project to the source.
import type { Node } from "web-tree-sitter";

type DocRow = { startIndex: number; endIndex: number; row: number; column: number };
export type DocBody = { source: string; rows: (DocRow | null)[] };
export type DocSpan = { start: number; end: number; row: number; column: number };

export function docBodies(root: Node): DocBody[] {
  const groups: Node[][] = [];
  let current: Node[] | undefined;
  const visit = (node: Node): void => {
    if (node.type === "doc_kio_code" || node.type === "doc_kio_self_code") {
      if (!current || current[0].type !== node.type) groups.push(current = []);
      current.push(node);
    } else if (node.type === "comment_doc" || node.type === "doc_line_end") {
      return;
    } else if (node.type.startsWith("doc_") || node.childCount === 0) {
      if (node.endIndex > node.startIndex) current = undefined;
    } else {
      for (const child of node.children) visit(child);
    }
  };
  visit(root);
  return groups.map((nodes) => {
    const rows: (DocRow | null)[] = [];
    const lines: string[] = [];
    if (nodes[0].type === "doc_kio_self_code") {
      lines.push("fn __doc_body() {");
      rows.push(null);
    }
    let row = nodes[0].startPosition.row;
    for (const node of nodes) {
      while (row < node.startPosition.row) { lines.push(""); rows.push(null); row++; }
      lines.push(node.text);
      rows.push({ startIndex: node.startIndex, endIndex: node.endIndex, row: node.startPosition.row, column: node.startPosition.column });
      row++;
    }
    if (nodes[0].type === "doc_kio_self_code") { lines.push("}"); rows.push(null); }
    return { source: lines.join("\n"), rows };
  });
}

export function projectDocNode(body: DocBody, node: Node): DocSpan[] {
  if (node.isMissing) return [];
  const spans: DocSpan[] = [];
  const first = node.startPosition, last = node.endPosition;
  for (let row = first.row; row <= last.row && row < body.rows.length; row++) {
    const origin = body.rows[row];
    if (origin === null || origin === undefined) continue;
    const start = row === first.row ? first.column : 0;
    const length = origin.endIndex - origin.startIndex;
    const end = Math.min(length, row === last.row ? last.column : length);
    if (start < end) spans.push({
      start: origin.startIndex + start, end: origin.startIndex + end,
      row: origin.row, column: origin.column + start,
    });
  }
  return spans;
}

export function docCodeGaps(body: DocBody, covered: { start: number; end: number }[]): DocSpan[] {
  const sorted = [...covered].sort((a, b) => a.start - b.start);
  const gaps: DocSpan[] = [];
  let index = 0;
  for (const row of body.rows) {
    if (row === null) continue;
    let start = row.startIndex;
    while (index < sorted.length && sorted[index].end <= start) index++;
    while (index < sorted.length && sorted[index].start < row.endIndex) {
      const token = sorted[index++];
      if (start < token.start) gaps.push({ start, end: token.start, row: row.row, column: row.column + start - row.startIndex });
      start = Math.max(start, token.end);
    }
    if (start < row.endIndex) gaps.push({ start, end: row.endIndex, row: row.row, column: row.column + start - row.startIndex });
  }
  return gaps;
}
