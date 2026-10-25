# Stimulus files

`sinter sim --stimulus FILE` drives a design from a CSV file instead of random
inputs, and checks expected outputs. Each data row is one clock cycle (one
edge time, with [several clocks](../reference/clock-domains.md#in-simulation)).

```text
# A comment. Blank lines are ignored too.
en, expect.value_q
1,  0
1,  1
0,  2      # hold
1,  -      # don't check this cycle
```

## Columns

The first non-comment line is the header. Each column is either:

- an **input** port name: the value to drive that cycle, or
- `expect.` followed by an **output** port name: the value the output must
  have that cycle.

The port names are the flattened names in the generated Verilog, as printed
in the header of `sinter sim`'s output table: a parameter `p: Pair` gives
`p_lo` and `p_hi`, an `Option` gives `x_is_some` and `x_payload`, a `pub`
register gives `name_q`, and an extra output `out`. Inputs without a column
are driven 0 (`sinter sim` lists them), and columns may come in any order.

## Values

- Decimal, `0x` hexadecimal, or `0b` binary, with optional `_` separators.
- A negative decimal value is allowed only for a signed port.
- Every value must fit its port.
- A fixed-point port takes its raw bits: for an `i4.4`, `0x28` is 2.5.
- An enum port takes its encoded value (the first variant is 0 by default).
- In an `expect.` column, an empty cell or `-` means "don't check".

## Timing

Each row's inputs are applied for that cycle, and its expectations are
compared with the outputs during that cycle, before the clock edge. A `pub`
register's `_q` output shows its value *before* the edge, so it reflects
inputs from earlier rows; an extra output can react to the same row's inputs.

## Results

Every failed expectation is logged with its cycle and the CSV line, and
`sinter sim` exits with status 1 after the run. A file that passes prints
`all N expectation(s) over M cycle(s) passed`. Stimulus files make good
regression tests: keep them beside the design and run them in CI.
