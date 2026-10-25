# Items and modules

A source file is a sequence of **items**: constants, functions, structs,
enums, `impl` blocks, modules, and `use` declarations. Items can appear in
any order and refer to each other regardless of order.

## Constants

```sinter
const DEPTH: u8 = 16;
const IDLE_LEVEL: u1 = 1;
pub const BAUD_DIV: u16 = 217;
const ALIAS: u16 = BAUD_DIV;
```

A constant has a name, a type, and a value. The value must be a literal or
the name of another constant; constants can't do arithmetic yet. A constant
can be used anywhere an expression can, and as an array length, repeat count,
memory depth, or bit-slice bound.

## Functions

```sinter
// The larger of two values.
fn max(a: u8, b: u8) -> u8 {
    if a > b { a } else { b }
}

// The number of set bits in a nibble.
fn popcount4(x: u4) -> u3 {
    let a = x[0:0] + x[1:1];
    let b = x[2:2] + x[3:3];
    a + b
}
```

A function takes typed parameters and returns one value; return several by
returning a tuple or struct. The body is a [block](expressions.md#blocks): any
number of `let` bindings, then the expression that is the result. There is no
`return`.

Functions are **combinational**. Every call is inlined at its call site, so a
function costs exactly the logic in its body, once per call. A function can't
call itself, directly or indirectly.

A function marked `pipeline` or `multiplex` is a different thing: a
multi-cycle computation compiled to its own state. See
[Pipelines and multiplexing](staged.md).

## Structs

```sinter
struct Pixel {
    r: u5,
    g: u6,
    b: u5,
}

struct Counter {
    pub value: u8 = 0,
    wrapped: u1 = 0,
}
```

A struct groups named fields. Each field has a type and, optionally, a
**reset value** after `=`. Reset values matter when the struct is used as
[state](state.md): every register must have one, but a field whose type is a
struct gets its reset values from that struct, and an `Option` field with no
reset value resets to `None`. A reset value must be a constant expression:
literals, constants, and array, struct, `Some`, and `None` expressions built
from them.

A struct used only as a value (a function's parameter or result, like
`Pixel`) needs no reset values.

A struct value is built with every field named, in any order:

```sinter,ignore
Pixel { r: 31, g: 0, b: 0 }
```

There is no shorthand for "the same as before except": write each field. A
field is read with `.`: `p.r`.

## Enums

```sinter
enum Phase {
    Idle,
    Start,
    Data,
    Stop,
}

#[encoding(onehot)]
enum Grant {
    A,
    B,
    C,
}

// Explicit discriminants, for an encoding fixed by an external protocol.
enum Opcode {
    Load = 0b0000011,
    Store = 0b0100011,
    Branch = 0b1100011,
}
```

An enum is a value that is exactly one of its variants, written
`Phase::Idle`. Variants carry no data. See
[Types](types.md#enums) for encodings.

## `impl` blocks and methods

```sinter
struct Edge {
    prev: u1 = 0,
}

impl Edge {
    // High for one cycle when `level` goes from 0 to 1.
    fn rising(self, level: u1) -> u1 {
        level & !self.prev
    }

    // Remember this cycle's level.
    fn step(self, level: u1) -> Edge {
        Edge { prev: level }
    }
}
```

An `impl` block defines methods on a struct. A method's first parameter is
`self`, the value it is called on, and it is called with `.`:
`self.edge.rising(x)`. Methods, like functions, are pure and inlined: `step`
doesn't change `self`, it returns a new `Edge`.

An `impl` must be in the same module as its struct. Every method must take
`self`; functions that don't belong at module level. One method may be marked
`#[clock]`, which makes it the design's clock root (see
[State and the clock](state.md)).

## Modules

A design can be split across files. The file you pass to `sinter` is the
**root module**. `mod name;` declares a child module, found in `name.snt`
next to the declaring file; that module's own children go in a directory
named after it:

```text
main.snt            the root: `mod display;`
display.snt         `mod font;`
display/font.snt
```

There is no `mod.snt`. A small module can also be written inline:

```sinter
mod util {
    pub fn double(x: u4) -> u5 {
        x + x
    }
}
~struct Top { pub v: u5 = 0 }
~impl Top { #[clock(domain = sys)] fn tick(self, x: u4) -> Top { Top { v: self::util::double(x) } } }
```

### Paths

Items in other modules are named with `::` paths that start with one of:

- `root::` — the root module,
- `self::` — the current module,
- `super::` — the parent module.

`use` brings a name into scope, so it can be used unqualified. The path in a
`use` may also start with a child module's name, meaning a path from the
root. `*` imports every public item of a module:

```sinter,project=modules,file=main.snt
mod display;

use root::display::Seg;
use self::display::*;

struct Top {
    pub seg: Seg,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self, digit: u4) -> Top {
        Top { seg: self.seg.show(digit) }
    }
}
```

```sinter,project=modules,file=display.snt
mod font;

pub const BLANK: u7 = 0;

pub struct Seg {
    pub bits: u7 = 0,
}

impl Seg {
    pub fn show(self, digit: u4) -> Seg {
        Seg { bits: self::font::glyph(digit) }
    }
}
```

```sinter,project=modules,file=display/font.snt
pub fn glyph(digit: u4) -> u7 {
    match digit {
        0 => 0b0111111,
        1 => 0b0000110,
        _ => super::BLANK,
    }
}
```

In an expression, a path to a function or constant in another module must
start with `root`, `self`, or `super` (or be imported with `use`). Enum
variants are always written `Enum::Variant`.

### Visibility

Everything is private unless marked `pub`: items, struct fields, and methods.
A private item can be used in the module that defines it and in that module's
descendants. `pub` makes it usable everywhere.

```sinter,project=private,fail,file=main.snt
mod display;

use root::display::Seg;

struct Top {
    pub seg: Seg,
    pub raw: u7 = 0,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self) -> Top {
        Top { seg: self.seg, raw: self.seg.bits }
    }
}
```

```sinter,project=private,file=display.snt
pub struct Seg {
    bits: u7 = 0,
}
```

```text
error: field `bits` of struct `display::Seg` is private
```

The `#[clock]` root must be in the root module. Splitting a design into
modules never changes the hardware: the compiler resolves every name and
builds the same single module as if everything were in one file.
