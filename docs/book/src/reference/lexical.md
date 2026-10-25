# Lexical structure

A Sinter source file is UTF-8 text with the extension `.snt`.

## Comments and whitespace

```sinter
// A line comment runs to the end of the line.

/* A block comment
   can span lines. Block comments don't nest. */
const LIMIT: u8 = 200; // Comments can follow code.
```

Spaces, tabs, and newlines separate tokens and are otherwise ignored.

## Identifiers

An identifier starts with a letter or `_` and continues with letters, digits,
and `_`: `count`, `rx_data`, `_unused`, `Phase2`. Identifiers are
case-sensitive. A lone `_` is the wildcard pattern, not an identifier.

By convention, types and enum variants are `UpperCamelCase`, constants are
`SCREAMING_SNAKE_CASE`, and everything else is `snake_case`. The compiler
doesn't enforce this.

## Reserved words

These words can't be used as identifiers:

```text
as        const     else      enum      fn        gen       if
impl      let       match     mod       multiplex pipeline  pub
struct    use       yield     Option    Some      None
```

`gen` is reserved for generator functions, planned after 1.0.

Some words are special only where they appear, and are ordinary identifiers
elsewhere:

- `self` as a first parameter, and `self`, `super`, `root` at the start of a
  path (see [Items and modules](items.md#paths)).
- The cast modes `wrap`, `sat`, `trunc`, and `round` after `as T`.
- The built-in type names `mem`, `rom`, `fifo`, `ring`, `flow`, `Sender`,
  `Receiver`, `Push`, `sync`, `pulse`, `mailbox`, and `async_fifo` when
  followed by `<`.

Avoid naming a register or port after a SystemVerilog keyword (`logic`,
`begin`, `end`, and so on): the compiler rejects it, because the name would
appear in the generated Verilog.

## Integer literals

```sinter
const DEC: u32 = 1_000_000;
const HEX: u32 = 0xDEAD_BEEF;
const BIN: u8 = 0b1010_0101;
const MASK: u7 = 0b0111111_u7;
```

- Decimal, hexadecimal (`0x`), or binary (`0b`). There are no octal literals.
- `_` may separate digits, but only between two digits: `1_000` is fine,
  `1_` and `0x_FF` are not.
- A literal has no type of its own. It takes the type its context needs (the
  declared type of a `let` or `const`, the other operand of an operator, a
  parameter type), and must fit that type: `let x: u8 = 300;` is an error.
- A **suffix** `_uN` or `_iN` fixes the literal's type, and a mismatch with
  the context is an error. This helps with bit patterns, where a digit too
  many is easy to miss. A decimal value must be in the suffix type's range;
  a hex or binary value is a bit pattern and only has to fit its width, so
  `0x80_i8` is −128.
- There are no negative literals: `-5` is the negation operator applied to
  `5`.

When the compiler can't find a type for a literal it says so, and you fix it
with a type annotation or a suffix:

```sinter,fail
~struct Top {
~    pub v: u8 = 0,
~}
~impl Top {
~    #[clock(domain = sys)]
~    fn tick(self, o: Option<u8>) -> Top {
        let v = match o {
            Some(x) => x,
            None => 0,
        };
~        Top { v: v }
~    }
~}
```

```text
error: cannot infer type of `0`
```

Here `let v: u8 = match o { ... }` fixes it.

## Strings

Strings appear only as attribute values, file paths, and assertion messages:
`"rtl/uart.v"`, `"count overflowed"`. They have no escape sequences and can't
contain `"`.

## Punctuation

```text
+  -  *  <<  >>  &  |  ^  !  ~  @  ==  !=  <  >  <=  >=
=  :  ::  ;  ,  .  ->  =>  ?  #[  ]  (  )  [  ]  {  }  <  >
```

There is no `/` or `%` operator: division isn't a single-cycle operation
in hardware. Division by a power of two is a right shift.
