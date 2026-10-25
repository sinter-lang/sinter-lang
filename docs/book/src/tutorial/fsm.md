# A state machine

Now the transmitter itself. It is a state machine with four phases (idle,
start bit, data bits, stop bit), a shift register holding the byte being
sent, a count of the data bits sent so far, and the baud counter from the
last chapter. Here is the whole design; the rest of the chapter goes through
it.

```sinter
const BIT_CYCLES: u8 = 2;

// ---- Baud-rate counter --------------------------------------------------

struct Baud {
    count: u8 = 0,
}

impl Baud {
    // Count while `run` is high. The second result is 1 on the last cycle of
    // each bit period.
    fn step(self, run: u1) -> (Baud, u1) {
        let next = (self.count + 1) as u8 wrap;
        if run == 0 {
            (Baud { count: 0 }, 0)
        } else if next == BIT_CYCLES {
            (Baud { count: 0 }, 1)
        } else {
            (Baud { count: next }, 0)
        }
    }
}

// ---- Transmitter --------------------------------------------------------

enum Phase {
    Idle,
    Start,
    Data,
    Stop,
}

struct UartTx {
    phase: Phase = Phase::Idle,
    shift: u8 = 0,
    bit: u3 = 0,
    baud: Baud,
}

impl UartTx {
    // Whether a frame is being sent.
    fn busy(self) -> u1 {
        self.phase != Phase::Idle
    }

    // The serial line level for the current phase.
    fn line(self) -> u1 {
        match self.phase {
            Phase::Start => 0,
            Phase::Data => self.shift[0:0],
            _ => 1,
        }
    }

    // One clock cycle. When idle, a high `start` loads `data` and begins a
    // frame.
    fn step(self, start: u1, data: u8) -> UartTx {
        let (baud, done) = self.baud.step(self.busy());
        let load = self.phase == Phase::Idle & start == 1;
        let shift_out = self.phase == Phase::Data & done == 1;
        let phase = match self.phase {
            Phase::Idle => if start == 1 { Phase::Start } else { Phase::Idle },
            Phase::Start => if done == 1 { Phase::Data } else { Phase::Start },
            Phase::Data => if shift_out & self.bit == 7 { Phase::Stop } else { Phase::Data },
            Phase::Stop => if done == 1 { Phase::Idle } else { Phase::Stop },
        };
        UartTx {
            phase: phase,
            shift: if load { data } else if shift_out { 0_u1 @ self.shift[7:1] } else { self.shift },
            bit: if shift_out { (self.bit + 1) as u3 wrap } else if load { 0 } else { self.bit },
            baud: baud,
        }
    }
}

// ---- Top level ----------------------------------------------------------

struct Top {
    tx: UartTx,
    pub line: u1 = 1,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self, start: u1, data: u8) -> (Top, u1) {
        let tx = self.tx.step(start, data);
        (Top { tx: tx, line: tx.line() }, self.tx.busy())
    }
}
```

## Enums and `match`

`Phase` is an enum. The compiler picks its encoding (binary by default, so
two bits here), and `#[encoding(onehot)]` or explicit values choose another;
see [Types](../reference/types.md#enums). Enum values compare with `==` and
`!=`, and `match` selects on them:

```sinter,ignore
let phase = match self.phase {
    Phase::Idle => if start == 1 { Phase::Start } else { Phase::Idle },
    Phase::Start => if done == 1 { Phase::Data } else { Phase::Start },
    ...
};
```

A `match` must cover every case. Leave out `Phase::Stop` and the compiler says
`non-exhaustive match on 'Phase': missing Stop`. `_` matches anything, as in
`line`. In hardware, a `match` is a multiplexer: every arm is computed and the
scrutinee picks one.

## Nested state

`UartTx` holds a `Baud`, and `Top` holds a `UartTx`. A field whose type is a
struct is a group of registers; nesting gives each part of the design its own
state and methods, and the compiler flattens it all into one module (the
counter's register appears in the Verilog as `tx_baud_count`).

A struct field like `baud: Baud` needs no reset value: each of `Baud`'s own
fields has one.

## Methods

`busy`, `line`, and `step` are methods: functions in an `impl` block whose
first parameter is `self`. They are pure. Calling `self.baud.step(...)`
doesn't change anything; it returns a new `Baud`, and that value becomes part
of the next state only because `step` puts it in the `UartTx` it returns.

A method that returns `(Baud, u1)` produces a next state and an output in the
same cycle. `done` is combinational: it is high during the last cycle of a bit
period, not the cycle after.

Calls are inlined, so a method is not a sub-module: it costs exactly the
logic in its body, once per call.

## Bit slices and concatenation

The data bits go out least significant first. `self.shift[0:0]` is bit 0 of
`shift`, as a `u1`. After each bit, the shift register moves right:

```sinter,ignore
0_u1 @ self.shift[7:1]
```

`self.shift[7:1]` is the top seven bits, a `u7`, and `@` concatenates: `0_u1`
(a one-bit zero) on top, the seven bits below, for a `u8` again. You could
also write `(self.shift >> 1) as u8 wrap`: shifting a `u8` right by a constant
1 gives a `u7`, and the cast widens it back.

## Precedence of `&`

`&` binds more loosely than `==` in Sinter (unlike Rust), so
`self.phase == Phase::Idle & start == 1` means
`(self.phase == Phase::Idle) & (start == 1)`. Comparisons produce a `u1`, and
`&` of two `u1`s is a logical AND.

## Outputs

`Top::tick` returns `(Top, u1)`. The first element is the next state; the
second is an extra output, `busy`, which becomes the `out` port. Unlike the
`_q` ports of `pub` registers, an extra output is combinational: it reflects
the current state and inputs within the same cycle.

`line` is a register, so the serial output comes straight from a flip-flop
with no glitches. It resets to 1, the idle level.

## Run it

```sh
sinter sim uart.snt --cycles 30
```

Random inputs make `start` high often, so frames go out back to back. The
next chapter drives it with chosen inputs and checks the line.
