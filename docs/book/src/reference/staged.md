# Pipelines and multiplexing

An ordinary function finishes within one cycle. A **staged function** is
spread over several cycles, with `yield;` marking where one cycle ends and
the next begins. Sinter has two kinds, the two ends of the space-time trade:

- a `pipeline fn` replicates its stages in space: every stage works on a
  different input every cycle, so it accepts one input per cycle.
- a `multiplex fn` folds its stages in time: one input at a time passes
  through all the phases, reusing a single multiplier.

The compiler adds the registers for you: every value that is still needed
after a `yield` is stored in a register at that point.

## Pipelines

```sinter
// (a * b) + c in two stages.
pipeline fn mac(a: u8, b: u8, c: u16) -> u16 {
    let prod: u16 = a * b;
    yield;
    let sum = (prod + c) as u16 wrap;
    yield;
    sum
}

struct Top {
    mac: mac,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self, a: u8, b: u8, c: u16, valid: u1) -> (Top, (u16, u1)) {
        let (mac, result, result_valid) = self.mac.advance(a, b, c, valid);
        (Top { mac: mac }, (result, result_valid))
    }
}
```

A `pipeline fn` isn't called like a function. Its name is a type: a state
field of that type (`mac: mac`) holds the pipeline's registers, with no reset
value needed. Each cycle, the clock root calls the field's `advance` method
once and puts the returned pipeline in the next state:

```sinter,ignore
self.mac.advance(a, b, c, in_valid) -> (mac, result, result_valid)
```

- The arguments are the function's parameters, then `in_valid`, a `u1` that
  says whether this cycle's inputs are real.
- `result` is the function's value for the inputs given `L` cycles ago, where
  `L` is the number of `yield`s (the latency). `result_valid` is the
  `in_valid` from then.

Every stage runs every cycle, so a pipeline accepts a new input each cycle. A
cycle with `in_valid = 0` is a *bubble*: its result comes out `L` cycles later
with `result_valid = 0`. After reset the pipeline is empty, so the first `L`
results are invalid.

Rules:

- A pipeline needs at least one `yield`, and `yield` must be at the top level
  of the body, not inside an `if` or a block.
- A value carried across a `yield` needs an explicit type, through a `let`
  annotation or a cast (`let prod: u16 = a * b;`), because its type is a
  register's width.
- `let` bindings in a pipeline are plain names (no tuple patterns), and a
  name can't be reused across stages.
- A pipeline takes no `self`: it sees only its inputs, constants, and its own
  values, so stages never interfere with each other.
- `in_valid` is reserved as a parameter name.

### Stalling

To freeze a pipeline, keep its old value instead of the advanced one. Every
pipeline register then holds, and nothing is lost:

```sinter,ignore
let (next, result, ok) = self.mac.advance(a, b, c, 1);
if enable == 1 {
    Top { mac: next, y: result, valid: ok }
} else {
    Top { mac: self.mac, y: self.y, valid: self.valid }
}
```

## Multiplexing

```sinter
// a0*b0 + a1*b1 over two cycles, on one multiplier.
multiplex fn dot2(a0: u8, b0: u8, a1: u8, b1: u8) -> u17 {
    let p = (a0 * b0) as u17 wrap;
    yield;
    (p + (a1 * b1)) as u17 wrap
}

struct Top {
    dot: dot2,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self, a0: u8, b0: u8, a1: u8, b1: u8) -> (Top, (u17, u1)) {
        let (dot, result, valid) = self.dot.advance(a0, b0, a1, b1, 1);
        (Top { dot: dot }, (result, valid))
    }
}
```

A `multiplex fn` with `N` `yield`s has `N + 1` phases, and runs one phase
per cycle in a loop. It is declared and driven exactly like a pipeline, but
it works on one input at a time:

- In phase 0 it takes all of its inputs. Inputs in other cycles are ignored.
- In the last phase, `N` cycles later, its result is available and `valid` is
  1. The next cycle is phase 0 again.

So it accepts one input every `N + 1` cycles, and the cycle after `valid` is
the one that takes the next input. The phase counter runs continuously from
reset; the `in_valid` argument is required but currently ignored.

Each phase may contain at most one `*`. All the multiplies share **one**
multiplier, with its operands selected by phase: `dot2` uses one 8×8
multiplier where a plain function would use two. That is the point of a
`multiplex fn`: an expensive resource, used once per phase, with the sharing
visible in the source.

The same rules as for pipelines apply to `yield` placement, and `ph`,
`shared`, and `in_valid` are reserved names.

## Outputs

As with channels, a `pub` field holding a staged function exposes its internal
registers as ports, with compiler-chosen names. Route the results you need
through an extra output or a `pub` register instead.
