/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

// Keyword captures follow structural owners. The lexical scanner classifies
// local delimiter runs; provider lookup and operator association belong to Kio.
function sep1(rule, sep) {
  return seq(repeat(sep), rule, repeat(seq(repeat1(sep), rule)), repeat(sep));
}

function kw(word) {
  return token(prec(2, word));
}

function fieldOrders(required, optional = [], between = [], separator = null) {
  const orders = [];
  const visit = (prefix, remaining) => {
    if (required.every((rule) => prefix.includes(rule))) {
      if (separator) {
        if (prefix.length) {
          const gap = between.map((rule) => repeat(seq(rule, separator)));
          orders.push(seq(...gap, ...prefix.flatMap((rule, index) =>
            index ? [separator, ...gap, rule] : [rule]),
          ...between.map((rule) => repeat(seq(separator, rule)))));
        } else if (between.length) {
          orders.push(separated(choice(...between), separator));
        }
      } else {
        orders.push(seq(...between, ...prefix.flatMap((rule) => [rule, ...between])));
      }
    }
    for (const rule of remaining) {
      visit([...prefix, rule], remaining.filter((other) => other !== rule));
    }
  };
  visit([], [...required, ...optional]);
  return choice(...orders);
}

function separated(rule, separator) {
  return seq(rule, repeat(seq(separator, rule)));
}

function semicolonBody($, content, { runs = false, empty = false } = {}) {
  const edge = runs ? repeat($.separator_semicolon) : optional($.separator_semicolon);
  return seq($.bracket_lbrace,
    runs && empty ? seq(edge, optional(seq(content, edge))) :
      empty ? optional(seq(edge, content, edge)) : seq(edge, content, edge),
    $.bracket_rbrace);
}

function configField($, keywords) {
  return seq(choice(...keywords.map((word) => alias(kw(word), $.keyword_declaration)), $.identifier), $._field_value);
}

const NUMBER = /[0-9][0-9_]*(\.[0-9_]+)?([eE][+\-]?[0-9_]+)?/;
const VALUE_REFERENCE_NAME = /_*[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*/;
const TYPE_REFERENCE_NAME = /_*[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*/;
const LABEL_NAME = /[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*/;
const PLACEHOLDER_STEM = /_?[a-z]+(?:[0-9]*_[a-z]+)*/;

function ordinaryAtoms($, prefix = false) {
  return choice(
    $.variadic_expression, $.incomplete_variadic_expression,
    $.recursive_call_expression, $.incomplete_recursive_call_expression,
    ...(prefix ? [$.block_prefix_call, $.block_prefix_ufcs] : [$.call_expr, $.ufcs_expr]),
    $.expression_path, $.grouped_expression, $.unit_expression,
    $.bool_literal, $.number_literal, $.string_literal,
    $.fn_lambda_expr, ...(prefix ? [$.block_prefix_field] : [$.label_value_expr, $.field_expression]),
    $.placeholder_expression, $.elaborator_name,
  );
}

module.exports = grammar({
  name: "kio",

  externals: ($) => [
    $._varop_star_open, $._varop_open, $._varop_star_close, $._varop_close,
    $._editor_boundary, $._editor_block_boundary, $._fixed_run, $._minus, $._unowned_bracket_run,
    $._varop_head_open, $._malformed_varop_head_open,
    $._placeholder_open,
    $._editor_eof,
  ],
  extras: ($) => [/[ \t\r\n]+/, $.comment_doc, $.comment_line],

  inline: ($) => [$._identifier_like, $._value_identifier_like, $._expression_name, $._callable_path_segment, $._type_chain_operator, $._local_binding_name, $._host_type_body, $._host_fn_body],
  conflicts: ($) => [
    [$.build_block],
    [$.elaborator_body],
    [$._expression_atom, $.block_call],
    [$.expression_arguments, $.unit_expression],
    [$.expression_arguments, $.grouped_expression],
    [$.expression_arguments, $.grouped_expression, $.block_value_arguments],
    [$.call_callee],
    [$.module_path_sep, $.operator_run],
    [$._raw_unit, $.call_expr],
    [$._raw_unit, $._expression_atom],
    [$._expression_atom, $.call_expr],
    [$.elaborator_captures_clause],
    [$.binding_pattern, $._type],
    [$.local_binding_pattern, $._type],
    [$.expression_path, $.call_callee, $.label_path],
    [$._token, $.recursive_function_definition, $.recursive_function_group,
      $.recursive_call_expression, $.incomplete_recursive_call_expression, $.call_callee],
    [$.expression_path, $.recursive_call_expression, $.incomplete_recursive_call_expression, $.call_callee],
    [$.recursive_call_expression, $.incomplete_recursive_call_expression, $.call_callee],
    [$.recursive_call_annotation, $._recursive_annotation_tail],
    [$.expression_path, $.recursive_call_annotation, $._recursive_annotation_tail],
    [$.expression_path, $._recursive_annotation_tail],
    [$.expression_path, $._star_type_param_binder],
    [$.recursive_call_annotation],
    [$.expression_path, $.label_path],
    [$.row_let_shorthand_path, $.label_path],
    [$._complete_expression_block, $.label_value_expr],
    [$.expression_block, $.unowned_function],
    [$._token, $.call_callee, $._visibility_modifier],
    [$.call_callee, $._visibility_modifier],
    [$.expression_path, $._visibility_modifier],
    [$.expression_path, $.call_callee],
    [$.type_path, $.expression_path],
    [$._type, $.operator_builtin],
    [$._token, $.op_decl, $.incomplete_op_decl, $.call_callee],
  ],
  word: ($) => $.identifier,

  rules: {
    source_file: ($) => choice(
      $.package_file, seq($.signature_file, repeat($._unit)), $.dependency_file, $.lock_file,
      prec.right(seq(
        repeat(choice($.module_decl, $.import_decl, $.incomplete_import_decl)),
        optional(seq($._body_unit, repeat($._unit))),
      )),
    ),
    package_file: ($) => prec.right(seq($.package_decl, fieldOrders([], [
      seq($.build_block, optional($.separator_semicolon)),
      seq($.bridge_block, optional($.separator_semicolon)),
    ]))),

    _declaration: ($) =>
      choice(
        $.host_decl,
        prec.right(seq(choice($.function_definition, $.recursive_function_definition,
          $.recursive_function_group, $.type_rec_group, $.newtype_decl_head,
          $.equiv_definition, $.op_decl, $.variadic_decl, $.elaborator_decl),
        optional($.separator_semicolon))),
        $.labels_decl_head,
        $.literal_definition,
        $.incomplete_op_decl,
        $.incomplete_variadic_decl,
        $.type_alias_decl_head,
        $.label_forward_decl,
      ),
    _unit: ($) => choice($.module_decl, $.import_decl, $._body_unit),
    _body_unit: ($) => choice($._declaration, $._raw_unit),
    _raw_unit: ($) =>
      choice(
        $.lexical_binary_number,
        $.variadic_expression,
        $.incomplete_variadic_expression,
        $.fn_lambda_expr,
        $.body_block,
        $.ufcs_expr,
        $.call_expr,
        $.recursive_call_expression,
        $.incomplete_recursive_call_expression,
        $._token,
      ),

    _token: ($) => prec(-100, choice(
      alias($._identifier_like, $.identifier),
      $.elaborator_name, $.unowned_symbol_run,
      $.bool_literal, $.number_literal,
      $.string_literal, $.slot, $.operator_arrow_type, $.operator_arrow_ufcs,
      $.operator_row_suffix, $.operator_builtin, $.module_path_sep, $.operator_run,
      $.bracket_lparen, $.bracket_rparen, $.bracket_lbrace, $.bracket_rbrace,
      $.separator_semicolon, $.separator_comma, $.separator_dot,
    )),

    package_decl: ($) =>
      seq(
        $.keyword_package,
        alias($._identifier_like, $.identifier),
        $.separator_semicolon,
      ),
    signature_file: ($) =>
      prec.right(30,
        seq(
          $.keyword_signature,
          $.identifier,
          $.signature_version,
          $.separator_semicolon,
          repeat(seq($.signature_version_block, optional($.separator_semicolon))),
        ),
      ),

    signature_version_block: ($) =>
      seq(
        $.signature_version,
        semicolonBody($, choice(
          fieldOrders([$.signature_breaking_block], [$.signature_with_block, $.signature_nonbreaking_block], [], $.separator_semicolon),
          fieldOrders([$.signature_nonbreaking_block], [$.signature_with_block], [], $.separator_semicolon),
        )),
      ),

    signature_with_block: ($) =>
      seq(
        $.keyword_with,
        semicolonBody($, separated($.signature_module_section, $.separator_semicolon)),
      ),

    signature_breaking_block: ($) =>
      seq($.keyword_breaking, $._signature_change_body),

    signature_nonbreaking_block: ($) =>
      seq($.keyword_nonbreaking, $._signature_change_body),

    _signature_change_body: ($) =>
      semicolonBody($,
        choice(
          fieldOrders([$.signature_add_block], [$.signature_modify_block, $.signature_remove_block], [], $.separator_semicolon),
          fieldOrders([$.signature_modify_block], [$.signature_remove_block], [], $.separator_semicolon),
          $.signature_remove_block,
        ),
      ),

    signature_add_block: ($) =>
      seq($.keyword_add, $._signature_declaration_changes),

    signature_modify_block: ($) =>
      seq($.keyword_modify, $._signature_declaration_changes),

    _signature_declaration_changes: ($) =>
      semicolonBody($, separated(choice($.signature_item_reference, $.signature_module_section), $.separator_semicolon)),

    signature_module_section: ($) =>
      seq($.keyword_module, $.module_path, $.variant_block),

    signature_remove_block: ($) =>
      seq(
        $.keyword_remove,
        semicolonBody($, separated(choice($.signature_item_reference, $.signature_remove_module), $.separator_semicolon)),
      ),

    signature_remove_module: ($) =>
      seq(
        $.keyword_module,
        $.module_path,
        semicolonBody($, separated(alias($._identifier_like, $.identifier), $.separator_semicolon)),
      ),

    signature_item_reference: ($) =>
      seq(
        $.module_path,
        $.separator_dot,
        alias($._identifier_like, $.identifier),
      ),

    signature_version: ($) =>
      seq(
        alias(kw("v"), $.keyword_declaration),
        $.bracket_lparen,
        $.number_literal,
        $.bracket_rparen,
      ),

    dependency_file: ($) =>
      prec.right(30,
        seq(
          $.keyword_dependency,
          $.identifier,
          $.separator_semicolon,
          repeat($.dependency_remap),
          $.source_block, optional($.separator_semicolon),
          repeat($.dependency_remap),
        ),
      ),

    lock_file: ($) =>
      prec.right(30,
        seq(
          $.keyword_lock,
          $.identifier,
          $.separator_semicolon,
          $.resolved_block, optional($.separator_semicolon),
        ),
      ),

    source_block: ($) => seq($.keyword_source, semicolonBody($, separated(alias($._source_config_field, $.config_field), repeat1($.separator_semicolon)), { runs: true })),
    resolved_block: ($) => seq($.keyword_resolved, semicolonBody($, separated(alias($._resolved_config_field, $.config_field), repeat1($.separator_semicolon)), { runs: true })),
    dependency_remap: ($) => seq(
      alias(kw(choice("rehost", "retype")), $.keyword_declaration), $.remap_path,
      alias(kw("to"), $.keyword_declaration), $.remap_path, $.separator_semicolon,
    ),
    remap_path: ($) => seq($.module_path, optional(seq($.separator_dot, alias($.uppercase_identifier, $.entity_name_type)))),

    variant_block: ($) => semicolonBody($, choice(
      separated($._variant_entry, $.separator_semicolon),
      seq(separated($._variant_import, $.separator_semicolon),
        optional(seq($.separator_semicolon, separated($._variant_entry, $.separator_semicolon)))),
    ), { empty: true }),
    _variant_import: ($) => alias($._import_decl_body, $.import_decl),
    _variant_entry: ($) => choice(
      alias($._variant_host_decl, $.host_decl), $.fn_decl_head,
      alias($._type_alias_decl_body, $.type_alias_decl_head), $.type_rec_group,
      $.newtype_decl_head, alias($._labels_decl_body, $.labels_decl_head),
    ),
    build_block: ($) => seq($.keyword_build,
      semicolonBody($, fieldOrders([], [$.cache_declaration, $.docs_block], [$.target_block], repeat1($.separator_semicolon)), { runs: true, empty: true })),
    cache_declaration: ($) => seq(alias(kw("cache"), $.keyword_declaration), $._field_value),
    docs_block: ($) => seq(
      alias(kw("docs"), $.keyword_declaration),
      semicolonBody($, separated(alias($._docs_config_field, $.config_field), repeat1($.separator_semicolon)), { runs: true, empty: true }),
    ),
    target_block: ($) => seq(
      alias(kw("target"), $.keyword_declaration), $.target_name,
      semicolonBody($, separated($.config_field, repeat1($.separator_semicolon)), { runs: true, empty: true }),
    ),
    target_name: ($) => seq($.identifier, repeat(seq(alias("-", $.operator_run), $.identifier))),
    config_field: ($) => seq($.identifier, $._field_value),
    _source_config_field: ($) => configField($, ["path", "git", "ref"]),
    _resolved_config_field: ($) => configField($, ["git", "ref", "path", "commit", "sig"]),
    _docs_config_field: ($) => configField($, ["md", "support", "md_out", "html"]),
    _field_value: ($) => choice($.unit_expression, $.string_literal, $.number_literal, $.identifier),
    bridge_block: ($) => seq(
      $.keyword_bridge,
      semicolonBody($, separated($.bridge_glob, $.separator_semicolon), { empty: true }),
    ),
    bridge_glob: ($) => seq($._bridge_segment, repeat(choice(
      seq($.module_path_sep, $._bridge_segment),
      alias(token(/\/\*\*?/), $.operator_run),
    ))),
    _bridge_segment: ($) => choice(alias($._identifier_like, $.identifier), alias(token(/\*\*?/), $.operator_run)),
    host_decl: ($) => seq(
      optional($._visibility_modifier), $.keyword_host, optional($._visibility_modifier),
      choice($.host_type_head, alias($._incomplete_host_type_head, $.host_type_head), $.host_fn_head),
    ),
    _variant_host_decl: ($) => seq(
      optional($._visibility_modifier), $.keyword_host, optional($._visibility_modifier),
      choice(alias($._signature_host_type_head, $.host_type_head), alias($._signature_host_fn_head, $.host_fn_head)),
    ),
    _signature_host_type_head: ($) => $._host_type_body,
    _signature_host_fn_head: ($) => $._host_fn_body,
    host_type_head: ($) => seq($._host_type_body, $.separator_semicolon),
    _host_type_body: ($) => seq(
      $.keyword_type, alias($.uppercase_identifier, $.entity_name_type),
      optional($.type_param_list), optional($.role_annotation),
      optional($.owned_block),
    ),
    _incomplete_host_type_head: ($) => prec.dynamic(-1, prec.right(seq(
      $.keyword_type, alias($.uppercase_identifier, $.entity_name_type),
      optional($.type_param_list),
      optional(choice($.role_annotation, $.incomplete_role_annotation)),
      optional($.owned_block),
    ))),
    incomplete_role_annotation: ($) => seq(alias(kw("role"), $.keyword_declaration), $.bracket_lparen),
    role_annotation: ($) => seq(
      alias(kw("role"), $.keyword_declaration), $.bracket_lparen,
      alias(choice("i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "f32", "f64", "bool", "str"), $.keyword_declaration),
      $.bracket_rparen,
    ),
    owned_block: ($) => seq($.bracket_lbrace, alias(kw("owned"), $.keyword_declaration), $.bracket_rbrace),
    host_fn_head: ($) => seq($._host_fn_body, $.separator_semicolon),
    _host_fn_body: ($) => seq(
      $.keyword_fn, alias($._identifier_like, $.entity_name_function),
      $.fn_signature, $._fn_return_type,
    ),

    keyword_package: ($) => kw("package"),
    keyword_build: ($) => kw("build"),
    keyword_host: ($) => kw("host"),
    keyword_bridge: ($) => kw("bridge"),
    keyword_dependency: ($) => kw("dependency"),
    keyword_source: ($) => kw("source"),
    keyword_lock: ($) => kw("lock"),
    keyword_resolved: ($) => kw("resolved"),
    keyword_signature: ($) => kw("signature"),
    keyword_with: ($) => kw("with"),
    keyword_breaking: ($) => kw("breaking"),
    keyword_nonbreaking: ($) => kw("nonbreaking"),
    keyword_add: ($) => kw("add"),
    keyword_modify: ($) => kw("modify"),
    keyword_remove: ($) => kw("remove"),

    _identifier_like: ($) => choice($.uppercase_identifier, $._value_identifier_like),
    _value_identifier_like: ($) =>
      choice(
        $.keyword_varop,
        $.keyword_let, $.keyword_rec,
        "poly", "cont", "escape",
        $.identifier,
        $.keyword_declaration,
        $.keyword_dependency,
        $.keyword_source,
        $.keyword_lock,
        $.keyword_resolved,
        $.keyword_signature,
        $.keyword_with,
        $.keyword_breaking,
        $.keyword_nonbreaking,
        $.keyword_add,
        $.keyword_modify,
        $.keyword_remove,
        $.keyword_package,
        $.keyword_module,
        $.keyword_build,
        $.keyword_host,
        $.keyword_bridge,
        $.keyword_import,
        $.keyword_op,
        alias($._keyword_variadic, $.identifier),
        $.keyword_as,
        $.keyword_type,
        $.keyword_pub,
        $.keyword_pure,
        $.keyword_fn,
        $.keyword_literal,
        $.keyword_newtype,
        $.keyword_labels,
        $.keyword_equiv,
        $.keyword_constructor,
        $.keyword_projector,
      ),
    module_decl: ($) =>
      seq(
        $.keyword_module,
        field("path", $.module_path),
        $.separator_semicolon,
      ),
    keyword_module: ($) => kw("module"),
    module_path: ($) =>
      prec.right(seq(
        $._module_segment,
        repeat(seq($.module_path_sep, $._module_segment)),
      )),
    module_path_sep: ($) => "/",
    _module_segment: ($) => alias($._identifier_like, $.entity_name_module),
    import_decl: ($) => prec.dynamic(30, seq($._import_decl_body, $.separator_semicolon)),
    _import_decl_body: ($) =>
      prec.dynamic(30, seq(
          $.keyword_import,
          choice(
            $._import_intrinsics_tail,
            $._import_qualified_tail,
            $._import_selective_tail,
          ),
        )),

    keyword_import: ($) => kw("import"),
    incomplete_import_decl: ($) => prec.dynamic(-1, prec.right(seq($.keyword_import, $.module_path))),
    keyword_op: ($) => kw("op"),
    keyword_varop: ($) => kw("varop"),
    _keyword_variadic: ($) => token(prec(3, "variadic")),
    keyword_as: ($) => kw("as"),
    keyword_type: ($) => kw("type"),
    keyword_pure: ($) => kw("pure"),

    _import_intrinsics_tail: ($) =>
      seq(
        alias(choice("__intrinsics__", "__comptime__"), $.entity_name_module),
      ),

    _import_qualified_tail: ($) =>
      prec(
        1,
        seq(
          $.module_path,
          alias($.keyword_as, $.keyword_declaration),
          alias($._identifier_like, $.entity_name_module),
        ),
      ),

    _import_selective_tail: ($) =>
      prec(
        1,
        seq(
          $.module_path,
          $.bracket_lparen,
          repeat(choice(
            $.import_operator_item,
            $.import_label_item,
            alias($._identifier_like, $.identifier),
            $.separator_comma,
          )),
          $.bracket_rparen,
        ),
      ),

    import_operator_item: ($) =>
      prec.dynamic(20,
        choice(
          seq($.keyword_op, repeat1($._import_pattern_token)),
          seq($.keyword_varop, choice($.variadic_head, alias($._malformed_varop_head_open, $.variadic_open))),
        ),
      ),

    _import_pattern_token: ($) =>
      choice($._import_pattern_leaf, $.string_literal, $.import_pattern_group),

    import_pattern_group: ($) =>
      seq(
        $.bracket_lparen,
        repeat1($._import_pattern_token),
        $.bracket_rparen,
      ),

    _import_pattern_leaf: ($) =>
      choice(
        $.slot,
        $.operator_builtin,
        $.operator_arrow_type,
        $.operator_arrow_ufcs,
        $.operator_row_suffix,
        $._operator_run,
      ),
    import_label_item: ($) =>
      seq(
        $.bracket_lbrace,
        repeat(choice($.comment_doc, $.comment_line)),
        alias($.lowercase_identifier, $.entity_name_label),
        repeat(choice($.comment_doc, $.comment_line)),
        $.bracket_rbrace,
      ),
    type_rec_group: ($) =>
      prec.dynamic(50,
        seq(
          $.keyword_rec,
          semicolonBody($, seq($._type_rec_member, $.separator_semicolon,
            separated($._type_rec_member, $.separator_semicolon))),
        ),
      ),
    _type_rec_member: ($) => choice(alias($._type_alias_decl_body, $.type_alias_decl_head),
      $.newtype_decl_head, alias($._labels_decl_body, $.labels_decl_head)),
    newtype_decl_head: ($) =>
      prec.dynamic(20, prec.right(
        seq(
          optional($._visibility_modifier),
          optional($.keyword_rec),
          $.keyword_newtype,
          alias($._identifier_like, $.entity_name_type),
          optional($.type_param_list),
          repeat($.existential_binder),
          alias(":", $.operator_builtin),
          $._type,
          $.newtype_body,
        ),
      )),
    newtype_body: ($) =>
      semicolonBody($, separated($.newtype_member, repeat1($.separator_semicolon)), { runs: true, empty: true }),

    newtype_member: ($) =>
      seq(
        optional($._visibility_modifier),
        choice($.keyword_constructor, $.keyword_projector),
        alias($._identifier_like, $.entity_name_function),
      ),

    keyword_constructor: ($) => kw("constructor"),
    keyword_projector: ($) => kw("projector"),

    keyword_literal: ($) => kw("literal"),

    function_definition: ($) =>
      choice(
        seq($.fn_decl_head, $._editor_boundary),
        prec.dynamic(1, seq($.fn_decl_head, $.expression_block)),
      ),

    recursive_function_definition: ($) =>
      seq(
        optional($._visibility_modifier),
        $.keyword_rec,
        $.recursive_capability,
        $.function_definition,
      ),

    recursive_function_group: ($) =>
      seq(
        $.keyword_rec,
        $.recursive_capability,
        semicolonBody($, separated($.function_definition, $.separator_semicolon)),
      ),

    recursive_capability: ($) =>
      prec.dynamic(10, seq($.bracket_lparen, $.expression_path, $.bracket_rparen)),

    expression_block: ($) => choice($._complete_expression_block, $.incomplete_expression_block),
    _complete_expression_block: ($) => seq(
      $.bracket_lbrace, repeat($.separator_semicolon),
      repeat(seq(choice($.let_statement, $.row_let_stmt, $._expression), repeat1($.separator_semicolon))),
      optional($._expression), repeat($.unowned_function), $.bracket_rbrace,
    ),
    incomplete_expression_block: ($) => seq(
      $.bracket_lbrace, repeat($.separator_semicolon),
      repeat(seq(choice($.let_statement, $.row_let_stmt, $._expression), repeat1($.separator_semicolon))),
      optional($._expression), repeat($.function_definition), $._editor_eof,
    ),
    unowned_function: ($) => seq(
      alias($.fn_decl_head, $.unowned_function_head),
      alias($._complete_expression_block, $.expression_block),
    ),
    let_statement: ($) => seq(
      $.keyword_let,
      choice($._local_let_pattern, seq($.separator_dot, $.grouped_existential_pattern)),
      alias("=", $.operator_builtin), $._expression,
    ),
    _local_let_pattern: ($) => choice(
      alias($._value_identifier_like, $.identifier), alias("_", $.slot),
      seq($.separator_dot, $.local_pattern_tuple),
    ),
    grouped_existential_pattern: ($) => seq($.bracket_lparen,
      repeat1($.existential_binder),
      choice(alias($._value_identifier_like, $.identifier), alias($._typed_local_pattern_tuple, $.local_pattern_tuple)),
      $.bracket_rparen),
    local_binding_pattern: ($) => prec.dynamic(20, choice(
      $._local_binding_name, $._typed_local_binding_pattern, $.local_pattern_tuple,
    )),
    _local_binding_name: ($) => choice(alias($._value_identifier_like, $.identifier), alias("_", $.slot)),
    _typed_local_binding_pattern: ($) => choice(
      seq($._local_binding_name, alias(":", $.operator_builtin), $._type),
      seq(alias($._value_identifier_like, $.identifier), alias(":", $.operator_builtin),
        alias($._typed_local_pattern_tuple, $.local_pattern_tuple)),
    ),
    local_pattern_tuple: ($) => seq($.bracket_lparen, sep1($.local_binding_pattern, $.separator_comma), $.bracket_rparen),
    // Native as/opening lookahead skips initial '(' tokens, then requires a
    // colon before that group's close. A later outer annotation cannot qualify
    // an initial untyped nested tuple.
    _typed_local_pattern_tuple: ($) => prec(1, seq($.bracket_lparen, choice(
      alias($._typed_local_pattern_tuple, $.local_pattern_tuple),
      $._typed_local_binding_pattern,
      seq(choice(repeat1($.separator_comma), seq($._local_binding_name, repeat1($.separator_comma))),
        repeat(seq($.local_binding_pattern, repeat1($.separator_comma))), $._typed_local_binding_pattern),
    ), repeat(seq(repeat1($.separator_comma), $.local_binding_pattern)),
    repeat($.separator_comma), $.bracket_rparen)),
    keyword_let: ($) => kw("let"),
    existential_binder: ($) => seq(
      alias("<", $.operator_run), alias($._identifier_like, $.variable_parameter),
      alias(">", $.operator_run),
    ),
    binding_pattern: ($) => choice(
      seq(choice(alias($._value_identifier_like, $.variable_parameter), $.slot),
        optional(seq(alias(":", $.operator_builtin), choice($._type, $.pattern_tuple)))),
      $.pattern_tuple,
    ),
    pattern_tuple: ($) => seq($.bracket_lparen, optional(sep1($.binding_pattern, $.separator_comma)), $.bracket_rparen),
    _expression: ($) => choice($.operator_expression, $._expression_atom),
    _expression_atom: ($) => choice(ordinaryAtoms($), $.block_call),
    block_call: ($) => prec.right(seq(
      field('head', alias($.elaborator_name, $.block_head)),
      optional(field('prefix', choice($.block_value_arguments, $._block_bare_prefix))),
      field('first', $.neutral_block),
      repeat(field('labelled', $.labelled_block)),
      optional(field('elided', $.elided_block)),
    )),
    block_value_arguments: ($) => prec(1, seq(
      $.bracket_lparen,
      repeat($.separator_comma),
      optional(seq($._expression,
        repeat(seq(repeat1($.separator_comma), $._expression)),
        repeat($.separator_comma))),
      $.bracket_rparen,
    )),
    _block_bare_prefix: ($) => choice($._block_prefix_atom, $.block_prefix_operator),
    _block_prefix_atom: ($) => ordinaryAtoms($, true),
    block_prefix_call: ($) => prec.left(seq(
      choice($.call_callee, $.elaborator_name, $.block_prefix_call,
        $.grouped_expression, $.fn_lambda_expr, $.number_literal, $.string_literal, $.bool_literal),
      $.expression_arguments,
    )),
    block_prefix_ufcs: ($) => choice(
      prec.right(20, seq($._block_prefix_atom, alias($._right_ufcs_arrow, $.operator_arrow_ufcs),
        choice($.call_callee, $.elaborator_name), optional($._call_args_group))),
      prec.left(20, seq(choice($.call_callee, $.elaborator_name), optional($._call_args_group),
        alias($._left_ufcs_arrow, $.operator_arrow_ufcs), $._block_prefix_atom)),
    ),
    block_prefix_field: ($) => prec.left(30, seq($._block_prefix_atom, $.operator_row_suffix, $.label_value_expr)),
    block_prefix_operator: ($) => prec.left(1, seq(
      $._block_bare_prefix, $._expression_operator,
      optional($._expression),
    )),
    labelled_block: ($) => seq(field('label', alias($.identifier, $.block_label)), field('body', $.neutral_block)),
    elided_block: ($) => prec.right(seq(field('label', alias($.identifier, $.block_label)), field('body', $.block_call))),
    neutral_block: ($) => choice($._complete_neutral_block, $.incomplete_neutral_block),
    _complete_neutral_block: ($) => prec(2, seq(
      $.bracket_lbrace, repeat($.separator_semicolon),
      repeat(seq(choice($.neutral_bind, $.let_statement, $.row_let_stmt, $._expression),
        repeat1($.separator_semicolon))),
      optional($._expression), repeat($.unowned_function), $.bracket_rbrace,
    )),
    incomplete_neutral_block: ($) => prec(2, seq(
      $.bracket_lbrace, repeat($.separator_semicolon),
      repeat(seq(choice($.neutral_bind, $.let_statement, $.row_let_stmt, $._expression),
        repeat1($.separator_semicolon))),
      optional($._expression), repeat($.function_definition), $._editor_eof,
    )),
    neutral_bind: ($) => seq($.keyword_let, $._local_let_pattern,
      alias('<-', $.operator_run), $._expression),
    operator_expression: ($) => choice(
      prec.right(3, seq($._expression_operator, $._expression)),
      prec.left(2, seq($._expression, $._expression_operator)),
      prec.left(1, seq($._expression, $._expression_operator, $._expression)),
    ),
    _expression_operator: ($) => choice($.operator_run, $.operator_builtin, $.operator_arrow_type),
    placeholder_expression: ($) => seq(
      alias($._placeholder_open, $.separator_dot),
      alias(token.immediate(PLACEHOLDER_STEM), $.variable_parameter),
      alias(token.immediate("."), $.separator_dot),
      $.expression_block,
    ),
    variadic_expression: ($) => prec.dynamic(60, choice(
      seq($.ambiguous_lbracket_star, optional($.variadic_elements), $.variadic_star_close),
      seq($.variadic_open, optional($.variadic_elements), $.variadic_close),
    )),
    incomplete_variadic_expression: ($) => prec.dynamic(30, prec.right(seq(
      choice($.ambiguous_lbracket_star, $.variadic_open), optional($.variadic_elements), $._editor_boundary,
    ))),
    variadic_head: ($) => seq(
      alias($._varop_head_open, $.variadic_open), $.variadic_head_gap,
      choice($.variadic_star_close, $.variadic_close),
    ),
    variadic_head_gap: ($) => choice($.head_whitespace, $.comment_doc, $.comment_line),
    head_whitespace: ($) => token.immediate(/[ \t\r\n]+/),
    variadic_elements: ($) => choice(repeat1($.separator_comma), sep1($.variadic_element, $.separator_comma)),
    variadic_element: ($) => $._expression,
    variadic_star_close: ($) => $._varop_star_close,
    variadic_open: ($) => $._varop_open,
    variadic_close: ($) => $._varop_close,
    forall_type: ($) => prec.dynamic(65, seq($.type_param_list, $._type)),
    incomplete_forall_type: ($) => seq(
      $.type_param_list, optional($.operator_arrow_type), $._editor_boundary,
    ),
    _type: ($) => choice(
      $.type_path, $.forall_type, $.type_group, $.type_arrow, $.type_product,
      alias(".", $.separator_dot), alias("!", $.operator_builtin), $.slot,
    ),
    type_path: ($) => prec.dynamic(1, prec.right(seq(
      repeat(seq(alias($._expression_name, $.identifier), $.separator_dot)),
      alias($.uppercase_identifier, $.entity_name_type),
      optional(choice($.type_arguments, $.incomplete_type_arguments)),
    ))),
    type_arguments: ($) => seq($.bracket_lparen, sep1($._type, $.separator_comma), $.bracket_rparen),
    incomplete_type_arguments: ($) => seq(
      $.bracket_lparen, optional(sep1($._type, $.separator_comma)), $._editor_block_boundary,
    ),
    type_group: ($) => seq($.bracket_lparen, $._type, $.bracket_rparen),
    type_arrow: ($) => prec.right(1, seq($._type, $.operator_arrow_type, $._type)),
    type_product: ($) => prec.right(2, choice(
      seq($._type, $._type_chain_operator, optional($._type)),
      seq($._type_chain_operator, optional($._type)),
    )),
    _type_chain_operator: ($) => choice(
      alias(choice("&", "|"), $.operator_builtin), alias(choice(/&&+/, /\|\|+/), $.operator_run),
    ),
    field_expression: ($) => prec.left(30, seq($._expression_atom, $.operator_row_suffix, $.label_value_expr)),

    _expression_name: ($) =>
      $._identifier_like,

    expression_path: ($) =>
      prec.right(seq(
        alias($._identifier_like, $.identifier),
        repeat(seq($.separator_dot, alias($._expression_name, $.identifier))),
      )),

    expression_arguments: ($) => seq(
      $.bracket_lparen,
      optional(sep1(choice($._expression, $._type, $.incomplete_forall_type), $.separator_comma)),
      $.bracket_rparen,
    ),

    recursive_call_expression: ($) =>
      prec.dynamic(10, seq(
        $.keyword_rec,
        optional($.recursive_call_annotations),
        alias($._expression_name, $.entity_name_function),
        $.expression_arguments,
      )),

    incomplete_recursive_call_expression: ($) => prec.dynamic(5, prec.right(choice(
      seq($.keyword_rec, optional($.recursive_call_annotations),
        alias($._expression_name, $.entity_name_function)),
      seq($.keyword_rec, $.bracket_lparen, optional($._recursive_call_annotation_sequence),
        $._editor_block_boundary),
    ))),

    recursive_call_annotations: ($) =>
      seq(
        $.bracket_lparen,
        optional($._recursive_call_annotation_sequence),
        $.bracket_rparen,
      ),

    _recursive_call_annotation_sequence: ($) => choice(
      sep1(choice($.recursive_call_annotation, $.invalid_recursive_call_annotation), $.separator_comma),
      repeat1($.separator_comma),
    ),

    recursive_call_annotation: ($) => choice(
      prec.dynamic(2, prec.right(seq(
        field("name", alias(choice("poly", "cont", "escape"), $.keyword_control)),
        repeat($._recursive_annotation_tail),
      ))),
      prec.dynamic(1, prec.right(seq(
        field("name", alias($._identifier_like, $.identifier)),
        repeat($._recursive_annotation_tail),
      ))),
    ),

    invalid_recursive_call_annotation: ($) => prec.right(repeat1($._recursive_annotation_tail)),

    recursive_recovery_group: ($) => choice(
      seq($.bracket_lparen, repeat(choice($._recursive_annotation_tail, $.separator_comma)), $.bracket_rparen),
      seq($.bracket_lbrace, repeat(choice($._recursive_annotation_tail, $.separator_comma)), $.bracket_rbrace),
      seq($.ambiguous_lbracket_star, repeat(choice($._recursive_annotation_tail, $.separator_comma)), $.variadic_star_close),
      seq($.variadic_open, repeat(choice($._recursive_annotation_tail, $.separator_comma)), $.variadic_close),
      seq(choice($.ambiguous_lbracket_star, $.variadic_open),
        repeat(choice($._recursive_annotation_tail, $.separator_comma)), $._editor_boundary),
    ),

    _recursive_annotation_tail: ($) => choice(
      $.recursive_recovery_group, alias($._identifier_like, $.identifier),
      $.elaborator_name, $.unowned_symbol_run,
      $.bool_literal, $.number_literal,
      $.string_literal, $.slot, $.operator_arrow_type, $.operator_arrow_ufcs,
      $.operator_row_suffix, $.operator_builtin, $.module_path_sep, $.operator_run,
      $.separator_semicolon, $.separator_dot,
    ),

    grouped_expression: ($) => seq($.bracket_lparen, sep1($._expression, $.separator_comma), $.bracket_rparen),
    unit_expression: ($) => seq($.bracket_lparen, $.bracket_rparen),

    type_alias_decl_head: ($) => seq($._type_alias_decl_body, $.separator_semicolon),
    _type_alias_decl_body: ($) =>
      prec.dynamic(20, prec.right(
        seq(
          optional($._visibility_modifier),
          $.keyword_type,
          alias($.uppercase_identifier, $.entity_name_type),
          optional($.type_param_list),
          seq(
            $.operator_builtin, // `=`
            $._type,
          ),
        ),
      )),

    label_forward_decl: ($) =>
      prec.dynamic(20, seq(
        optional($._visibility_modifier),
        $.keyword_type,
        $.bracket_lbrace,
        alias($.lowercase_identifier, $.entity_name_label),
        $.bracket_rbrace,
        alias("=", $.operator_builtin),
        $.bracket_lbrace,
        $.label_path,
        $.bracket_rbrace,
        $.separator_semicolon,
      )),
    uppercase_identifier: ($) => token(prec(1, TYPE_REFERENCE_NAME)),
    lowercase_identifier: ($) => token(prec(1, LABEL_NAME)),
    type_param_list: ($) => prec.right(repeat1($.type_param_group)),

    type_param_group: ($) =>
      choice(
        seq(
          $.bracket_lbracket,
          $._type_param_group_after_open,
        ),
        seq(
          $.ambiguous_lbracket_star,
          $._star_type_param_binder,
          $._type_param_group_after_first_binder,
        ),
      ),

    _star_type_param_binder: ($) => alias($._identifier_like, $.variable_parameter),

    _type_param_group_after_open: ($) =>
      seq(
        repeat($.separator_comma),
        $.type_param_binder,
        $._type_param_group_after_first_binder,
      ),

    _type_param_group_after_first_binder: ($) =>
      seq(
        repeat(seq(repeat1($.separator_comma), $.type_param_binder)),
        repeat($.separator_comma),
        $.bracket_rbracket,
      ),
    fn_lambda_expr: ($) => prec.dynamic(20, seq(
      $.separator_dot, $.fn_signature, optional($._fn_return_type), $.expression_block,
    )),

    body_block: ($) => $.expression_block,

    call_expr: ($) => prec.left(seq(
      choice($.call_callee, $.elaborator_name, $.call_expr,
        $.grouped_expression, $.fn_lambda_expr, $.number_literal, $.string_literal, $.bool_literal),
      $.expression_arguments,
    )),
    call_callee: ($) => choice(
      alias($._value_identifier_like, $.entity_name_function),
      seq(alias($._identifier_like, $.identifier), $.separator_dot,
        repeat(seq(alias($._expression_name, $.identifier), $.separator_dot)),
        alias($._expression_name, $.entity_name_function)),
    ),

    ufcs_expr: ($) =>
      choice(
        prec.right(
          20,
          seq(
            $._expression_atom,
            alias($._right_ufcs_arrow, $.operator_arrow_ufcs),
            choice($.call_callee, $.elaborator_name),
            optional($._call_args_group),
          ),
        ),
        prec.left(
          20,
          seq(
            choice($.call_callee, $.elaborator_name),
            optional($._call_args_group),
            alias($._left_ufcs_arrow, $.operator_arrow_ufcs),
            $._expression_atom,
          ),
        ),
      ),

    _right_ufcs_arrow: ($) => token(prec(2, choice(".>", ".>>"))),
    _left_ufcs_arrow: ($) => token(prec(2, choice(".<", ".<<"))),
    _call_args_group: ($) => $.expression_arguments,

    row_let_stmt: ($) =>
      prec.dynamic(30,
        seq(
          $.keyword_let,
          $.separator_dot,
          $.bracket_lparen,
          $.bracket_lbrace,
          repeat($.separator_comma),
          $.row_let_entry,
          repeat(seq(
            repeat1($.separator_comma),
            $.row_let_entry,
          )),
          repeat($.separator_comma),
          $.bracket_rbrace,
          $.bracket_rparen,
          alias("=", $.operator_builtin),
          $._expression,
        ),
      ),

    row_let_entry: ($) =>
      prec.right(
        choice(
          seq(
            $.label_path,
            alias($.keyword_as, $.keyword_declaration),
            alias($.identifier, $.variable_parameter),
          ),
          $.row_let_shorthand_path,
        ),
      ),

    row_let_shorthand_path: ($) =>
      seq(
        repeat(seq($.identifier, $.separator_dot)),
        alias($.identifier, $.variable_parameter),
      ),

    label_path: ($) =>
      seq(
        repeat(seq(alias($._identifier_like, $.identifier), $.separator_dot)),
        alias($._identifier_like, $.entity_name_label),
      ),
    label_value_expr: ($) =>
      prec.dynamic(20,
        seq(
          $.bracket_lbrace,
          optional(sep1($.label_value_label, $.separator_comma)),
          $.bracket_rbrace,
        ),
      ),

    label_value_label: ($) =>
      prec.right(
        choice(
          seq(
            $.label_path,
            alias("=", $.operator_builtin),
            optional($._expression),
          ),
          $.label_path,
        ),
      ),

    labels_decl_head: ($) => seq($._labels_decl_body, $.separator_semicolon),
    _labels_decl_body: ($) =>
      prec.dynamic(40,
        seq(
          optional($._visibility_modifier),
          optional($.keyword_rec),
          $.keyword_labels,
          optional(
            seq(
              alias($._identifier_like, $.entity_name_type),
              optional($.type_param_list),
              $.operator_builtin, // `=`
            ),
          ),
          $.labels_arms,
        ),
      ),

    labels_arms: ($) => sep1($.labels_block, $.operator_builtin),

    labels_block: ($) =>
      seq(
        $.bracket_lbrace,
        optional(sep1($.label_entry, $.separator_comma)),
        $.bracket_rbrace,
      ),
    label_entry: ($) =>
      prec.right(
        seq(
          alias($._identifier_like, $.entity_name_label),
          optional($.type_param_list),
          repeat($.existential_binder),
          $.operator_builtin, // `:`
          $._type,
        ),
      ),

    keyword_pub: ($) => kw("pub"),
    keyword_fn: ($) => kw("fn"),
    keyword_rec: ($) => kw("rec"),
    keyword_newtype: ($) => kw("newtype"),
    keyword_labels: ($) => kw("labels"),
    keyword_equiv: ($) => kw("equiv"),

    _visibility_modifier: ($) => prec.dynamic(3, seq($.keyword_pub, optional(seq($.bracket_lparen, $.module_path, $.bracket_rparen)))),

    _fn_decl_modifiers: ($) =>
      prec(3, choice(
        $._visibility_modifier,
        $.keyword_pure,
        seq($._visibility_modifier, $.keyword_pure),
        seq($.keyword_pure, $._visibility_modifier),
      )),
    fn_decl_head: ($) =>
      prec.dynamic(20, prec.right(
        seq(
          optional($._fn_decl_modifiers),
          $.keyword_fn,
          alias($._identifier_like, $.entity_name_function),
          $.fn_signature,
          optional($._fn_return_type),
        ),
      )),

    equiv_definition: ($) => seq($.equiv_decl_head, $.expression_block),
    literal_definition: ($) => seq(
      optional($._visibility_modifier), $.keyword_literal, $.identifier,
      alias("=", $.operator_builtin),
      choice($.number_literal, $.string_literal, $.bool_literal), $.separator_semicolon,
    ),

    equiv_decl_head: ($) =>
      prec.dynamic(20, prec.right(
        seq(
          optional($.keyword_pub),
          $.keyword_equiv,
          alias($._identifier_like, $.entity_name_function),
          optional($.fn_signature),
          optional($._fn_return_type),
        ),
      )),
    op_decl: ($) =>
      prec.dynamic(50,
        seq(
          optional($._visibility_modifier),
          $.keyword_op,
          repeat1($._op_pattern_token),
          $.callable_impl_block,
        ),
      ),
    incomplete_op_decl: ($) => prec.dynamic(10, prec.right(seq($.keyword_op, repeat1($._op_pattern_token)))),

    _op_pattern_token: ($) =>
      choice(
        $.comment_doc,
        $.comment_line,
        $.string_literal,
        $.slot,
        $.operator_builtin,
        $.operator_arrow_type,
        $.operator_arrow_ufcs,
        $.operator_row_suffix,
        $._operator_run,
        $.bracket_lparen,
        $.bracket_rparen,
      ),

    callable_impl_block: ($) =>
      semicolonBody($, seq(
        repeat(choice($.comment_doc, $.comment_line)),
        alias(token(prec(3, "impl")), $.keyword_declaration),
        $.callable_path,
        repeat(choice($.comment_doc, $.comment_line)),
      )),

    variadic_decl: ($) =>
      prec.dynamic(50,
        seq(
          optional($._visibility_modifier),
          $.keyword_varop,
          $.variadic_head,
          $.variadic_callable_block,
        ),
      ),
    incomplete_variadic_decl: ($) => prec.dynamic(10, prec.right(seq(
      $.keyword_varop, alias($._malformed_varop_head_open, $.variadic_open),
    ))),

    variadic_callable_block: ($) =>
      semicolonBody($, seq(
        repeat(choice($.comment_doc, $.comment_line)),
        fieldOrders([$._variadic_primary_clause], [$._variadic_finalize_clause], [], $.separator_semicolon),
      )),

    _variadic_primary_clause: ($) =>
      seq(
        alias(
          token(prec(3, choice("foldl", "foldr", "foldl1", "foldr1"))),
          $.keyword_declaration,
        ),
        $.callable_path,
        $.callable_path,
        repeat(choice($.comment_doc, $.comment_line)),
      ),
    _variadic_finalize_clause: ($) =>
      seq(
        alias(token(prec(3, "finalize")), $.keyword_declaration),
        $.callable_path,
        repeat(choice($.comment_doc, $.comment_line)),
      ),

    elaborator_decl: ($) =>
      prec.dynamic(50,
        seq(
          optional($._visibility_modifier),
          $.keyword_declaration,
          alias($._callable_path_segment, $.entity_name_function),
          $.operator_builtin, // `:`
          $._type,
          $.elaborator_body,
        ),
      ),

    elaborator_body: ($) =>
      semicolonBody($, seq(
        repeat(choice($.comment_doc, $.comment_line)),
        fieldOrders([$.elaborator_impl_clause], [$.elaborator_captures_clause], [$.trailing_block_clause], $.separator_semicolon),
        repeat(choice($.comment_doc, $.comment_line)),
      )),

    trailing_block_clause: ($) => seq(
      alias(kw("trailing"), $.keyword_declaration),
      alias(kw(choice("product", "thunk", "sequence")), $.keyword_declaration),
      optional(field("label", alias($.identifier, $.block_label))),
    ),

    elaborator_captures_clause: ($) =>
      seq(
        alias(token(prec(3, "captures")), $.keyword_declaration),
        choice(
          $.plain_value_path,
          seq(
            $.bracket_lparen,
            optional(sep1($.plain_value_path, $.separator_comma)),
            optional($.separator_comma),
            $.bracket_rparen,
          ),
        ),
        repeat(choice($.comment_doc, $.comment_line)),
      ),

    elaborator_impl_clause: ($) =>
      seq(
        alias(token(prec(3, "impl")), $.keyword_declaration),
        choice(
          $.callable_path,
          seq(
            repeat($._callable_path_comment),
            $.bracket_lparen,
            repeat($._callable_path_comment),
            alias(token(prec(3, "fills")), $.keyword_declaration),
            repeat($._callable_path_comment),
            $.bracket_rparen,
            $.callable_path,
          ),
        ),
      ),

    plain_value_path: ($) =>
      seq(
        alias($._callable_path_segment, $.identifier),
        repeat(seq($.separator_dot, alias($._callable_path_segment, $.identifier))),
      ),

    callable_path: ($) =>
      prec.right(
        seq(
          repeat($._callable_path_comment),
          repeat(
            seq(
              alias($._callable_path_segment, $.identifier),
              repeat($._callable_path_comment),
              $.separator_dot,
              repeat($._callable_path_comment),
            ),
          ),
          alias($._callable_path_segment, $.entity_name_function),
          repeat($._callable_path_comment),
        ),
      ),

    _callable_path_comment: ($) =>
      choice(
        $.comment_doc,
        $.comment_line,
      ),

    _callable_path_segment: ($) => $._identifier_like,
    _fn_return_type: ($) =>
      prec.right(seq($.operator_arrow_type, $._type)),
    fn_signature: ($) =>
      prec.dynamic(30, prec.right(repeat1(choice($.type_param_group, $.value_param_group)))),

    value_param_group: ($) =>
      seq(
        $.bracket_lparen,
        optional(sep1($.value_param_binder, $.separator_comma)),
        $.bracket_rparen,
      ),

    type_param_binder: ($) =>
      seq(
        optional(
          choice(
            alias(token(/\*+/), $.operator_run),
          ),
        ),
        alias($._identifier_like, $.variable_parameter),
      ),

    value_param_binder: ($) => $.binding_pattern,

    _sig_type_token: ($) => $._type,

    separator_semicolon: ($) => ";",
    separator_comma: ($) => ",",
    separator_dot: ($) => ".",
    keyword_declaration: ($) => kw("elab"),
    elaborator_name: ($) =>
      token(
        prec(2, seq(
          VALUE_REFERENCE_NAME,
          "!",
        )),
      ),
    slot: ($) => choice("___", "__", "_"),

    bool_literal: ($) => seq(alias($.separator_dot, $.bool_prefix), $.bool_value),
    bool_value: ($) => token.immediate(choice("t", "f")),
    number_literal: ($) => choice(token(NUMBER), prec(4, seq($._minus, token.immediate(NUMBER)))),
    // The lexical fallback also retains the native previous-token rule when
    // the surrounding text has no complete expression owner.
    lexical_binary_number: ($) => prec.dynamic(2, prec.left(2, seq(
      choice(alias($._identifier_like, $.identifier), $.number_literal, $.string_literal,
        $.bracket_rparen, $.bracket_rbrace, $.grouped_expression, $.unit_expression,
        $.call_expr, $.body_block, $.bool_literal, $.lexical_binary_number),
      alias($._minus, $.operator_run), alias(token(NUMBER), $.number_literal),
    ))),
    string_literal: ($) =>
      token(
        seq(
          '"',
          repeat(
            choice(
              /[^"\\\n]/,
              /\\(?:["\\\/bfnrt]|u[0-9A-Fa-f]{4})/,
            ),
          ),
          '"',
        ),
      ),
    comment_doc: ($) =>
      token(seq("///", optional(seq(/[ \t\r]/, /[^\n]*/)))),

    comment_line: ($) =>
      token(seq("//", optional(seq(/[ \t\r]/, /[^\n]*/)))),
    bracket_lparen: ($) => "(",
    bracket_rparen: ($) => ")",
    bracket_lbrace: ($) => "{",
    bracket_rbrace: ($) => "}",
    bracket_lbracket: ($) => "[",
    bracket_rbracket: ($) => token(prec(3, "]")),
    _operator_run: ($) => $.operator_run,

    ambiguous_lbracket_star: ($) => seq(
      alias($._varop_star_open, $.star_open_prefix),
      alias(token.immediate(/\*+/), $.kind_annotation),
    ),
    operator_arrow_type: ($) => "->",
    operator_arrow_ufcs: ($) => token(prec(2, choice(".>", ".>>", ".<", ".<<"))),
    operator_row_suffix: ($) => token(prec(2, choice(".?", ".!"))),
    operator_builtin: ($) => choice("&", "|", "=", ":", "!"),
    operator_run: ($) => choice($._fixed_run, $._minus, "/"),
    unowned_symbol_run: ($) => $._unowned_bracket_run,
    identifier: ($) => token(choice(
      /[A-Za-z][A-Za-z0-9_]*/,
      /_+[A-Za-z0-9][A-Za-z0-9_]*/,
    )),
  },
});
