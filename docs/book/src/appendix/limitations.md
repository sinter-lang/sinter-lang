# Limitations

What Sinter doesn't do yet, gathered in one place. Most of these are planned.

## Language

- Values are at most 64 bits wide.
- No division or remainder operator.
- Enum variants carry no data, and there are no user-defined generic types.
- Enum encodings are binary, one-hot, or explicit; no Gray or Johnson codes.
- Fixed-point constants must be literals (integer constants can be
  computed).
- `impl` blocks are for structs, and every method takes `self`: there are no
  enum methods or associated functions.
- `debug_assert!` behaves exactly like `assert!`.
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
- No elastic, ready/valid `Stream<T>` with per-stage backpressure; a
  pipeline can only stall as a whole.
- Pipelines and multiplex functions have fixed latency.

## Hierarchy

- A `#[module]`'s outputs are its step's `Out` struct; its parent can't read
  its state or call its other methods.
- `sinter build --faults` doesn't support designs with `#[module]`
  instances.

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
