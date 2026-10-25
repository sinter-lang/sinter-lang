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
| `#[verilog(module = "m", ...)]` | a struct | bind the struct to Verilog module `m` ([Foreign Verilog](foreign-verilog.md)) |

`#[encoding(gray)]` and `#[encoding(johnson)]` are accepted, but currently
give the binary encoding.

`#[cdc_sync]`, `#[cdc_side]`, and `#[cdc_ram]` are reserved for the compiler's
own clock-domain crossings and are rejected in source.

The compiler currently ignores attributes it doesn't know, so a misspelled
attribute has no effect and no warning. Check the spelling of the ones above.
