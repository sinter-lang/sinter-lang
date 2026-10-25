; Sinter syntax highlighting.

(line_comment) @comment
(block_comment) @comment

(integer_literal) @number
(string_literal) @string

; Types
(primitive_type) @type.builtin
(generic_type
  name: (type_identifier) @type.builtin
  (#match? @type.builtin "^(Option|mem|rom|fifo|ring|flow|Sender|Receiver|Push|sync|pulse|mailbox|async_fifo)$"))
(type_identifier) @type

; Definitions
(function_item name: (identifier) @function)
(parameter name: (identifier) @variable.parameter)
(enum_variant name: (identifier) @constant)
(const_item name: (identifier) @constant)
(mod_item name: (identifier) @module)
(field_declaration name: (field_identifier) @property)

; Uses
(call_expression function: (identifier) @function.call)
(call_expression
  function: (field_expression field: (field_identifier) @function.method.call))
(field_expression field: (field_identifier) @property)
(field_initializer field: (field_identifier) @property)
(scoped_identifier (identifier) @type . (identifier) @constant .)
(self) @variable.builtin
(none) @constant.builtin

; Attributes
(attribute "#[" @punctuation.special "]" @punctuation.special)
(attribute name: (identifier) @attribute)
(attribute_argument key: (identifier) @property)

[
  "assert!"
  "debug_assert!"
  "include_hex!"
] @function.macro

[
  "use" "mod" "const" "struct" "enum" "impl" "fn" "let" "yield"
] @keyword

; Reserved for generators (`gen fn`, DESIGN §4.4), which have no syntax yet.
((identifier) @keyword
  (#eq? @keyword "gen"))

(visibility) @keyword
(function_color) @keyword
(cast_mode) @keyword
"as" @keyword.operator
"Some" @constant.builtin

[ "if" "else" "match" ] @keyword.conditional

[
  "@" "|" "^" "&" "==" "!=" "<" ">" "<=" ">=" "<<" ">>" "+" "-" "*" "!" "~"
  "=" "=>" "?"
] @operator

[ "(" ")" "[" "]" "{" "}" ] @punctuation.bracket
[ "," ";" ":" "::" "." "->" ] @punctuation.delimiter

(wildcard_pattern) @variable.builtin
