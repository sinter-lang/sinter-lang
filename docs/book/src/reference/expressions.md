# Expressions

Everything that computes a value is an expression, including `if`, `match`,
and blocks. There are no statements that change anything: a function body
binds names with `let` and ends with the expression that is its result.

## Operator precedence

From tightest to loosest binding:

| Operators | Meaning |
|---|---|
| `.f` `.m(..)` `f(..)` `a[i]` `a[h:l]` `?` | field, method call, call, index, bit slice, [option propagation](option.md#the--operator) |
| `!` `~` `-` (prefix) | logical not, bitwise not, negation |
| `as T mode` | [cast](#casts) |
| `*` | multiply |
| `+` `-` | add, subtract |
| `<<` `>>` | shifts |
| `<` `>` `<=` `>=` | comparisons |
| `==` `!=` | equality |
| `&` | and |
| `^` | exclusive or |
| `\|` | or |
| `@` | concatenation |

Binary operators group left to right. Note that `&`, `^`, and `|` bind
**more loosely** than comparisons, unlike in Rust, so
`a == 1 & b == 2` means `(a == 1) & (b == 2)`. Use parentheses whenever the
reading isn't obvious.

## Arithmetic

Arithmetic never overflows: the result type is wide enough for every result.

| Expression | Result type |
|---|---|
| `uA + uB`, `uA - uB` | `u(max(A, B) + 1)` |
| `iA + iB`, `iA - iB` | `i(max(A, B) + 1)` |
| `uA * uB` | `u(A + B)` |
| `iA * iB` | `i(A + B)` |
| `-iA` | `iA` |

- Both operands must have the same signedness. Mixing `u8` and `i8` is an
  error; cast one first.
- Unsigned subtraction stays unsigned: `u8 - u8` is a `u9`, and 1 − 2 is 511
  (the 9-bit two's complement pattern of −1). For a signed difference, cast
  the operands to a signed type first:
  `(a as i9 wrap) - (b as i9 wrap)` is an `i10`.
- Negation `-` applies only to signed values, and keeps the width: `-x` of
  the most negative value is itself.
- Fixed-point follows the same rules on its integer bits; see
  [Types](types.md#fixed-point).
- There is no division or remainder.

To store a result in a narrower register, cast it: `(acc + x) as i16 wrap`.
In a feedback loop like this one, the cast at the register is where you
choose between wrapping and saturating.

## Bitwise and logical operators

`&`, `|`, `^`, and `~` work bit by bit. The binary ones need both operands to
have the same type, which is also the result type. On `u1` values they are
logical and, or, and exclusive or.

`!` is logical not and takes only a `u1`: `!(a == b)`. For the bitwise
complement of a wider value, use `~`.

## Comparisons

`==`, `!=`, `<`, `>`, `<=`, and `>=` compare two values of the same type and
produce a `u1`. They work on integers, fixed-point numbers (signed comparisons
for signed types), and, for `==` and `!=`, enums. Structs, tuples, arrays, and
`Option`s can't be compared directly.

## Shifts

What a shift does depends on whether the amount is a **literal**:

- **Literal amount**: the result changes width, so no bits are lost.
  `x << 2` on a `u8` is a `u10` (`x` times 4), and `x >> 4` on a `u8` is a
  `u4` (the top nibble).
- **Any other amount** (a runtime value, or a named constant): the result has
  the operand's type. Bits shifted out are lost, and shifting by the width or
  more gives 0. The amount must be unsigned.

`>>` is arithmetic (copies the sign bit) for signed operands and logical for
unsigned ones. `<<` is always logical.

```sinter
struct Shifts {
    pub quad: u10 = 0,
    pub barrel: u8 = 0,
    pub sra: i8 = 0,
}

impl Shifts {
    #[clock(domain = sys)]
    fn tick(self, x: u8, s: i8, amount: u3) -> Shifts {
        Shifts {
            quad: x << 2,
            barrel: x << amount,
            sra: s >> amount,
        }
    }
}
```

## Bit slices and concatenation

`x[hi:lo]` is bits `hi` down to `lo` of `x`, inclusive, as a
`u(hi - lo + 1)`. `x` must be unsigned, and the bounds are literals or
constants: `x[7:4]` is the top nibble of a byte, `x[0:0]` its lowest bit.

`a @ b` concatenates: `a` becomes the high bits and `b` the low bits of a
`u(A + B)`. Both must be unsigned. Together they rearrange fields, for example
to assemble a RISC-V branch offset:

```sinter
fn b_imm(insn: u32) -> u13 {
    insn[31:31] @ insn[7:7] @ insn[30:25] @ insn[11:8] @ 0_u1
}
~struct T { pub v: u13 = 0 }
~impl T { #[clock(domain = sys)] fn tick(self, i: u32) -> T { T { v: b_imm(i) } } }
```

To slice a signed value, cast it to unsigned first. To sign-extend a field,
cast it to a signed type of the same width and then widen:
`(field as i12 wrap) as i32 wrap`.

## Casts

`x as T mode` converts `x` to type `T`. A cast is the only way to change a
type, and the **mode is required**, even for a cast that can't lose anything.

| Mode | Integer bits dropped | Fraction bits dropped |
|---|---|---|
| `wrap` | keep the low bits (modular) | truncate (toward −∞) |
| `trunc` | keep the low bits (modular) | truncate (toward −∞) |
| `sat` | clamp to the target's range | truncate (toward −∞) |
| `round` | keep the low bits (modular) | round to nearest, halves up |

- **Widening** (more bits) is exact in every mode: a signed value is
  sign-extended, an unsigned one zero-extended.
- **Changing signedness** works the same way: `wrap` reinterprets the bits
  (`-1_i4 as u8 wrap` is 255, `0xFFFF_FFFF_u32 as i32 wrap` is −1), and `sat`
  clamps the value (`-5_i8 as u8 sat` is 0, `200 as i8 sat` is 127).
- **Fixed-point** casts adjust the fraction first, then the integer part.
  `x as u8 sat` on an `i4.4` drops the fraction and clamps negative values to
  0. To round *and* saturate, cast twice.
- `x as T mode` binds tighter than any binary operator, so parenthesize the
  operand: `(a + b) as u8 wrap`. To chain casts, parenthesize the inner one.

## Literals in expressions

An integer literal takes its type from context: the other operand
(`x + 1`, `x == 3`), a `let` annotation, a parameter, a field. It must fit
that type. When there is no context, as in `let n = 5;`, give one with an
annotation or a suffix (`5_u4`). See
[Lexical structure](lexical.md#integer-literals).

## Blocks

A block is `{ ... }`: zero or more `let` bindings, then an expression, which
is the block's value. A block can appear anywhere an expression can.

```sinter,ignore
let parity = {
    let lo = x[3:0] ^ x[7:4];
    let two = lo[1:0] ^ lo[3:2];
    two[0:0] ^ two[1:1]
};
```

## `let` and patterns

```sinter,ignore
let total: u9 = a + b;
let (next, carry) = self.digit.step(cmd);
let (_, flag) = pair;
```

`let` binds a name to a value for the rest of the block. An optional type
annotation must match the value's type exactly. A later `let` may reuse a
name, hiding the earlier binding.

A `let` pattern can be a name, `_` (ignore the value), or a tuple of
patterns. Patterns that might not match, such as `Some(x)`, belong in a
`match`.

## `if`

```sinter,ignore
if count == LIMIT { 0 } else { (count + 1) as u8 wrap }
```

The condition is a `u1`. Both branches must have the same type, and `else` is
required: an `if` always has a value. `else if` chains as usual. In hardware,
an `if` is a multiplexer: both branches are computed.

## `match`

```sinter,ignore
match op {
    Op::Add => (a + b) as u8 wrap,
    Op::Xor => a ^ b,
    _ => 0,
}
```

`match` compares a value against patterns, top to bottom, and evaluates the
first arm that matches. Arms are separated by commas, and all arms must have
the same type. The scrutinee can be an integer, an enum, an `Option`, or a
tuple of these, and an arm's pattern can be:

| Pattern | Matches |
|---|---|
| `3`, `0x1F` | that integer |
| `Op::Add` | that enum variant |
| `Some(p)`, `None` | an `Option` whose value matches `p` (see [Option](option.md)) |
| `(p, q)` | a tuple whose elements match `p` and `q` |
| `name` | anything, binding it to `name` |
| `_` | anything |

Patterns nest, so one `match` can decide on several values:

```sinter
~enum Op { Add, Sub }
fn apply(op: Op, arg: Option<u8>, x: u8) -> u8 {
    match (op, arg) {
        (Op::Add, Some(v)) => (v + x) as u8 wrap,
        (Op::Sub, Some(v)) => (v - x) as u8 wrap,
        (_, None) => x,
    }
}
~struct T { pub v: u8 = 0 }
~impl T { #[clock(domain = sys)] fn tick(self, op: Op, a: Option<u8>, x: u8) -> T { T { v: apply(op, a, x) } } }
```

A `match` must be **exhaustive**: some arm must match every possible value.
Enum variants, `Some` / `None`, and the values 0 and 1 of a `u1` can each be
listed exhaustively; any other integer needs a `_` arm (or a binding). When
an arm is missing, the error shows a value that no arm matches:

```sinter,fail
~enum Op { Add, Sub }
fn apply(op: Op, arg: Option<u8>, x: u8) -> u8 {
    match (op, arg) {
        (Op::Add, Some(v)) => (v + x) as u8 wrap,
        (_, None) => x,
    }
}
~struct T { pub v: u8 = 0 }
~impl T { #[clock(domain = sys)] fn tick(self, op: Op, a: Option<u8>, x: u8) -> T { T { v: apply(op, a, x) } } }
```

```text
error: non-exhaustive match on `(Op, Option<u8>)`: no arm matches `(Op::Sub, Some(_))`
```

In hardware, each arm's pattern is a comparison on the bits it inspects, and
the arms form a priority multiplexer.

## Calls and methods

`f(a, b)` calls a function and `x.m(a)` calls method `m` with `x` as `self`.
Arguments must have exactly the parameter types. Calls are inlined, so they
cost nothing beyond the logic they contain.

## Fields and elements

`s.field` reads a struct field, and `a[i]` reads an array element. A runtime
index past the end of the array reads the last element.

`a.set(i, v)` returns a copy of array `a` with element `i` replaced by `v`
(an index past the end changes nothing). There is no assignment, so this is
how an array register is updated:

```sinter
struct Regs {
    pub r: [u8; 4] = [0; 4],
}

impl Regs {
    #[clock(domain = sys)]
    fn tick(self, we: u1, addr: u2, data: u8) -> Regs {
        Regs { r: if we == 1 { self.r.set(addr, data) } else { self.r } }
    }
}
```
