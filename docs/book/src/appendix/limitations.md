# Limitations

What Sinter doesn't do yet, gathered in one place. Most of these are planned.

## Language

- Values are at most 64 bits wide.
- No division or remainder operator.
- Enum variants carry no data, and there are no user-defined generic types.
- `match` patterns can't be tuples or nested (`Some(Some(x))`).
- Constants are literals or other constants; constant arithmetic isn't
  supported.
- Struct values must list every field; there is no update syntax
  (`..self`).
- There is no `Self` type: write the struct's name.
- `impl` blocks are for structs. Every method takes `self`.
- `#[encoding(gray)]` and `#[encoding(johnson)]` produce the binary
  encoding.
- `debug_assert!` behaves exactly like `assert!`.
- Unknown attributes are ignored rather than reported.
- Some errors, such as an untyped literal, are only found in code the clock
  root reaches; a function nothing calls is checked less thoroughly.

## Modules

- `pub use` doesn't re-export a name.
- Modules have no visibility of their own: a private module's `pub` items
  are reachable from anywhere.
- There are no packages or external dependencies.

## Memories, channels, and staged functions

- Memory, ROM, and channel elements are integers or fixed-point values, not
  structs, enums, or `Option`s.
- No `include_bin!`, and a ROM's depth must be written out.
- A `multiplex fn` ignores its `in_valid` argument and runs continuously.
- No elastic, ready/valid `Stream<T>` with per-stage backpressure; a
  pipeline can only stall as a whole.
- Pipelines and multiplex functions have fixed latency.

## Clocks and hardware

- Clocks have fixed integer periods: no derived or related clocks, no clock
  gating.
- `pipeline` and `multiplex` functions, the fault latch (`--faults`), and
  pin maps work only in single-clock designs.
- Foreign Verilog bindings have scalar ports only, and can't override Verilog
  parameters. Clock-generating modules (PLLs) belong in a hand-written top
  level.

## Tools

- No language server; editors get syntax highlighting.
- The parser stops at the first syntax error.
