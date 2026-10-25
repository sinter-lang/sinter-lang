# Clock domains

A design can run on several clocks. It is still one state struct and one
Verilog module, with one clock root per clock **domain**. Every register
belongs to one domain, and the compiler checks that no domain reads another's
registers except through a synchronizer.

```sinter
struct Clocks {
    #[domain(fast)]
    pub fast_count: u8 = 0,
    #[domain(slow)]
    pub slow_sum: u8 = 0,
}

impl Clocks {
    #[clock(domain = fast, period = 3)]
    fn tick_fast(self, fast_en: u1) -> (Clocks, u8) {
        let count = if fast_en == 1 { (self.fast_count + 1) as u8 wrap } else { self.fast_count };
        (Clocks { fast_count: count, slow_sum: self.slow_sum }, count)
    }

    #[clock(domain = slow, period = 7)]
    fn tick_slow(self, slow_add: u4) -> Clocks {
        Clocks { fast_count: self.fast_count, slow_sum: (self.slow_sum + slow_add) as u8 wrap }
    }
}
```

- `#[domain(d)]` puts a field, and every register in it, in domain `d`.
- Each domain has one clock root, `#[clock(domain = d, period = p)]`. Its
  parameters are inputs in that domain and its extra outputs are outputs in
  that domain.
- `period` is the clock period in abstract time units, used by the
  simulators to interleave the clocks. With two or more domains, every clock
  root needs one. All clocks start at time 0.

A design with a single clock root needs no `#[domain]` tags: everything is in
that one domain.

## The domain rule

A domain's clock root may read only its own domain's registers, and must pass
every other domain's fields through unchanged (`slow_sum: self.slow_sum`
above). Breaking either rule is an error:

```sinter,fail
struct Clocks {
    #[domain(fast)]
    pub count: u8 = 0,
    #[domain(slow)]
    pub copy: u8 = 0,
}

impl Clocks {
    #[clock(domain = fast, period = 2)]
    fn tick_fast(self) -> Clocks {
        Clocks { count: (self.count + 1) as u8 wrap, copy: self.copy }
    }

    #[clock(domain = slow, period = 5)]
    fn tick_slow(self) -> Clocks {
        Clocks { count: self.count, copy: self.count }
    }
}
```

```text
error: `tick_slow` (domain `slow`) reads `count`, which belongs to domain `fast`; cross clock domains through a CDC primitive (`sync`, `pulse`, `mailbox`, `async_fifo`)
```

The rule is checked after every function is inlined, so moving the read into
a helper function doesn't hide it.

## Crossing domains

Data crosses between domains only through one of four built-in types. Each is
a state field (with no `#[domain]` tag and no reset value) whose type names
its source and destination domains:

| Type | Carries | Source side | Destination side |
|---|---|---|---|
| `sync<A, B>` | a level, one bit | `send(v)` | `recv()`, `value()` |
| `pulse<A, B>` | one-cycle events | `send(fire)` | `recv()`, `value()` |
| `mailbox<T, A, B>` | one word at a time | `sender()`, `send(push)` | `receiver()`, `recv(pop)` |
| `async_fifo<T, N, A, B>` | a stream of words | `sender()`, `send(push)` | `receiver()`, `recv(pop)` |

Each side has a step method (`send` or `recv`) that **must be called exactly
once per cycle** by that side's clock root, outside any `if` or `match` arm,
with its result put in the next state. The compiler rejects a missing,
repeated, or conditional call, so a synchronizer can't be left out.

- **`sync`** is a source register followed by a two-flop synchronizer. Use it
  for slowly changing levels. `value()` in the destination is the level a few
  destination cycles later.
- **`pulse`** turns each one-cycle `fire` in the source into a one-cycle
  `value()` in the destination (it toggles a level and detects the toggle).
  Events closer together than about three destination cycles may merge.
- **`mailbox`** moves one word at a time with a request/acknowledge
  handshake. The source pushes through a `Sender` (as for a
  [`fifo`](channels.md#pushing-through-a-sender)), which is full until the
  previous word has been taken; the destination sees it at `front()` of its
  `Receiver` and pops it with `recv(1)`.
- **`async_fifo`** is a dual-clock FIFO with `N` entries (`N` a power of two,
  at least 2): block RAM written in the source domain and read in the
  destination, with gray-coded pointers crossing between them. Its interface
  is the mailbox's, and `front()` shows the next word without extra latency,
  so a consumer that pops every cycle takes a word every cycle.

```sinter
struct Link {
    #[domain(fast)]
    pub next_word: u8 = 1,
    #[domain(slow)]
    pub last: u8 = 0,
    q: async_fifo<u8, 16, fast, slow>,
}

impl Link {
    #[clock(domain = fast, period = 3)]
    fn tick_fast(self, go: u1) -> Link {
        let tx = self.q.sender();
        let push = if go == 1 { tx.try_push(self.next_word) } else { tx.no_push() };
        let sent = go & !tx.is_full();
        Link {
            next_word: if sent { (self.next_word + 1) as u8 wrap } else { self.next_word },
            last: self.last,
            q: self.q.send(push),
        }
    }

    #[clock(domain = slow, period = 4)]
    fn tick_slow(self, take: u1) -> Link {
        let last = match self.q.receiver().front() {
            Some(w) => if take == 1 { w } else { self.last },
            None => self.last,
        };
        Link { next_word: self.next_word, last: last, q: self.q.recv(take) }
    }
}
```

The simulators don't model metastability: a synchronizer is a fixed delay of
destination clock edges, the same in simulation and in the Verilog.

## Reset

The module has a single `rst_n` input. With several domains, the compiler adds
a reset synchronizer per domain (asynchronous assert, release two edges later
on that domain's clock), so drive `rst_n` directly.

## In the generated Verilog

A multi-domain module has a clock input per domain, `clk_<domain>`, in place
of `clk`, and each domain's extra outputs are named `<domain>_out`
(`fast_out`, `fast_out_0`, ...). Each register is clocked by its domain's
clock.

## In simulation

The simulator advances from one clock edge to the next. At each edge time,
every domain with an edge computes its next state from the same current
state, and they all update together. One row of `sinter sim` output (and of a
stimulus file) is one edge time, and shows which domains ticked:

```text
 step |  time | ticks | fast_en slow_add || fast_count_q slow_sum_q fast_out
    0 |     0 | fast,slow | 1 0 || 0 0 1
    1 |     3 | fast  | 1 0 || 1 0 2
    2 |     6 | fast  | 1 0 || 2 0 3
    3 |     7 | slow  | 1 7 || 3 0 4
```

A row's inputs are used only by the domains that tick at that time.

Clocks have fixed integer periods: derived clocks, phase relationships, and
clock gating aren't supported, and `pipeline` and `multiplex` functions work
only in single-clock designs.
