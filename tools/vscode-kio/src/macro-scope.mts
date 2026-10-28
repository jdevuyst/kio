import type { Node } from "web-tree-sitter";

type Binding = { name: string; macro: boolean; order: number; family?: number };
type Scope = { names: Map<string, Binding>; numbered: Binding[] };

// This index follows authored introductions, before any expression rewriting.
// Rebuilding it after an edit also invalidates unchanged later call subtrees.
export type MacroScope = {
  heads: Set<number>;
  functions: Set<number>;
};

export function macroHeads(root: Node): Set<number> {
  return macroScope(root).heads;
}

export function macroScope(root: Node): MacroScope {
  const heads = new Set<number>();
  const functions = new Set<number>();
  const emptyScope = (): Scope => ({ names: new Map(), numbered: [] });
  const scopes = [emptyScope()];
  let order = 0, family = 0, nextFamily = 0;
  const bind = (name: string, macro = false, numbered = false): void => {
    const scope = scopes[scopes.length - 1];
    const binding = { name, macro, order: order++, family: numbered ? family : undefined };
    if (numbered) scope.numbered.push(binding);
    else scope.names.set(name, binding);
  };
  const lookup = (name: string): Binding | undefined => {
    const currentFamily = family;
    for (let i = scopes.length - 1; i >= 0; i--) {
      let binding = scopes[i].names.get(name);
      for (const family of scopes[i].numbered) {
        if (family.family === currentFamily && (!binding || family.order > binding.order) && name.startsWith(family.name) && /^[0-9]+$/.test(name.slice(family.name.length))) binding = family;
      }
      if (binding) return binding;
    }
    return undefined;
  };
  const leaves = (node: Node, kinds: string[]): Node[] => {
    if (kinds.includes(node.type)) return [node];
    return node.namedChildren.flatMap((child) => leaves(child, kinds));
  };
  const bindPattern = (node: Node): void => {
    if (node.type === "numbered_binding") {
      const stem = leaves(node, ["variable_parameter"])[0];
      if (stem) bind(stem.text, false, true);
      return;
    }
    if (["row_let_entry", "row_let_shorthand_path"].includes(node.type)) {
      for (const binder of leaves(node, ["variable_parameter"])) bind(binder.text);
      return;
    }
    if (["identifier", "variable_parameter"].includes(node.type)) { bind(node.text); return; }
    let annotated = false;
    for (const child of node.namedChildren) {
      if (child.type === "operator_builtin" && child.text === ":") { annotated = true; continue; }
      if (!annotated || child.type === "local_pattern_tuple") bindPattern(child);
    }
  };
  const directHead = (node: Node): Node | undefined => {
    if (node.type === "typed_ufcs_callable") {
      const callee = node.namedChildren.find((child) =>
        ["call_callee", "grouped_ufcs_callable"].includes(child.type));
      return callee ? directHead(callee) : undefined;
    }
    if (["call_callee", "expression_path"].includes(node.type)) {
      const words = node.namedChildren.filter((child) => !child.type.startsWith("comment") && !child.type.startsWith("doc_"));
      return words.length === 1 && ["identifier", "entity_name_function"].includes(words[0].type) ? words[0] : undefined;
    }
    if (["grouped_expression", "grouped_ufcs_callable"].includes(node.type)) {
      const values = node.namedChildren.filter((child) => !child.type.startsWith("bracket_") && child.type !== "separator_comma" && !child.type.startsWith("comment") && !child.type.startsWith("doc_"));
      return values.length === 1 ? directHead(values[0]) : undefined;
    }
    return undefined;
  };
  const visit = (node: Node): void => {
    if (node.type.startsWith("doc_") || node.type.startsWith("comment")) return;
    if (node.type === "module_decl") { scopes[0] = emptyScope(); return; }
    if (node.type === "import_decl") {
      const aliasMarker = node.namedChildren.findIndex((child) => child.type === "keyword_declaration" && child.text === "as");
      const moduleAlias = aliasMarker >= 0 ? node.namedChildren[aliasMarker + 1] : undefined;
      if (moduleAlias?.type === "entity_name_module") bind(moduleAlias.text);
      for (const child of node.namedChildren) {
        if (child.type === "import_macro_item") {
          const source = child.childForFieldName("source");
          const alias = child.childForFieldName("alias");
          let end = child.nextNamedSibling;
          while (end && (end.type.startsWith("comment") || end.type.startsWith("doc_"))) end = end.nextNamedSibling;
          const completeAs = source?.text !== "as" || alias || (end && !end.isMissing && ["separator_comma", "bracket_rparen"].includes(end.type));
          if (source && !source.isMissing && !alias?.isMissing && completeAs) {
            bind((alias ?? source).text, true);
            heads.add(source.startIndex);
            if (alias) heads.add(alias.startIndex);
          }
        } else if (child.type === "import_named_item") {
          const source = child.childForFieldName("source");
          const alias = child.childForFieldName("alias");
          if (source) bind((alias ?? source).text);
        }
      }
      return;
    }
    if (node.type === "macro_decl") {
      const name = node.namedChildren.find((child) => child.type === "entity_name_function");
      // The declaration's implementation observes the preceding module scope.
      for (const child of node.namedChildren) if (child !== name) visit(child);
      if (name) { bind(name.text, true); heads.add(name.startIndex); }
      return;
    }
    if (["function_definition", "fn_lambda_expr", "equiv_definition"].includes(node.type)) {
      const head = node.namedChildren.find((child) => ["fn_decl_head", "equiv_decl_head"].includes(child.type));
      const name = head?.namedChildren.find((child) => child.type === "entity_name_function");
      if (name && node.type === "function_definition") bind(name.text);
      scopes.push(emptyScope());
      const signature = head ?? node.namedChildren.find((child) => child.type === "fn_signature");
      if (signature) for (const parameter of leaves(signature, ["variable_parameter"])) bind(parameter.text);
      for (const child of node.namedChildren) if (child !== head && child !== signature) visit(child);
      scopes.pop();
      return;
    }
    if (node.type === "placeholder_expression") {
      const previousFamily = family;
      family = ++nextFamily;
      scopes.push(emptyScope());
      const stem = node.namedChildren.find((child) => child.type === "variable_parameter");
      if (stem) bind(stem.text, false, true);
      for (const child of node.namedChildren) if (child !== stem) visit(child);
      scopes.pop();
      family = previousFamily;
      return;
    }
    if (["let_statement", "neutral_bind", "row_let_stmt"].includes(node.type)) {
      const children = node.namedChildren;
      const equal = children.findIndex((child) => ["=", "<-"].includes(child.text));
      if (equal !== -1) {
        for (const child of children.slice(equal + 1)) visit(child);
        for (const child of children.slice(1, equal)) bindPattern(child);
      }
      return;
    }
    if (["expression_block", "neutral_block"].includes(node.type)) {
      scopes.push(emptyScope());
      for (const child of node.namedChildren) visit(child);
      scopes.pop();
      return;
    }
    if (["call_expr", "block_call", "ufcs_expr"].includes(node.type)) {
      const callee = node.type === "block_call" ? node.childForFieldName("head") : node.type === "ufcs_expr" ? node.childForFieldName("callee") : node.namedChildren.find((child) => ["call_callee", "grouped_expression"].includes(child.type));
      const head = callee && directHead(callee);
      const firstBody = node.type === "block_call" && node.childForFieldName("first");
      const unownedPrefix = firstBody && node.namedChildren.some((child) => child.type === "ERROR" && child.endIndex <= firstBody.startIndex);
      if (head && !unownedPrefix) {
        functions.add(head.startIndex);
        if (lookup(head.text)?.macro) heads.add(head.startIndex);
      }
    }
    for (const child of node.namedChildren) visit(child);
  };
  visit(root);
  return { heads, functions };
}
