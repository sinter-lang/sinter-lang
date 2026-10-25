# Buffering with a FIFO

The transmitter accepts a byte only when it is idle, and a frame takes 10 bit
periods. Whatever produces the bytes would rather hand them over when it has
them. This chapter puts a 4-entry FIFO in front of the transmitter.

Replace `Top` with this (`Baud`, `Phase`, and `UartTx` stay as they were):

```sinter
~const BIT_CYCLES: u8 = 2;
~struct Baud {
~    count: u8 = 0,
~}
~impl Baud {
~    fn step(self, run: u1) -> (Baud, u1) {
~        let next = (self.count + 1) as u8 wrap;
~        if run == 0 {
~            (Baud { count: 0 }, 0)
~        } else if next == BIT_CYCLES {
~            (Baud { count: 0 }, 1)
~        } else {
~            (Baud { count: next }, 0)
~        }
~    }
~}
~enum Phase {
~    Idle,
~    Start,
~    Data,
~    Stop,
~}
~struct UartTx {
~    phase: Phase = Phase::Idle,
~    shift: u8 = 0,
~    bit: u3 = 0,
~    baud: Baud,
~}
~impl UartTx {
~    fn busy(self) -> u1 {
~        self.phase != Phase::Idle
~    }
~    fn line(self) -> u1 {
~        match self.phase {
~            Phase::Start => 0,
~            Phase::Data => self.shift[0:0],
~            _ => 1,
~        }
~    }
~    fn step(self, start: u1, data: u8) -> UartTx {
~        let (baud, done) = self.baud.step(self.busy());
~        let load = self.phase == Phase::Idle & start == 1;
~        let shift_out = self.phase == Phase::Data & done == 1;
~        let phase = match self.phase {
~            Phase::Idle => if start == 1 { Phase::Start } else { Phase::Idle },
~            Phase::Start => if done == 1 { Phase::Data } else { Phase::Start },
~            Phase::Data => if shift_out & self.bit == 7 { Phase::Stop } else { Phase::Data },
~            Phase::Stop => if done == 1 { Phase::Idle } else { Phase::Stop },
~        };
~        UartTx {
~            phase: phase,
~            shift: if load { data } else if shift_out { 0_u1 @ self.shift[7:1] } else { self.shift },
~            bit: if shift_out { (self.bit + 1) as u3 wrap } else if load { 0 } else { self.bit },
~            baud: baud,
~        }
~    }
~}
struct Top {
    q: fifo<u8, 4>,
    tx: UartTx,
    pub line: u1 = 1,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self, incoming: Option<u8>) -> (Top, u1) {
        // Queue the incoming byte, if there is one and there is room.
        let push = self.q.sender().offer(incoming);

        // Start the next frame when the transmitter is idle and a byte waits.
        let start = self.tx.busy() == 0 & self.q.is_empty() == 0;
        let data: u8 = match self.q.front() {
            Some(byte) => byte,
            None => 0,
        };
        let tx = self.tx.step(start, data);

        (Top { q: self.q.next(push, start), tx: tx, line: tx.line() }, self.q.is_full())
    }
}
```

## `Option`

`incoming` is an `Option<u8>`: either `Some(byte)` or `None`. In hardware it
is a valid bit and an 8-bit payload, and its ports are `incoming_is_some` and
`incoming_payload`. `None` is a cycle with nothing to send.

`self.q.front()` is an `Option<u8>` too: the oldest byte, or `None` when the
queue is empty. `match` takes it apart. The arms are a binding (`byte`) and a
bare literal (`0`), so the `let` gives the type: a literal has no type of its
own, and here the compiler has nothing to take one from.

See [Option and `?`](../reference/option.md).

## The FIFO

`fifo<u8, 4>` is a queue of four bytes. Like any state, it is a field, and the
design threads it: `self.q.next(push, pop)` returns the queue as it will be
after this cycle's push and pop, and that value goes into the next `Top`.

You can't push a plain value into a `fifo`. A push is a `Push<u8>` token, and
only the queue's `Sender` makes one: `sender().offer(incoming)` pushes
`incoming` if there is room and otherwise pushes nothing. So the queue can
never be overfilled; the type system rules it out. Here the extra output,
`self.q.is_full()`, tells the producer when a byte would be refused.

Pops are plain `u1` flags: `start` both begins a frame and pops the byte it
loads. Popping an empty queue does nothing.

There are other kinds of queue: a `ring` overwrites its oldest entry when
full, and a `flow` keeps just the newest value. See
[Channels](../reference/channels.md).

## Try it

Send three bytes back to back:

```text
incoming_is_some, incoming_payload
1, 0x01
1, 0x02
1, 0x03
```

Add 60 rows of `0, 0` and run it with `--stimulus`. The three frames go out
one after another on `line_q`, with one idle cycle between them.
