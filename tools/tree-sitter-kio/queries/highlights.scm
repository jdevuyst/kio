; Highlight queries for the Kio tree-sitter grammar.
;
; Maps grammar nodes to the canonical highlight roles. The
; canonical token-kind vocabulary is the `TokenKind` enum in
; `kio-rs/src/tokens.rs` (the source of truth) — its kinds
; (dot-separated) are written out by `kio debug tokens`, and both
; tokenizers must agree on the lexical layer.
;
; Captures use tree-sitter's standard `@kind` syntax. Editor
; integrations (Neovim, Helix, Zed, …) consume the `@`-prefixed
; capture names; the C.1 agreement check
; (`ci/checks/orchestrators/highlight-agreement.sh`) maps each capture back to the
; canonical dot-separated kind.

(let_statement (keyword_let) @keyword.declaration)
(row_let_stmt (keyword_let) @keyword.declaration)
(neutral_bind (keyword_let) @keyword.declaration)
(block_head) @keyword.elaborator
(block_label) @keyword.control
(trailing_block_clause (keyword_declaration) @keyword.declaration)
(cache_declaration (keyword_declaration) @keyword.declaration)
(callable_impl_block (keyword_declaration) @keyword.declaration)
(config_field (keyword_declaration) @keyword.declaration)
(dependency_remap (keyword_declaration) @keyword.declaration)
(docs_block (keyword_declaration) @keyword.declaration)
(elaborator_decl (keyword_declaration) @keyword.declaration)
(elaborator_captures_clause (keyword_declaration) @keyword.declaration)
(elaborator_impl_clause (keyword_declaration) @keyword.declaration)
(import_decl (keyword_declaration) @keyword.declaration)
(role_annotation (keyword_declaration) @keyword.declaration)
(incomplete_role_annotation (keyword_declaration) @keyword.declaration)
(owned_block (keyword_declaration) @keyword.declaration)
(signature_version (keyword_declaration) @keyword.declaration)
(row_let_entry (keyword_declaration) @keyword.declaration)
(target_block (keyword_declaration) @keyword.declaration)
(variadic_callable_block (keyword_declaration) @keyword.declaration)
(dependency_file (keyword_dependency) @keyword.declaration)
(source_block (keyword_source) @keyword.declaration)
(lock_file (keyword_lock) @keyword.declaration)
(resolved_block (keyword_resolved) @keyword.declaration)
(signature_file (keyword_signature) @keyword.declaration)
(signature_with_block (keyword_with) @keyword.declaration)
(signature_breaking_block (keyword_breaking) @keyword.declaration)
(signature_nonbreaking_block (keyword_nonbreaking) @keyword.declaration)
(signature_add_block (keyword_add) @keyword.declaration)
(signature_modify_block (keyword_modify) @keyword.declaration)
(signature_remove_block (keyword_remove) @keyword.declaration)
(package_decl (keyword_package) @keyword.declaration)
(module_decl (keyword_module) @keyword.declaration)
(signature_module_section (keyword_module) @keyword.declaration)
(signature_remove_module (keyword_module) @keyword.declaration)
(build_block (keyword_build) @keyword.declaration)
(host_decl (keyword_host) @keyword.declaration)
(bridge_block (keyword_bridge) @keyword.declaration)
(import_decl . (keyword_import) @keyword.declaration)
(incomplete_import_decl (keyword_import) @keyword.declaration)
(op_decl (keyword_op) @keyword.declaration)
(incomplete_op_decl (keyword_op) @keyword.declaration)
(variadic_decl (keyword_varop) @keyword.declaration)
(incomplete_variadic_decl (keyword_varop) @keyword.declaration)
(import_operator_item . (keyword_op) @keyword.declaration)
(keyword_as)              @identifier
(host_type_head (keyword_type) @keyword.declaration)
(label_forward_decl (keyword_type) @keyword.declaration)
(type_alias_decl_head (keyword_type) @keyword.declaration)
(elaborator_decl (keyword_pub) @keyword.declaration)
(equiv_decl_head (keyword_pub) @keyword.declaration)
(fn_decl_head (keyword_pub) @keyword.declaration)
(host_decl (keyword_pub) @keyword.declaration)
(label_forward_decl (keyword_pub) @keyword.declaration)
(labels_decl_head (keyword_pub) @keyword.declaration)
(literal_definition (keyword_pub) @keyword.declaration)
(newtype_decl_head (keyword_pub) @keyword.declaration)
(newtype_member (keyword_pub) @keyword.declaration)
(op_decl (keyword_pub) @keyword.declaration)
(recursive_function_definition (keyword_pub) @keyword.declaration)
(type_alias_decl_head (keyword_pub) @keyword.declaration)
(variadic_decl (keyword_pub) @keyword.declaration)
(fn_decl_head (keyword_pure) @keyword.declaration)
(fn_decl_head (keyword_fn) @keyword.declaration)
(host_fn_head (keyword_fn) @keyword.declaration)
(type_rec_group . (keyword_rec) @keyword.declaration)
(recursive_function_definition (keyword_rec) @keyword.declaration)
(recursive_function_group (keyword_rec) @keyword.declaration)
(recursive_call_expression (keyword_rec) @keyword.declaration)
(incomplete_recursive_call_expression (keyword_rec) @keyword.declaration)
((recursive_call_annotation name: (_) @keyword.control)
 (#match? @keyword.control "^(poly|cont|escape)$"))
(newtype_decl_head (keyword_rec) @keyword.declaration . (keyword_newtype))
(labels_decl_head (keyword_rec) @keyword.declaration . (keyword_labels))
(newtype_decl_head (keyword_newtype) @keyword.declaration)
(labels_decl_head (keyword_labels) @keyword.declaration)
(equiv_decl_head (keyword_equiv) @keyword.declaration)
(literal_definition (keyword_literal) @keyword.declaration)
(newtype_member (keyword_constructor) @keyword.declaration)
(newtype_member (keyword_projector) @keyword.declaration)
(elaborator_name)             @keyword.elaborator

(identifier)              @identifier
(import_decl (entity_name_module) @entity.name.module)
(module_path (entity_name_module) @entity.name.module)
(host_type_head (entity_name_type) @entity.name.type)
(labels_decl_head (entity_name_type) @entity.name.type)
(newtype_decl_head (entity_name_type) @entity.name.type)
(remap_path (entity_name_type) @entity.name.type)
(type_alias_decl_head (entity_name_type) @entity.name.type)
(type_path (entity_name_type) @entity.name.type)
(call_callee (entity_name_function) @entity.name.function)
(callable_path (entity_name_function) @entity.name.function)
(elaborator_decl (entity_name_function) @entity.name.function)
(equiv_decl_head (entity_name_function) @entity.name.function)
(fn_decl_head (entity_name_function) @entity.name.function)
(host_fn_head (entity_name_function) @entity.name.function)
(newtype_member (entity_name_function) @entity.name.function)
(recursive_call_expression (entity_name_function) @entity.name.function)
(incomplete_recursive_call_expression (entity_name_function) @entity.name.function)
(import_label_item (entity_name_label) @entity.name.label)
(label_entry (entity_name_label) @entity.name.label)
(label_forward_decl (entity_name_label) @entity.name.label)
(label_path (entity_name_label) @entity.name.label)
(binding_pattern (variable_parameter) @variable.parameter)
(existential_binder (variable_parameter) @variable.parameter)
(row_let_entry (variable_parameter) @variable.parameter)
(row_let_shorthand_path (variable_parameter) @variable.parameter)
(type_param_binder (variable_parameter) @variable.parameter)
(type_param_group (variable_parameter) @variable.parameter)
(placeholder_expression (variable_parameter) @variable.parameter)

(operator_builtin)        @operator.builtin
(operator_arrow_type)     @operator.builtin
(operator_arrow_ufcs)     @operator.builtin
(operator_row_suffix)     @operator.user
(operator_run)            @operator.user
(unowned_symbol_run)      @operator.user
(module_path_sep)         @operator.user
(variadic_open)           @operator.user
(variadic_close)          @operator.user
(variadic_star_close)     @operator.user
(variadic_expression (ambiguous_lbracket_star) @operator.user)
(incomplete_variadic_expression (ambiguous_lbracket_star) @operator.user)
(type_param_group (ambiguous_lbracket_star
  (star_open_prefix) @punctuation.bracket
  (kind_annotation) @operator.user))
(import_operator_item (keyword_varop) @keyword.declaration)

(string_literal)          @literal.string
(number_literal)          @literal.number
(bool_literal)            @literal.bool

(comment_line)            @comment.line
(comment_doc)             @comment.doc

(bracket_lparen)          @punctuation.bracket
(bracket_rparen)          @punctuation.bracket
(bracket_lbrace)          @punctuation.bracket
(bracket_rbrace)          @punctuation.bracket
(bracket_lbracket)        @punctuation.bracket
(bracket_rbracket)        @punctuation.bracket
(separator_semicolon)     @punctuation.separator
(separator_comma)         @punctuation.separator
(separator_dot)           @punctuation.separator

(slot)                    @slot

; Recovery leaves have no authenticated structural owner.
(ERROR [
  (entity_name_function)
  (entity_name_label)
  (entity_name_module)
  (entity_name_type)
  (keyword_add)
  (keyword_breaking)
  (keyword_bridge)
  (keyword_build)
  (keyword_constructor)
  (keyword_declaration)
  (keyword_dependency)
  (keyword_equiv)
  (keyword_fn)
  (keyword_host)
  (keyword_import)
  (keyword_labels)
  (keyword_let)
  (keyword_literal)
  (keyword_lock)
  (keyword_modify)
  (keyword_module)
  (keyword_newtype)
  (keyword_nonbreaking)
  (keyword_op)
  (keyword_package)
  (keyword_projector)
  (keyword_pub)
  (keyword_pure)
  (keyword_rec)
  (keyword_remove)
  (keyword_resolved)
  (keyword_signature)
  (keyword_source)
  (keyword_type)
  (keyword_varop)
  (keyword_with)
  (variable_parameter)
] @identifier)

; A nested function header inside a balanced body is recovery syntax.
(unowned_function_head [
  (keyword_fn)
  (keyword_pub)
  (keyword_pure)
  (entity_name_function)
] @identifier)
