# Option and `?`

`Option<T>` is a value that may be missing: either `Some(value)` or `None`.
In hardware it is a **valid bit** and a payload of type `T`, so it is the
natural type for anything that is only sometimes meaningful: a bus beat, a
lookup that can miss, the head of a queue. `T` can be any type.

## Making and matching options

```sinter
// The input doubled, if it is small enough.
fn double_small(x: u8) -> Option<u9> {
    if x < 128 { Some(x + x) } else { None }
}

struct Top {
    pub last: Option<u9>,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self, x: u8) -> (Top, u9) {
        let shown: u9 = match self.last {
            Some(v) => v,
            None => 0,
        };
        (Top { last: double_small(x) }, shown)
    }
}
```

- `Some(v)` makes a present value and `None` a missing one. `None` needs its
  type from context (here, the function's result type).
- `match` takes an option apart. `Some(v)` binds the payload to `v`;
  `Some(_)` matches any present value. A `match` must cover both `Some` and
  `None` (or end with `_`).
- Patterns nest: `Some(Some(v))` matches an `Option<Option<T>>` holding a
  value, and `(Some(a), Some(b))` two options that both hold one.
- `Option` values can't be compared with `==`; use `match`.

An `Option` field with no reset value resets to `None`. In the generated
Verilog, `last` becomes `last_is_some_q` and `last_payload_q`. When the valid
bit is 0, the payload is defined (it is 0 after reset) but meaningless.

## The `?` operator

Inside a function that returns an `Option`, `e?` takes the payload of the
option `e`, and makes the function's result `None` if `e` is `None`:

```sinter
fn below(x: u8, limit: u8) -> Option<u8> {
    if x < limit { Some(x) } else { None }
}

// x + y, if both are below 100.
fn sum_small(x: u8, y: u8) -> Option<u9> {
    let a = below(x, 100)?;
    let b = below(y, 100)?;
    Some(a + b)
}
~struct Top { pub s: Option<u9> }
~impl Top { #[clock(domain = sys)] fn tick(self, x: u8, y: u8) -> Top { Top { s: sum_small(x, y) } } }
```

This is not control flow: nothing returns early. The hardware computes every
payload, and ANDs the valid bits: `sum_small` is `Some` exactly when both
`below` calls are. The payload of a `None` result is computed anyway and
should be ignored.

`?` is only allowed in a function whose result type is an `Option`. In
particular, a register can't be updated with `?`, because a register always
has a next value. To keep a register's value when an option is `None`, say so
with a `match`:

```sinter,ignore
let total = match sum_small(x, y) {
    Some(s) => s,
    None => self.total,
};
```

## Options in the language

Several built-in types use `Option` in their interfaces. A queue's `front()`
is an `Option` (`None` when empty), and pushes into a `ring` or `flow` are
`Option`s (`None` means no push). A `pipeline fn` reports results as a valid
bit beside the payload; see [Pipelines](staged.md).
