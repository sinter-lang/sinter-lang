# Channels

A channel is a fixed-capacity queue held in a state field. There are three,
and the type says what happens when a queue is full:

| Type | Capacity | When full |
|---|---|---|
| `fifo<T, N>` | `N` | refuses the push: nothing is ever lost |
| `ring<T, N>` | `N` | drops the oldest entry to make room |
| `flow<T>` | 1 | the new value replaces the old one |

`T` is an integer or fixed-point type, and `N` is a power of two, at least 2.
A channel field has no reset value: it resets empty.

## `fifo`

```sinter
struct Queue {
    q: fifo<u8, 4>,
}

impl Queue {
    #[clock(domain = sys)]
    fn tick(self, incoming: Option<u8>, take: u1) -> (Queue, Option<u8>) {
        let push = self.q.sender().offer(incoming);
        let head = self.q.front();
        (Queue { q: self.q.next(push, take) }, head)
    }
}
```

A `fifo` has these methods:

| Method | Result |
|---|---|
| `front()` | `Option<T>`: the oldest entry, or `None` if empty |
| `is_empty()`, `is_full()` | `u1` |
| `len()` | the number of entries, a `uK` that holds 0 to `N` |
| `sender()`, `receiver()` | the queue's two views (below) |
| `next(push, pop)` | the queue after this cycle |

`next(push, pop)` is the queue's step: it adds the pushed value, if any, and
removes the oldest entry if `pop` is 1 (popping an empty queue does nothing).
Its result goes into the next state, like a memory's. Call it once per
cycle, outside any `if` or `match` arm, and control it through `push` and
`pop`. If a cycle both pushes and pops, `front()` that cycle is the old head,
and it is the one popped.

### Pushing through a `Sender`

`push` isn't a plain value; it is a `Push<T>`, and the only way to make one is
from the queue's `Sender`, which won't push into a full queue:

| `Sender` method | Result |
|---|---|
| `try_push(v)` | push `v` if there is room, otherwise nothing |
| `offer(o)` | for an `Option<T>`: push its value if it has one and there is room |
| `no_push()` | push nothing |
| `is_full()`, `len()` | as on the queue |

So a lossless queue really is lossless: overfilling it is a type error, not a
runtime check. The producer sees `is_full()` to know whether its value was
taken.

A `Receiver` has `front()`, `is_empty()`, and `len()`. Both views are plain
values, so you can pass them to functions, including functions in other
modules, that should only produce or only consume:

```sinter
struct Queue {
    q: fifo<u8, 4>,
}

// The producer can only push, and only through its Sender.
fn produce(tx: Sender<u8, 4>, incoming: Option<u8>) -> Push<u8> {
    tx.offer(incoming)
}

// The consumer pops when asked and the queue isn't empty.
fn consume(rx: Receiver<u8, 4>, take: u1) -> u1 {
    take & !rx.is_empty()
}

impl Queue {
    #[clock(domain = sys)]
    fn tick(self, incoming: Option<u8>, take: u1) -> (Queue, Option<u8>) {
        let push = produce(self.q.sender(), incoming);
        let pop = consume(self.q.receiver(), take);
        (Queue { q: self.q.next(push, pop) }, self.q.front())
    }
}
```

### Storage

A `fifo` with 64 or more entries is built from block RAM; a smaller one from
registers. `#[storage(regs)]` or `#[storage(bram)]` on the field overrides the
choice. The two behave identically, cycle for cycle: `front()` is available in
the same cycle either way.

```sinter,ignore
#[storage(bram)]
q: fifo<u32, 16>,
```

## `ring`

A `ring` has the same methods as a `fifo` except the views: its `next` takes
the push as a plain `Option<T>`, because dropping data is its contract. A push
into a full ring discards the oldest entry. Use it where the newest data
matters most, such as a trace buffer:

```sinter
struct Trace {
    r: ring<u8, 4>,
}

impl Trace {
    #[clock(domain = sys)]
    fn tick(self, sample: Option<u8>, take: u1) -> (Trace, Option<u8>) {
        (Trace { r: self.r.next(sample, take) }, self.r.front())
    }
}
```

## `flow`

A `flow<T>` holds at most one value: the latest one pushed. `value()` returns
it as an `Option<T>` without removing it, and `next(push)` replaces it when
`push` is `Some`. There is no pop, so a producer never waits and a consumer
always sees the newest sample:

```sinter
struct Sampler {
    latest: flow<u12>,
}

impl Sampler {
    #[clock(domain = sys)]
    fn tick(self, adc: Option<u12>) -> (Sampler, Option<u12>) {
        (Sampler { latest: self.latest.next(adc) }, self.latest.value())
    }
}
```

## Outputs

A channel is built from registers whose names are the compiler's, so the
clock root's `pub` fields can't hold one (directly, or inside a struct). To
publish a queue's state, output what you need, such as `len()` or `front()`,
through the clock root's extra outputs or a `pub` register:

```sinter,fail
struct Queue {
    pub q: fifo<u8, 4>,
}
~impl Queue {
~    #[clock(domain = sys)]
~    fn tick(self) -> Queue {
~        Queue { q: self.q.next(self.q.sender().no_push(), 0) }
~    }
~}
```

```text
error: `pub` field `q` holds a channel, whose registers are internal to the compiler and can't be output ports
```

`pub` on a channel field of any other struct is fine: there it only makes the
field visible to other modules.

Queues between clock domains are on the [Clock domains](clock-domains.md)
page.
