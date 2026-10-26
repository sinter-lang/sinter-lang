/**
 * @file Sinter grammar for tree-sitter
 * @license MIT OR Apache-2.0
 *
 * The editor-side grammar of Sinter (DESIGN §7.1). The normative grammar is
 * the compiler's pest grammar, `crates/sinter-syntax/src/grammar/sinter.pest`;
 * this one follows it for highlighting and structural editing and is tested
 * against the same corpus (`crates/sinter-syntax/tests/corpus/pass/`) and
 * every example. Where pest has a keyword this grammar sometimes has an
 * identifier (the generic type names `mem`, `fifo`, ...), so it accepts a
 * little more than the compiler does.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const PREC = {
  concat: 1,
  or: 2,
  xor: 3,
  and: 4,
  eq: 5,
  rel: 6,
  shift: 7,
  add: 8,
  mul: 9,
  cast: 10,
  unary: 11,
  postfix: 12,
};

/**
 * Zero or more `rule`s separated by commas, with an optional trailing comma.
 * @param {RuleOrLiteral} rule
 */
function commaSep(rule) {
  return optional(commaSep1(rule));
}

/**
 * One or more `rule`s separated by commas, with an optional trailing comma.
 * @param {RuleOrLiteral} rule
 */
function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}

module.exports = grammar({
  name: 'sinter',

  extras: $ => [/\s/, $.line_comment, $.block_comment],

  word: $ => $.identifier,

  conflicts: $ => [
    // `if x {` / `match x {`: a block, or the struct literal `x { f: .. }`.
    [$._expression, $.struct_expression],
  ],

  rules: {
    source_file: $ => repeat($._item),

    _item: $ => choice(
      $.use_item,
      $.mod_item,
      $.const_item,
      $.struct_item,
      $.enum_item,
      $.impl_item,
      $.function_item,
    ),

    visibility: _ => 'pub',

    // --- Items -------------------------------------------------------------

    use_item: $ => seq(optional($.visibility), 'use', $.use_path, ';'),
    use_path: $ => seq(
      choice('*', $.identifier),
      repeat(seq('::', choice('*', $.identifier))),
    ),

    mod_item: $ => seq(
      optional($.visibility),
      'mod',
      field('name', $.identifier),
      choice(';', seq('{', repeat($._item), '}')),
    ),

    const_item: $ => seq(
      optional($.visibility),
      'const',
      field('name', $.identifier),
      ':',
      field('type', $._type),
      '=',
      field('value', $._expression),
      ';',
    ),

    struct_item: $ => seq(
      repeat($.attribute),
      optional($.visibility),
      'struct',
      field('name', alias($.identifier, $.type_identifier)),
      '{',
      commaSep($.field_declaration),
      '}',
    ),
    field_declaration: $ => seq(
      repeat($.attribute),
      optional($.visibility),
      field('name', alias($.identifier, $.field_identifier)),
      ':',
      field('type', $._type),
      optional(seq('=', field('value', $._expression))),
    ),

    enum_item: $ => seq(
      repeat($.attribute),
      optional($.visibility),
      'enum',
      field('name', alias($.identifier, $.type_identifier)),
      '{',
      commaSep($.enum_variant),
      '}',
    ),
    enum_variant: $ => seq(
      field('name', $.identifier),
      optional(seq('=', field('value', $._expression))),
    ),

    impl_item: $ => seq(
      'impl',
      field('type', alias($.identifier, $.type_identifier)),
      '{',
      repeat($.function_item),
      '}',
    ),

    function_item: $ => seq(
      repeat($.attribute),
      optional($.visibility),
      optional(field('color', $.function_color)),
      'fn',
      field('name', $.identifier),
      field('parameters', $.parameters),
      '->',
      field('return_type', $._type),
      field('body', $.block),
    ),
    function_color: _ => choice('pipeline', 'multiplex'),
    parameters: $ => seq('(', commaSep(choice($.self, $.parameter)), ')'),
    parameter: $ => seq(field('name', $.identifier), ':', field('type', $._type)),

    attribute: $ => seq(
      '#[',
      field('name', $.identifier),
      optional(seq('(', commaSep1($.attribute_argument), ')')),
      ']',
    ),
    attribute_argument: $ => seq(
      field('key', $.identifier),
      optional(seq('=', field('value', choice(
        $.identifier,
        $.integer_literal,
        $.string_literal,
      )))),
    ),

    // --- Statements --------------------------------------------------------

    block: $ => seq(
      '{',
      repeat(choice($.let_statement, $.yield_statement, $.assert_statement)),
      field('value', $._expression),
      '}',
    ),
    let_statement: $ => seq(
      'let',
      field('pattern', $._pattern),
      optional(seq(':', field('type', $._type))),
      '=',
      field('value', $._expression),
      ';',
    ),
    yield_statement: $ => seq('yield', optional($._expression), ';'),
    assert_statement: $ => seq(
      field('macro', choice('assert!', 'debug_assert!')),
      '(',
      field('condition', $._expression),
      ',',
      field('message', $.string_literal),
      ')',
      ';',
    ),

    // --- Patterns ----------------------------------------------------------

    _pattern: $ => choice(
      $.some_pattern,
      $.none,
      $.tuple_pattern,
      $.wildcard_pattern,
      $.integer_literal,
      $.scoped_identifier,
      $.identifier,
    ),
    some_pattern: $ => seq('Some', '(', $._pattern, optional(','), ')'),
    tuple_pattern: $ => seq('(', commaSep1($._pattern), ')'),
    wildcard_pattern: _ => '_',

    // --- Types -------------------------------------------------------------

    _type: $ => choice(
      $.primitive_type,
      $.array_type,
      $.tuple_type,
      $.generic_type,
      alias($.identifier, $.type_identifier),
    ),
    // `u8`, `i16`, and fixed-point `u4.12` / `i1.15`.
    primitive_type: _ => token(prec(1, /[ui][0-9]+(\.[0-9]+)?/)),
    array_type: $ => seq('[', field('element', $._type), ';', field('length', $._expression), ']'),
    tuple_type: $ => seq('(', $._type, repeat1(seq(',', $._type)), optional(','), ')'),
    // `Option<T>`, `mem<T, N>`, `fifo<T, N>`, `sync<from, to>`, ...: the
    // built-in generic types, told apart by name in the highlight queries.
    generic_type: $ => seq(
      field('name', alias($.identifier, $.type_identifier)),
      '<',
      commaSep1(choice($._type, $.integer_literal)),
      '>',
    ),

    // --- Expressions -------------------------------------------------------

    _expression: $ => choice(
      $.binary_expression,
      $.cast_expression,
      $.unary_expression,
      $.field_expression,
      $.call_expression,
      $.index_expression,
      $.try_expression,
      $.if_expression,
      $.match_expression,
      $.block,
      $.include_hex,
      $.some_expression,
      $.none,
      $.struct_expression,
      $.scoped_identifier,
      $.parenthesized_expression,
      $.tuple_expression,
      $.array_expression,
      $.array_repeat_expression,
      $.integer_literal,
      $.self,
      $.identifier,
    ),

    binary_expression: $ => {
      /** @type {[number, RuleOrLiteral][]} */
      const table = [
        [PREC.concat, '@'],
        [PREC.or, '|'],
        [PREC.xor, '^'],
        [PREC.and, '&'],
        [PREC.eq, choice('==', '!=')],
        [PREC.rel, choice('<', '>', '<=', '>=')],
        [PREC.shift, choice('<<', '>>')],
        [PREC.add, choice('+', '-')],
        [PREC.mul, '*'],
      ];
      return choice(...table.map(([p, op]) => prec.left(p, seq(
        field('left', $._expression),
        field('operator', op),
        field('right', $._expression),
      ))));
    },

    cast_expression: $ => prec.left(PREC.cast, seq(
      field('value', $._expression),
      'as',
      field('type', $._type),
      field('mode', $.cast_mode),
    )),
    cast_mode: _ => choice('sat', 'wrap', 'trunc', 'round'),

    unary_expression: $ => prec(PREC.unary, seq(
      field('operator', choice('!', '~', '-')),
      field('operand', $._expression),
    )),

    field_expression: $ => prec(PREC.postfix, seq(
      field('value', $._expression),
      '.',
      field('field', alias($.identifier, $.field_identifier)),
    )),
    call_expression: $ => prec(PREC.postfix, seq(
      field('function', $._expression),
      field('arguments', $.arguments),
    )),
    arguments: $ => seq('(', commaSep($._expression), ')'),
    // `a[i]`, or the bit-slice `a[hi:lo]`.
    index_expression: $ => prec(PREC.postfix, seq(
      field('value', $._expression),
      '[',
      field('index', $._expression),
      optional(seq(':', field('low', $._expression))),
      ']',
    )),
    try_expression: $ => prec(PREC.postfix, seq($._expression, '?')),

    if_expression: $ => seq(
      'if',
      field('condition', $._expression),
      field('consequence', $.block),
      'else',
      field('alternative', choice($.block, $.if_expression)),
    ),

    match_expression: $ => seq(
      'match',
      field('value', $._expression),
      '{',
      commaSep($.match_arm),
      '}',
    ),
    match_arm: $ => seq(field('pattern', $._pattern), '=>', field('value', $._expression)),

    include_hex: $ => seq('include_hex!', '(', field('path', $.string_literal), ')'),

    some_expression: $ => seq('Some', '(', $._expression, optional(','), ')'),
    none: _ => 'None',

    // `S { a: x, ..base }`: the base is last, and may stand alone.
    struct_expression: $ => seq(
      field('name', alias($.identifier, $.type_identifier)),
      '{',
      choice(
        seq(
          $.field_initializer,
          repeat(seq(',', $.field_initializer)),
          optional(seq(',', $.struct_base)),
          optional(','),
        ),
        $.struct_base,
      ),
      '}',
    ),
    struct_base: $ => seq('..', field('value', $._expression)),
    field_initializer: $ => seq(
      field('field', alias($.identifier, $.field_identifier)),
      ':',
      field('value', $._expression),
    ),

    scoped_identifier: $ => seq($.identifier, repeat1(seq('::', $.identifier))),

    parenthesized_expression: $ => seq('(', $._expression, ')'),
    tuple_expression: $ => seq(
      '(',
      $._expression,
      repeat1(seq(',', $._expression)),
      optional(','),
      ')',
    ),
    array_expression: $ => seq('[', commaSep($._expression), ']'),
    array_repeat_expression: $ => seq(
      '[',
      field('value', $._expression),
      ';',
      field('length', choice($.integer_literal, $.identifier)),
      ']',
    ),

    // --- Tokens ------------------------------------------------------------

    self: _ => 'self',

    // Decimal, `0x` hex, or `0b` binary, with `_` separators and an optional
    // width suffix (`_u7`, `_i16`).
    integer_literal: _ => token(seq(
      choice(
        /0x[0-9a-fA-F](_*[0-9a-fA-F])*/,
        /0b[01](_*[01])*/,
        /[0-9](_*[0-9])*/,
      ),
      optional(/_[ui][0-9]+/),
    )),

    string_literal: _ => token(seq('"', /[^"]*/, '"')),

    identifier: _ => /[A-Za-z_][A-Za-z0-9_]*/,

    line_comment: _ => token(seq('//', /[^\n]*/)),
    block_comment: _ => token(seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/')),
  },
});
