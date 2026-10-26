# Types

Every value in Sinter has a fixed width known at compile time, and every
value is at most 64 bits wide (aggregates can hold any number of values).
There are no implicit conversions: where a type is expected, the value must
have exactly that type, and changing a type takes a [cast](expressions.md#casts).

## Integers

| Type | Values | Width |
|---|---|---|
| `uN` | unsigned, 0 to 2<sup>N</sup>−1 | N bits, 1 ≤ N ≤ 64 |
| `iN` | two's complement, −2<sup>N−1</sup> to 2<sup>N−1</sup>−1 | N bits, 1 ≤ N ≤ 64 |

`u1` is the boolean type: comparisons produce a `u1`, `if` conditions and
`!` take one, and `1` is true.

Arithmetic widens so its result always fits (`u8 + u8` is a `u9`); see
[Expressions](expressions.md#arithmetic) for the rules.

## Fixed-point

`uI.F` and `iI.F` are fixed-point numbers with `I` integer bits and `F`
fractional bits, `I + F` bits in all. For a signed type, `I` includes the
sign bit, so `i4.8` is 12 bits holding −8.0 to just under +8.0 in steps of
2<sup>−8</sup>. `I + F` is at most 64.

```sinter
struct Filter {
    pub y: i4.4 = 0,
}

impl Filter {
    #[clock(domain = sys)]
    fn tick(self, x: i4.4) -> Filter {
        // An `i4.4` times an `i4.4` is an `i8.8`, exactly.
        let product = x * x;
        // Back to Q4.4: round the dropped fraction bits, wrap the integer.
        Filter { y: product as i4.4 round }
    }
}
```

- `*` adds the integer and the fractional bits of its operands: `i4.4 * i4.4`
  is `i8.8`. A product never overflows.
- `+` and `-` need both operands to have the same number of fractional bits,
  and add one integer bit. Align them with a cast first.
- A [cast](expressions.md#casts) converts between fixed-point formats, and
  between fixed-point and integers: `x as i8 trunc` drops the fraction,
  rounding toward −∞ (2.5 becomes 2, −2.5 becomes −3).
- A literal is the **raw stored value**, not the number it represents: in a
  `u4.4`, `16` is 1.0 and `1` is 1/16. Write `1.0` as the integer cast instead
  (`1_u1 as u4.4 wrap`) or as its raw bits.

In the generated Verilog, a fixed-point value is a plain bit vector of its raw
bits, commented with its Q format.

## Enums

```sinter
enum Phase {
    Idle,
    Busy,
    Done,
}
~struct T { pub p: Phase = Phase::Idle }
~impl T { #[clock(domain = sys)] fn tick(self, go: u1) -> T { T { p: if go == 1 { Phase::Busy } else { self.p } } } }
```

An enum value is one of its variants. Variants carry no data. Enums compare
with `==` and `!=`, and `match` selects on them.

The **encoding** is how the variants are stored:

- **binary** (the default): variant *k* in declaration order is the number
  *k*, in the fewest bits that hold the largest.
- **`#[encoding(onehot)]`**: one bit per variant, with variant *k* setting
  bit *k*.
- **explicit discriminants** (`Load = 0b0000011`): each variant is the value
  given, in the fewest bits that hold the largest. Use them when an encoding
  is fixed by something outside the design, such as an instruction set.

The encoding decides the width of an enum port in the generated Verilog. The
choice never changes what a design computes, only how it is stored.

## Structs

A struct type is a named group of fields (see
[Items](items.md#structs)). Two struct types with the same fields are still
different types. A struct value can't be compared with `==`; compare its
fields.

## Tuples

`(u8, u1)` is a tuple type: an anonymous group of values. Tuples are how
functions return several values. A tuple is built with `(a, b)` and taken
apart with a `let` pattern:

```sinter,ignore
let (next, carry) = self.digit.step(cmd);
```

There is no `.0` field access, and tuples can't be matched in a `match`.

## Arrays

`[T; N]` is `N` values of type `T`, numbered from 0. `N` is a literal or a
constant.

```sinter
const TAPS: u8 = 4;

struct Shift {
    pub taps: [u8; TAPS] = [0; TAPS],
}

impl Shift {
    #[clock(domain = sys)]
    fn tick(self, x: u8) -> Shift {
        Shift { taps: [x, self.taps[0], self.taps[1], self.taps[2]] }
    }
}
```

- `[a, b, c]` lists the elements, and `[v; N]` repeats `v` `N` times.
- `a[i]` reads an element. With a constant index it is just a wire; with a
  runtime index it is a multiplexer.
- `a.set(i, v)` is a copy of `a` with element `i` replaced.

An array field of a state struct is a bank of registers, all readable at
once. For large or deep storage use a [memory](storage.md) instead. Arrays
can't be compared with `==`.

## `Option`

`Option<T>` is either `Some(value)` or `None`. It is stored as a valid bit and
a `T`, and is how Sinter represents "maybe a value": a bus beat, the head of a
queue, a lookup that can miss. See [Option and `?`](option.md).

## Built-in stateful types

These types are only for fields of state structs. Each is used through
methods, and each is explained on its own page.

| Type | What it is | Page |
|---|---|---|
| `mem<T, N>`, `mem<T, N, R>` | block RAM, `N` words, `R` read ports | [Storage](storage.md) |
| `rom<T, N>` | read-only memory with contents fixed at compile time | [Storage](storage.md) |
| `fifo<T, N>` | lossless queue | [Channels](channels.md) |
| `ring<T, N>` | queue that overwrites its oldest entry when full | [Channels](channels.md) |
| `flow<T>` | single slot holding the newest value | [Channels](channels.md) |
| `Sender<T, N>`, `Receiver<T, N>`, `Push<T>` | views of a `fifo`, and a push request | [Channels](channels.md) |
| `sync<A, B>`, `pulse<A, B>` | single-bit clock-domain crossings | [Clock domains](clock-domains.md) |
| `mailbox<T, A, B>`, `async_fifo<T, N, A, B>` | multi-bit clock-domain crossings | [Clock domains](clock-domains.md) |

A `pipeline fn` or `multiplex fn` also names a stateful type: a field whose
type is the function's name holds its registers. See
[Pipelines and multiplexing](staged.md).
