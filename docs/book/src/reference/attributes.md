# Attributes

An attribute is written `#[name]` or `#[name(args)]` before the item or field
it applies to. An argument is a word, `key = value`, where the value is a
name, an integer, or a string, or `key` alone.

| Attribute | On | Meaning |
|---|---|---|
| `#[clock(domain = d)]` | a method | the clock root for domain `d` ([State](state.md)) |
| `#[clock(domain = d, period = p)]` | a method | a clock root with period `p`, required with several domains ([Clock domains](clock-domains.md)) |
| `#[domain(d)]` | a state field | puts the field's registers in domain `d` ([Clock domains](clock-domains.md)) |
| `#[encoding(binary)]`, `#[encoding(onehot)]` | an enum | how its variants are encoded ([Types](types.md#enums)) |
| `#[storage(regs)]`, `#[storage(bram)]` | a `fifo` field | build the queue from registers or block RAM ([Channels](channels.md#storage)) |
| `#[module]` | a struct | make it a Verilog module of its own, instantiated by its parent ([Hierarchy](hierarchy.md)) |
| `#[verilog(module = "m", ...)]` | a struct | bind the struct to Verilog module `m` ([Foreign Verilog](foreign-verilog.md)) |

Any other attribute, an attribute in the wrong place, or an argument it
doesn't take is an error, so a misspelling can't go unnoticed:

```sinter,fail
struct Queue {
    #[storag(bram)]
    q: fifo<u32, 16>,
}
~impl Queue {
~    #[clock(domain = sys)]
~    fn tick(self) -> Queue {
~        Queue { q: self.q.next(self.q.sender().no_push(), 0) }
~    }
~}
```

```text
error: unknown attribute `#[storag]`; the attributes are `clock`, `domain`, `encoding`, `storage`, `module`, `verilog`
```
