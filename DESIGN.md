# Sinter — Language Design Report

> A Rust- and Rhai-inspired register-transfer-level (RTL) hardware description
> language that lowers to portable Verilog and to a cycle-accurate Rust
> simulator. This document is the seed specification for the repository. It is
> written to be handed to an implementation agent (e.g. Claude Code) as the
> source of truth for the initial design.

---

## 0. Name

**Sinter.**

- Sintering is the process of fusing powder/particles into one solid, hardened
  mass using heat **below** the melting point — a clean metaphor for compiling
  many discrete modules into a single hardened netlist.
- Adjacent to silicon / ceramics / metallurgy; short, one fewer syllable than the
  alternatives, pronounceable (`/ˈsɪntər/`), and reads like a real tool name.

**Naming & availability (checked at time of writing — re-verify before
registering):**

| Resource | Status / decision |
|---|---|
| `sinter` on crates.io | **taken** — a small, dormant string-interning crate (`0.1.1`, last updated mid-2024, low download volume). Not abandoned-zero but not actively developed. |
| `sinter-lang` on crates.io | **available** — use this as the published crate name |
| `github.com/sinter` (org handle) | taken |
| `github.com/sinter-lang` | **available** — use this as the GitHub org |
| `github.com/sinter-hdl` | available (fallback) |

**Decisions:**
- **GitHub org:** `sinter-lang` (mirrors the Spade project's `spade-lang`
  convention — reads as established practice, not a workaround).
- **Published crate:** `sinter-lang`. A crate and its installed binary need not
  share a name, so this can still install a short `sinter` executable.
- **CLI binary:** **`sinter`** (`cargo install sinter-lang` drops a `sinter`
  binary on PATH). If any echo of the interning crate is unwanted, fall back to
  `sinterc`.
- **Source file extension:** **`.snt`**.

> Note on the crate collision: the bare `sinter` crate name is occupied, so the
> compiler publishes as `sinter-lang`. This is a cosmetic constraint only —
> publishing and org naming both work cleanly under the `-lang` suffix. If a
> fully collision-free bare crate name is later judged important, the previously
> evaluated alternative `silicide` (silicon–metal compound; keeps the `-ide`
> tie to *oxide*/Rust) was clean on crates.io and remains the strongest backup.

---

## 1. Vision and Scope

Sinter is a **register-transfer-level** HDL. It is *not* high-level synthesis
(HLS): the programmer always describes hardware that exists, and the cost of
every construct is locally legible. The design borrows the ergonomics and type
system of Rust, plus lightweight scripting-language affordances reminiscent of
Rhai, and applies them to synchronous digital logic.

Core goals, in priority order:

1. **Bit-accuracy by construction.** The emitted Rust simulator and the emitted
   Verilog are two lowerings of one source and must agree bit-for-bit, cycle-for-cycle.
2. **Local reasoning / verifiability.** Every stateful element has exactly one
   driver; the per-cycle combinational graph is acyclic; no construct requires
   whole-program analysis to understand.
3. **Static safety for the classic silicon-failure classes** — clock-domain
   crossings, lossy narrowing, multiple drivers, and resource contention are
   *type errors*, not runtime/silicon surprises.
4. **Predictable lowering.** The type system is a compile-time-only guarantee;
   emitted RTL is "dumb" (plain bit vectors + localparams). No hidden cost model,
   no magic resource inference.

Explicit **non-goals** (at least for v1): HLS-style automatic pipelining of
arbitrary sequential code; automatic resource binding driven by a cost model;
suspendable coroutines with self-referential state.

---

## 2. Execution Model

### 2.1 The clock-cycle = function-invocation principle

The fundamental unit is a **transition function** whose single invocation
corresponds to **exactly one clock cycle**. A clock-root function is marked with
an attribute identifying it as the top of a clock domain.

```
#[clock(domain = sys)]
fn tick(self) -> Self {
    // body describes the next-state logic for one cycle of `sys`
}
```

A clock-root function may invoke other functions, but:

- **non-reentrantly** (no instance is stepped more than once per cycle), and
- all invoked combinational logic **completes within the same cycle**.

**Surface syntax (SETTLED direction; `examples/go_board_counter.snt` is the
canonical artifact).** Persistent state is a `struct` — which may nest other
structs (hierarchical ownership, §4.3) — with **inline reset values**
(`count: u20 = 0`). The transition function is a `self`-taking method whose
return is the new state; inputs arrive as additional parameters, and outputs are
either fields of the returned state or extra tuple elements (a combinational
sub-step uses the §2.3 `(new_state, outputs)` form, e.g.
`fn step(self, cmd) -> (Digit, u1)`). The **unique** `#[clock(domain = …)]`
function is the design's synthesizable top, and is what the physical pin map
(§7.6) resolves against. The Verilog-style `module { in/out/reg }` construct in
the current grammar is the legacy MVP surface, slated to migrate onto this form.

### 2.2 Two function colors at the base level

- **Combinational `fn`** — no cycle breaks; completes within the cycle; composes
  as a pure call tree; value → value; single-assignment by construction.
- **Sequential / stateful functions** — may contain cycle breaks (`yield`).
  Their persistent state lives in registers between invocations.

A combinational `fn` *may invoke* a stateful function. What it may not do is
**originate** the persistence: something up the hierarchy must own the stateful
instance's `self` in a register and thread it. (See §4.)

### 2.3 Lowering of a stateful step

A stateful function lowers to:

```
step(self, inputs) -> (self, outputs)
```

One invocation of `step` is **pure combinational logic**. Nothing about the
*call* spans a cycle. What spans cycles is that `self` is stored in a register
and fed back. This is exactly the Rust `async`/coroutine MIR transform
(`yield` ≈ `.await`, a stateful fn ≈ `async fn`, the generated state struct ≈ the
coroutine state machine) — **minus** the hard part: Sinter values are copied
bit-vectors with no references into the state, so there is no self-referential
borrow problem and no `Pin` analogue. We implement the *easy* version of the
transform.

---

## 3. The Two Staged Function Colors: `pipeline` and `multiplex`

`yield`-style staging generalizes into two first-class function colors that
represent the two poles of the space/time trade-off. They **share machinery**
(liveness-across-yield ⇒ registers) and differ in one bit: whether stage logic
is replicated in space or folded in time.

### 3.1 `pipeline fn`

- Cycle breaks denote **stage boundaries**.
- Variables live across a stage boundary are **implicitly held** in pipeline
  registers to the stages that need them.
- **All stages run on every cycle** (spatial replication). Throughput 1
  result/cycle; latency = number of stages N.
- The state carried across a break = the set of locals live across that break,
  plus (for data-dependent control) a resume tag.

**Backpressure (the hard part).** Naïve backward-flowing `ready` across N stages
is a combinational path the length of the pipe and will wreck the clock. Provide
a tiered model, **encoded in the type**:

| Mode | Type | Semantics | Cost |
|---|---|---|---|
| Free-running valid-only (**default**) | `Valid<T>` | data + valid bit march forward; bubbles = invalid beats; consumer must keep up (no ability to push back) | cheapest |
| Global stall | (enable) | one enable freezes all pipeline regs together | cheap, single flow-control authority only |
| Elastic / latency-insensitive (**opt-in**) | `Stream<T>` | per-stage valid+ready + 2-deep skid buffers; backpressure propagates one stage/cycle | ~2× pipeline registers |

**Key safety rule:** connecting a free-running valid-only source to a consumer
that needs to stall it is a **type error**, not a silicon bug. (Same move as
clock-domain safety: make the unsafe composition unrepresentable.)

> **Refinement (PL milestone):** the free-running "valid-only" type is just
> **`Option<T>`** (DESIGN §6.5) — `None` is a bubble, `Some` a datum — not a
> separate `Valid<T>`; they are the same "maybe a value," and `?` (§6.4)
> propagates bubbles. See §9.3.

**Pipeline hazard rule (load-bearing for verifiability):** a `pipeline fn` may
carry **per-wavefront** state (the marching variables, private to each datum)
but may **not** touch externally-owned mutable state except through an
explicitly hazard-managed primitive. Keeping pipelines pure dataflow avoids
reintroducing CPU-style RAW/WAW/WAR hazards and forwarding. In this
implementation the rule holds **structurally**: a `pipeline fn` takes no `self`,
so it can only read its inputs, `const`s, and its own per-wavefront locals.

**Driving, validity, and reset (settled; PL3 lowering).** A `pipeline fn` is the
*source* for an auto-generated stateful instance; the honest Mealy step lives in
that generated instance, not the source signature:

- The source stays straight-line `pipeline fn f(in…) -> Out` (no `self`). The
  compiler synthesizes an **instance type** (one register per live-across-`yield`
  local, plus a valid bit per stage) and an
  `advance(self, in: Option<In>) -> (Self, Option<Out>)` method — the existing
  sub-state `(new_state, output)` pattern (§2.2), just generated.
- The instance is **owned state**: a field of the parent struct, advanced once
  per cycle inside the parent's clocked `tick`; the returned new-instance is
  stored back into the parent's next state, the returned `Option<Out>` consumed
  combinationally that cycle.
- **Output is `Option<Out>`** — `None` during the `latency` fill cycles after
  reset and for any bubble; `Some` once a wavefront reaches the end. Validity is
  the consumer's "a real result popped out vs. values still in flight" signal,
  not a junk value masquerading as data.
- **Reset is fully defined** (oracle-safe): valid bits reset to 0 (empty), payload
  registers to their type's zero — so the fill-region payload is
  defined-but-ignored, never undefined. Fill latency and mid-stream bubbles are
  the **same** mechanism (the marching valid bits), not separate cases.
- **Outputs are final-only**: a pipeline has one output point (the tail).
  Mid-pipeline `yield expr` taps are **not** part of the model (the parsed form
  is rejected); revisit only if a concrete need appears.

### 3.2 `multiplex fn`

- Stages are **folded in time** onto shared logic.
- **Exactly one section/arm runs on any given cycle**, allowing a scarce
  resource (e.g. a multiplier) to be reused across tasks across cycles.
- Throughput 1/N; latency N (or data-dependent, see §3.4).

**Mutual exclusion is structural and free.** Because exactly one arm runs per
cycle, any resource touched by at most one arm is contention-free by
construction — the same free proof you get from non-overlapping `match` arms.

**Resource sharing is explicit, never automatic.** We deliberately reject
HLS-style automatic binding of operations onto shared units (that needs a cost
model = unpredictable magic). Instead:

- The user instantiates **one** shared resource (e.g. a `Mul` unit).
- Arms call it.
- The compiler **verifies** it is invoked from ≤1 arm per cycle (free,
  structural) and inserts the operand mux.
- The user sees exactly one DSP in the source.

This is the same principle as merge combinators (§5): the language never
silently shares; sharing is an explicit instance whose exclusivity obligation is
discharged by the multiplex arm structure.

### 3.3 Unified selector: binding time is a parameter

There is **one** `multiplex fn`. Its next-state / arm selector is a
**parameter** whose *binding time* varies. "Implicit sequential" is just the
selector bound to a constant.

- **Const selector** ⇒ schedule is a counter the compiler already knows ⇒ the
  next-state block folds to an increment; arms never visited are eliminated; tag
  register may shrink. **Static latency.**
- **Runtime selector** ⇒ next-state is real combinational logic over state and
  inputs ⇒ a full FSM is elaborated. **Dynamic latency.**

The mental model is Zig `comptime` / Rust const-generics + partial evaluation:
the same parameter slot accepts a constant or a runtime value and the compiler
specializes. Two instantiations of one source can produce very different
netlists; you pay for dynamism only where you ask for it.

**Hard boundary — what "runtime" may and may not mean:** the *set of arms* and
the *transition graph* are **always compile-time** (that graph *is* the
hardware; you cannot synthesize an arm you did not elaborate). What is runtime is
only *which edge fires* — a signal indexing a statically fixed transition table.
"Runtime selector" must never slide toward "runtime set of arms."

### 3.4 Latency is a type, derived from binding time

Binding time is not orthogonal to the type system — it **sets** the latency
class. The user-facing knob is "how is the selector bound"; the derived, checked
fact is "what latency class does this function expose." The user cannot choose
runtime selection *and* claim static latency.

Three latency points (not two):

| Latency class | Source | Composes into |
|---|---|---|
| `Static(n)` | const selector; fixed stage count | fixed pipelines directly |
| `Bounded(max)` | runtime selector with a *provable* worst-case bound (no unbounded back-edges, or a decrementing guard counter) | worst-case-sized fixed pipelines |
| `Dynamic` | runtime selector, no provable bound | only latency-insensitive (`Stream`) contexts |

The **`Bounded` middle is where most useful runtime selectors live** (early-exit
MAC, divider that finishes in ≤N, bounded protocol step). Capturing it lets a
runtime-selected FSM still legally drop into a worst-case-sized fixed pipeline,
retaining real static-scheduling leverage.

**Composition rule:** latency (or latency-insensitivity) is part of every
function's signature and is checked at composition:
- fixed-latency multiplex inside a pipeline stage → fine (adds stages);
- data-dependent (`Dynamic`) multiplex inside a *fixed* pipeline → **error**
  unless the pipeline is elastic.

This is precisely the territory of **Filament**'s timeline types — the most
directly relevant prior art for this part; read it before fixing the signatures.

### 3.5 OPEN QUESTION (decides scheduling power)

For the `Bounded` case: should the compiler **prove** the latency bound from the
transition graph (enabling a runtime-selected FSM to legally enter a
worst-case-sized fixed pipeline — retains Filament-style leverage, obligates a
real termination/bound analysis), **or** treat every runtime selector as fully
`Dynamic` and force unconditional elasticity (much simpler to implement, throws
away the free `Bounded` middle)? Recommendation: design the types to *admit*
`Bounded` from day one even if the prover starts conservative (counter-guarded
loops only), so the analysis can strengthen later without a type-system break.

---

## 4. State Ownership, Single-Driver Rule, Local Reasoning

### 4.1 Generalized single-driver rule

The classic "one register, one next-state expression" rule generalizes to
**every stateful element**, attributed to the element's **instance identity**
(its stored state), not to the by-value `self`:

- a register is assigned **once** per cycle;
- a memory **write-port** is driven **once** per cycle;
- a stateful/generator **instance** is **stepped once** per cycle.

Double-stepping an instance in a cycle is **literally the same error** as
double-driving a wire. This single rule gives non-reentrancy *and* makes `yield`
mean exactly one clock.

### 4.2 Acyclic combinational graph

The per-cycle combinational dataflow must be **acyclic**; any feedback must pass
through a register (i.e. through a yielded `self`). Enforcement = move-checking +
topological sort, both already trusted from Rust.

### 4.3 Hierarchical ownership (not flatten-to-root)

State **nests**: root ⊃ module ⊃ sub-FSM, the way a `Future` contains `Future`s.
A module owns its sub-FSM instances as fields of its own state struct, threads
them **locally**, and exposes only its combined next-state upward. Rejected
alternative: flattening every `self` to the root — more uniform but turns the
root signature into a phone book and destroys encapsulation.

### 4.4 Stateful function outputs are a sum type

A stateful function has two distinct outputs: the value yielded each active
cycle, and the value returned once on completion (often different types).
Therefore a step's output is naturally a sum:

```
enum Step<Y, R> { Running, Yielded(Y), Done(R) }
```

This is exactly where the `Option`/valid machinery (§6) plugs in: a multi-cycle
function *is* an `Option`-returning thing at cycle granularity, and a completed
generator producing a value is precisely a `valid + payload` handshake.

**OPEN QUESTION (signature shape):** model the yield-value and return-value as
*separate channels* in the signature, or *collapse* both into the `Step` status
enum and let the caller `match`? (Recommendation: the `Step` sum is cleaner and
threads through `?`/valid uniformly; separate channels may read better for
fixed-latency pipelines where "yield each cycle" is the norm. Revisit once the
pipeline vs multiplex signatures are drafted.)

---

## 5. Merge / Multiple-Driver Discipline (the "wired-OR" problem)

A primary goal: **statically reject** the implicit-OR behavior of wires when
multiple modules respond to the same register-read request.

- With single-assignment values, multiple drivers are **structurally
  unrepresentable** — there is no `assign` statement a second party can also
  write, so there is nothing to OR.
- The real case (N peripherals, one should answer a read) is handled by an
  **explicit typed combinator**, never by emergent wiring:
  - Model the address space as an **enum**; route the read with a `match`.
    Match arms cannot overlap ⇒ exclusivity is a **free structural proof**.
  - If peripherals self-decode with comparators, that reintroduces overlap;
    require either a static range-disjointness check (an SMT obligation) or a
    sim-only assertion.
- Provide two named combinators (never an implicit merge):
  - `merge_exclusive` — carries the proof obligation that at most one input is
    valid per cycle (checkable / structural via match arms);
  - `merge_priority` — defines an explicit winner when overlap is allowed.

**One consistent principle covers wired-OR, bus merge, and resource sharing:**
merge is always a *named combinator with stated resolution semantics*; sharing
is always an *explicit instance*; the structure (match arms / multiplex arms)
discharges the exclusivity obligation for free.

---

## 6. Type System

### 6.1 Fixed-point types (built-in)

- Signed/unsigned fixed-point are first-class, spelled with a **dotted**
  `uI.F` / `iI.F` notation (e.g. `i4.8` = signed fixed-point, 4 integer bits and
  8 fractional bits). The dot is parseable without ambiguity provided the
  fractional part lexes atomically as `"." ~ ASCII_DIGIT+` — the dot must be
  immediately followed by a digit, so it never collides with `..` ranges or with
  member access `.field` — and types only ever appear in type position.
- **Sign-bit convention (SETTLED).** For `iI.F` the total width is exactly
  `I + F` bits and **the sign bit is the MSB of the integer field — i.e. `I`
  includes the sign.** Worked example: `i4.8` is 12 bits total; the integer
  field is 4 bits (1 sign + 3 magnitude) and the fractional field is 8 bits, so
  the representable range is approximately `[-8.0, +8.0)` at a resolution of
  `2^-8`. Unsigned `uI.F` is `I + F` bits with no sign bit. This is normative;
  the Rust sim and the Verilog must both honor it bit-for-bit.
- **Word-scaled / coarse integers are *not* a separate type family (DEFERRED).**
  A "stored in N bits but each step is worth `2^k`" type is just a fixed-point
  number whose binary point sits to the *left* of the LSB (negative fractional
  bits): `u4` scaled by `2^2` ≡ `u4.-2`. A single "where is the binary point"
  axis covers both precision (fractional bits to the right) and coarseness (to
  the left). Deferred until a concrete need appears; if revived it folds into the
  fixed-point exponent rather than adding a third suffix.

### 6.2 Width-inferred arithmetic

- Addition/multiplication produce a result **wide enough to hold all possible
  values**:
  - `iN + iM` → `i(max(N,M)+1)`
  - `Qa.b * Qc.d` → `Q(a+c).(b+d)`
- **Signed multiply is *total* under full width inference — there is no
  most-negative special case at the multiply.** `iN × iN` infers to `i(2N)` (not
  `i(2N-1)`), and `min × min = +2^(2N-2)` fits, because a `2N`-bit signed value
  reaches `+2^(2N-1) − 1 ≥ 2^(2N-2)`. The classic "−1.0 × −1.0 = +1.0 overflows"
  gotcha is purely an artifact of the DSP convention that keeps `2N-1` bits
  (dropping the redundant sign bit / left-shifting to renormalize). Sinter keeps
  the multiply full-width and surprise-free; the min×min decision only surfaces
  later, at the **explicit narrowing cast** (§6.3) — exactly where every lossy
  choice should live. A hardware DSP block's double-wide output maps straight
  onto the inferred product type.
- Elegant consequence: in a feedback path (`acc = acc + x`) width cannot grow
  unbounded because the register's declared width **forces a narrowing at the
  register boundary** — i.e. the type system funnels every lossy operation to the
  clock edge, exactly where a DSP engineer wants to choose round/saturate.

### 6.3 Narrowing is an explicit, mode-named cast (NOT `?`)

Narrowing/saturation is a **value transform**, not an early return, so it must
**not** be overloaded onto `?`. Require a cast that names its mode, with **no
default**:

```
x as i6.10 sat     // saturate
x as i6.10 wrap    // wraparound / truncate high bits
x as i6.10 trunc   // truncate fractional bits
x as i6.10 round   // round
```

Any implicit lossy assignment is a **compile error**. (Mechanically enforceable,
not a convention.)

**Two-dimensional semantics (SETTLED, F4).** A cast `x as TYPE mode` adjusts the
fractional part then the integer part, lowering to shift/resize IR ops:
- **fractional bits.** Widening (more frac bits) shifts left (exact). Narrowing
  drops bits: `round` rounds half up (add `2^(k-1)`, then arithmetic shift
  right); every other mode truncates (arithmetic shift right toward −∞).
- **integer bits.** Widening sign/zero-extends (value-preserving). Narrowing
  truncates the high bits, except `sat` which saturates to the target range.

So the mode picks the frac rounding (`round` vs truncate) and the int overflow
(`sat` vs wrap): `wrap`/`trunc` = truncate frac + wrap int; `sat` = truncate frac
+ saturate int; `round` = round frac + wrap int. (Combining rounded frac with
saturated int is two casts.) The fixed-point representation **erases to a plain
`(I+F)`-bit integer** in the IR (DESIGN §7.2); the binary point lives only in the
front-end. Widening the fraction first builds an `xi + tf`-bit intermediate (old
integer bits, new fraction bits) before the integer step narrows it; that
intermediate must fit the 64-bit width limit (§6.2), else the cast is rejected.
*Deferred:* `+`/`-` require matching fractional bits (no implicit alignment yet);
sign-changing casts.

### 6.4 `?` is for Option/valid propagation only

`?` propagates validity (think data-valid wire on a bus). In a *combinational*
body, "early return None" lowers to valid-bit AND-reduction, **not** control
flow:

```
out.valid   = a.valid & b.valid & ...   // payload computed unconditionally, gated downstream
```

**Critical boundary:** `?` works for Option-typed *dataflow*. It does **not**
work for register writes — a register always has a next value; there is no
"skip." A register update on an invalid path is a **guarded hold**:

```
reg <= cond ? new : reg;   // NOT a skipped statement
```

So: `?` lives inside Option-returning helpers; `reg <= compute()?` must be
rejected or defined as a guarded hold, never as a vanishing write. Make this
explicit in the semantics.

### 6.5 Option and enums (native)

- `Option<T>` is a native type/concept; a channel read is morally
  `Option<T>`, so it threads cleanly through `?`.
- `enum` (algebraic, with payloads) is native; `Option` is the
  one-variant-payload special case.
- **Enum encoding is annotatable (SETTLED syntax).** A `#[encoding(...)]`
  attribute selects the bit-encoding: `binary` (the default, `ceil(log2 N)`
  bits), `onehot` (`N` bits), `gray`, or `johnson`. Independently, Rust-style
  **explicit discriminants** (`enum Op { Rd = 0b01, Wr = 0b10 }`) let an external
  protocol encoding be matched exactly. The two compose: the named scheme says
  "pick a good encoding of this kind"; explicit values say "I am these exact
  bits." Encoding is never silently inferred across an external boundary.

### 6.6 Containers, arrays, memories, channels (first-class)

- **Arrays split into two distinct concepts** (do *not* unify):
  - **vector-of-registers** — `[T; N]`, all elements readable combinationally,
    lowers to flops/LUTs;
  - **memory** — `mem<T, N>`, port-limited (N reads / M writes per cycle),
    lowers to BRAM. **Port count is part of the memory type.** A read carries a
    latency contract (§3.4). Accessed method-style on a struct-field instance
    (`self.m.value()` for the read-data, `self.m.next(raddr, we, waddr, wdata)`
    for the transition), never index-style — `m[addr]` already means the
    combinational vector-of-registers read.
    Realized for the **1R1W, `Static(1)` registered, `READ_FIRST`** slice in
    §9.6 (MEM1).
- **Fixed-capacity channels** — capacity is part of the type, enabling static
  reasoning about backpressure (cf. SpinalHDL `Stream`/`Flow`, Bluespec FIFOs).
  **RESOLVED (§6.11 / §9.8):** v1 channels expose ready/valid and guarantee
  *no-corruption* (a lossless `fifo<T, N>` has no method that overwrites a live
  slot) but **not** deadlock-freedom — the "genuinely hard" liveness tier is
  deferred. The `fifo` / `ring` / `flow` family and the milestone slice are §6.11
  / §9.8.
- **Arbitrary integer bit widths** (`iN`/`uN` for arbitrary N).

### 6.7 Clock domain as a phantom type parameter (flagship safety win)

Make the clock domain a **phantom parameter on every signal**. Crossing domains
without an explicit CDC primitive is a **type error**. Same trick applies to
reset polarity/domain. CDC bugs are a top cause of real silicon failure and
almost nothing catches them statically — this is a headline feature.

### 6.8 Division / non-pipelineable ops

Width inference is ill-behaved for division and such ops are not single-cycle.
**Forbid bare `/`**; force division through a multi-cycle primitive with a
handshake (a `Bounded`-latency multiplex instance, §3.4).

### 6.9 Memory model (RESOLVED — see §9.6)

Resolved per the §6.9 recommendation: a memory is a **distinct primitive type**
whose interface *is* a latency-contracted stateful instance (so it composes under
the §3.4 latency rules), but whose lowering is hard-wired to
BRAM/`DP16KD`/`PDPW16KD`-style primitives rather than synthesized from generic
state. It is held in a struct field and is explicitly guarded **out** of the SROA
flattening that turns a `[T; N]` vector-of-registers into N scalar leaf flops, so
the two concepts stay distinct (§6.6). MEM1 (§9.6) realizes the 1R1W,
single-clock, `Static(1)` registered-read, `READ_FIRST` slice via Yosys BRAM
inference; explicit `DP16KD`/`PDPW16KD` instantiation, multi-port, dual-clock,
and ROM-init are deferred.

### 6.10 Integer literals (SETTLED)

Integer literals are **untyped by default**; their width is resolved from
context (the surrounding type). A literal may carry an optional **Rust-style
width suffix** — `0b0111111_u7`, `42_i16` — which asserts its type and makes a
width mismatch with context a compile error (useful for bit-pattern constants,
where an off-by-one-bit typo is otherwise easy to miss). Supported forms:
decimal, `0x` hex, and `0b` binary, with `_` permitted as a digit separator.

### 6.11 Fixed-capacity channels and FIFOs (RESOLVED shape)

A **channel** is a fixed-capacity, single-clock queue held in a struct field and
accessed method-style — the `mem<T, N>` interface convention (§6.9 / §9.6) applied
to a ring buffer rather than a random-access array. `T` is a scalar (≤ 64 b), `N`
(capacity) a power of two.

**The interface is the user's mental model made literal:** a pure read of the
head, and a single threaded transition carrying an optional push and a pop flag.

```
fn front(self)    -> Option<T>          // head word; None iff empty  (pure)
fn is_empty(self) -> u1                 //                            (pure)
fn is_full(self)  -> u1                 // the ready / backpressure read (pure)
fn len(self)      -> uK                 // 0..=N, K = clog2(N)+1       (pure)
fn next(self, push: Option<T>, pop: u1) -> fifo<T, N>   // threaded transition
```

`next()` **is** the single-driver, exactly as for `mem`: its handle commits only
when threaded into the tail `Self { q: self.q.next(push, pop) }`, a discarded
`next()` is inert, and it **may not appear in an `if`/`match` arm** (gate
`push`/`pop` with flags, never mux the handle). `front()`/`is_full()`/`len()` are
pure reads, unrestricted. For the shallow register-backed lowering `front()` is
combinational (`Static(0)`); the deep BRAM-backed tier makes it a registered
`Static(1)` read (a deliberate, separately-milestoned latency change — §9.8).

**Loss policy is chosen by the type, never a call-site flag** (so a lossy queue
*reads as lossy at a glance*):

- **`fifo<T, N>` — lossless.** A push into a full queue (with no simultaneous pop)
  is a **guarded hold** (§6.4): contents and `count` unchanged, the word refused,
  no `X`. It has *no method that overwrites a live slot* — overwriting the newest
  *or* oldest datum is structurally unrepresentable. Pop-when-empty is inert. A
  same-cycle push+pop is **READ_FIRST** (the pop sees the old head), matching
  `mem`'s same-address policy so sim ↔ Verilog agree bit-for-bit.
- **`ring<T, N>` — overwrite-oldest.** Identical surface; a push into a full ring
  advances the head (drops the oldest) and always accepts. For trace/telemetry
  buffers where the newest data matters most.
- **`flow<T>` — depth-1 newest-wins** (optional, low priority). A single valid
  slot, `value() -> Option<T>`, no pop — erased exactly like `Option`. A "latest
  sample" register.

**Overflow safety — the honest ceiling.** "Push into a full FIFO" cannot be made a
*compile error* in general (it needs a static occupancy proof — undecidable
without dependent types). The layered guarantee that **is** true:

| Guarantee | Tier |
|---|---|
| No corruption; never overwrites live data; no `X`; deterministic | **compile-time / structural** — a `fifo` has no clobbering method; lossy needs a different type |
| A consumer can't push / a producer can't pop | **compile-time / type** — directional views (CHAN3) |
| Whether a push *fires* this cycle | **runtime** — the ready/valid the type surfaces (`is_full()`) |
| A producer that *ignores* `is_full()` and thus drops | **compile-time / type** once the §3.1 connection rule lands (CHAN3) |
| Deadlock / liveness freedom | **out of scope** — §6.6's "genuinely hard" tier |

The two hard requirements — never silently drop, never overwrite the newest — map
cleanly: overwrite-newest is *unrepresentable* (compile-time), and silent-drop is
*prevented by honoring the `is_full()` backpressure the type forces into view*
(runtime, promotable to compile-time via CHAN3).

**Sender/Receiver = capability views, not two owners (SETTLED).** Sinter threads
state by value through one owner, so two independently-threaded owners of one queue
is either a double-drive (illegal, §4.1) or forces a bespoke `merge` that hard-codes
the queue's internal count arithmetic and generalizes to nothing. Instead,
`Sender<T, N>` / `Receiver<T, N>` are **pure directional views** over the one owned
field (`Sender` exposes `is_full`/push-request forming; `Receiver` exposes
`front`/`is_empty`/pop-request forming). The commit stays a single `next()` on the
owner; cross-module producers/consumers exchange **values** (a `push` request
`Option<T>`, a `pop` request `u1`, the observations `is_full()` / `front()`), which
the owner reconciles in one `next()`. This gives the Rust Sender/Receiver
ergonomics with the single-driver invariant untouched. A `fifo` is also the
skid-buffer building block for the elastic `Stream<T>` (§3.1). Milestone: §9.8.

### 6.12 Fault flags and assertions (RESOLVED shape)

"Should never happen" conditions sort into three buckets, matching the language's
"prefer a type error; a sim-only assertion (or SMT obligation) otherwise" stance
(§5):

1. **Statically excludable** → `unreachable!()`, which the compiler must **prove
   dead** (a compile error otherwise). Zero emitted bits — the true "panic that
   compiles out."
2. **Runtime but diagnostic-only** → `assert!(cond, "msg")` / `debug_assert!` —
   **sim-only** predicates over already-traced signals; a failure aborts the sim
   run (a test failure) but emits **no** bit in either backend. This is the §5
   "sim-only assertion" escape hatch made first-class.
3. **Runtime, data-dependent, latched in silicon** → a `FaultSet<Fault>` — ordinary
   threaded state (below).

**`FaultSet<Fault>` — a sticky, one-way fault register.** `Fault` is a user `enum`
with **explicit discriminants** (so the aggregated code is stable across edits; `0`
is reserved for "no fault"). The set is a sticky one-hot over the variants, raised
and read method-style:

```
enum Fault { DivZero = 1, BadOpcode = 2, Overflow = 3 }
struct Alu { acc: i32 = 0, faults: FaultSet<Fault> = {} }   // resets empty

// raise(cond, F) is the guarded set (the we-gated write); it threads like
// mem.next() and chains, folding to `faults_q | (OR of one-hots)`:
let f = self.faults.raise(div0, Fault::DivZero)
                    .raise(bad,  Fault::BadOpcode);
// ... Alu { faults: f, .. }        // threading = the single-driver commit

fn first(self)           -> Option<Fault>   // priority-encode: WHICH site faulted
fn any(self)             -> u1
fn contains(self, Fault) -> u1
```

`first()` is the "spit out a number telling which thing faulted" the design wants;
its `Option<Fault>` threads through `match`/`?`. `.raise` is sticky
(`q <= q | mask`) — a guarded set that never vanishes (§6.4 boundary intact) and,
like `mem.next()`, may not sit in an `if`/`match` arm. It erases to one `uN`
register and SROA-flattens (no opaque IR).

**No implicit global fault bus.** An ambient bus every `assert!` silently ORs into
is exactly the emergent wired-OR §5 exists to outlaw, and it teleports leaf state to
the root against §4.3 ownership. Faults are named, owner-threaded fields;
cross-hierarchy aggregation is explicit (an owner merges a sub-FSM's set into its
own and threads it up — the §5 named OR-combinator). A future `#[fault]` roll-up
attribute may synthesize the *edit-invariant* `any()` reduction only.

**Observation-only rule (the compile-out keystone).** A value from
`first()`/`any()`/`contains()` is *diagnostic*: it may sink only into the trace
(`<field>_q`), another `FaultSet` read, or a dedicated **fault-sideband output** of
the `#[clock]` root — **never** a register next-state or a functional output. Faults
therefore cannot influence the datapath (to squash a datapath, use an explicit
`Option`/guarded hold on the datapath itself, not the fault channel).

**Compile-out is oracle-clean.** A `--faults on|off` build knob is read **once** at
`ir::Design` construction, so both backends fold from one IR (a Verilog-only strip
is unrepresentable). With `off`, `FaultSet` fields collapse to 0-width, `.raise`
folds to identity, the reads fold to `None`/`0` and DCE away, and the sideband port
is **tied off** (port shape mode-invariant). Because diagnostic values can't reach
the datapath, every functional bit is provably identical across modes. The oracle
runs **per mode** (on ≡ on, off ≡ off), never cross-comparing. Milestone: §9.9.

### 6.13 Bit manipulation: slicing, variable shift, concatenation

The current operator set (`| ^ &`, `+ - *`, `== != < > <= >=`, unary `! ~ -`,
mode-named casts) has **no bit-slice, no surface shift, and no concatenation** —
the IR's `Shl`/`Shr` exist only as compile-time-constant-amount nodes for
fixed-point alignment (§6.1, F4). This is the largest ergonomic gap for any
bit-oriented datapath (protocol decoders, packers, CRCs, and — acutely —
instruction decode; §9.12 / the RV32 forcing function). Proposed, all
width-explicit and predictably lowered (§1 goal 4):

- **Bit-slice** — `x[hi:lo]` extracts a contiguous field, type `u{hi-lo+1}`; `hi`
  and `lo` are **compile-time constants** in v1 (a dynamic slice is a barrel shift
  + mask, deferred). Distinct from `[T; N]` indexing, which selects a whole element
  and is only legal on a vector-of-registers.
- **Shift** — `<<` / `>>` surface operators, split by the shift *amount*:
  - a **literal** amount folds to the existing IR `Shl`/`Shr` and follows width
    inference (§6.2) — `<<` **widens** the result by the amount (`x << 2` is
    `u{w+2}`, no bits lost), `>>` **narrows** it. (Consistent with the rest of the
    language, only a *literal* is accepted as a widening amount — a named `const`
    is not, exactly as array/`mem` lengths and slice bounds require a literal.)
  - a **non-literal** amount (a runtime value, or a named `const`) is a
    **same-width barrel shift** (a new IR `VarShift` node → a native Verilog shift),
    keeping the operand type: bits shifted past the width are lost and a shift
    `>= width` yields `0` (or the sign fill for an arithmetic `>>`). `>>` is
    **arithmetic** for a signed operand, **logical** for unsigned; `<<` is logical.
    The amount is an unsigned integer. This is the `SLL`/`SRL`/`SRA` primitive.
- **Concatenation** — `a @ b` (or `{a, b}`): a width-additive, MSB-first join, type
  `u{wa+wb}`. The dual of slicing; how scattered immediate fields are assembled.
- **Sign-extension from bit k** — a widening `as iN`/`as uN` cast already
  sign/zero-extends from the operand's MSB (§6.3); assembling an immediate whose
  sign lives at an interior bit is `(field @ …) as iN` after the sign bit is slid
  to the MSB (or a `sext(x, k)` helper). Sign-*changing* casts remain deferred
  (§6.3).

Milestone: §9.10.

### 6.14 Runtime-indexed reads and register files (proposed)

A `[T; N]` vector-of-registers is readable/writable only at a **compile-time**
index today (the elaborator rejects a runtime index). A **register file** — the
canonical multi-read, single-write indexed state (an RV32 core needs two runtime
read ports `rs1`/`rs2` and one write port `rd`) — is therefore not expressible. Two
complementary routes, both with locally-legible cost:

- **Runtime index on a vector-of-registers.** A runtime read `regs[i]` lowers to a
  combinational **N:1 mux tree**; a functional-update write `regs.set(i, v)` — a
  pure array method returning the array with element `i` replaced — lowers to
  per-element `mux(i == k, v, regs[k])`, i.e. **guarded holds** under a 1:N decode
  (`x_k <= (i == k & we) ? v : x_k` once gated by an `if we`), the single-driver
  rule per element, unchanged. (There is no statement-level `regs[i] = v`; the
  surface has no assignment.) Multiple read ports are just multiple mux trees over
  the same array; a hardwired-zero register (RV32 `x0`) is a `set` the enable never
  commits. Cost (LUT mux/demux) is explicit, not inferred.
- **Multi-port memory** (`mem<T, N, R, W>`, the §9.6 deferred axis). A 2R1W BRAM
  register file trades the mux-tree LUTs for block RAM and a registered
  (`Static(1)`) read — the right choice for wide/deep files, a stage-latency change
  for narrow ones.

The mux-tree route (v1) reuses the existing SROA leaf-register machinery with no new
IR primitive. Milestone: §9.11.

---

## 7. Compiler & Tooling Architecture

Single source of truth: a **formal grammar specification** for `.snt`. But see
the parser caveat below — "one `.grammar` file emits all four tools" is the
plan's hidden cliff.

### 7.1 Frontend / parser strategy (decide consciously)

A generated grammar that drives *both* a batch compiler and an LSP does **not**
survive contact with editors. Batch compilers want clean ASTs from a clean
grammar (pest/lalrpop); language servers want **error-tolerant incremental
reparsing** — which is exactly why rust-analyzer uses a **hand-written** parser
over a **lossless** syntax tree, not a generated one.

Two realistic paths (pick one explicitly):

1. **tree-sitter** as the shared concrete-syntax layer (highlighting +
   structural editing) with a separate semantic frontend; or
2. a **rowan-style lossless tree** with a hand-written error-recovering parser,
   where the formal grammar becomes the **spec / conformance-test oracle** rather
   than the literal parser generator.

Either way: the formal grammar is the *normative spec and conformance oracle*,
not necessarily the literal parser. Do not chase the
one-grammar-emits-everything dream — it quietly costs a year.

### 7.2 Backends / lowering targets (ordered by priority)

1. **Rust cycle-accurate simulator** (Verilator-equivalent). Generated Rust;
   fixed-point throughout; emits **binary trace files** for field-by-field
   comparison. (Aligns with the user's existing `systemrs` TLM + Verilator trace
   methodology.)
2. **Portable / host-compatible Verilog** backend. Fixed-point, `Option`,
   enums, and width-inferred wide intermediates all **erase to plain bit vectors
   + a few localparams** at the Verilog boundary. The type system is
   compile-time-only; emitted RTL is dumb. This is the right boundary and it
   gives a built-in whole-compiler self-test (next item).
3. **(Later) place-and-route input for nextpnr.**
   **IMPORTANT SCOPE FLAG:** nextpnr consumes a *post-synthesis netlist* (Yosys
   JSON of mapped primitives: LUTs, DFFs, BRAM, DSP). Emitting that directly
   means **writing a synthesizer** (tech-mapping, LUT covering, ABC-equivalent
   work). The pragmatic ordering is **Sinter → Verilog → Yosys → nextpnr**; a
   native synthesizer is a *separate large project*, not a backend variant. Keep
   these as distinct milestones.

### 7.3 LSP / editor integration

`rust-analyzer`-style language server for VS Code: incremental, error-tolerant,
built on the frontend chosen in §7.1. Syntax highlighting via tree-sitter is the
low-cost early win regardless of the deeper parser choice.

### 7.4 Built-in correctness oracle (free, and matches the user's methodology)

Generate the Rust sim and the Verilog from the **same source**, run both, **diff
the binary traces**. The conformance harness becomes the language's own
correctness oracle. This should be wired into CI from the first milestone.

**Verilator settings.** The Verilator co-simulation runs under a shared flag file
(repo-root `verilator.f`) that mirrors the downstream **pico-gs** integration
config (`londey/pico-gs:integration/verilator.f`), so the generated SystemVerilog
is exercised under the same settings as the project that consumes it: `-Wall
-Wno-fatal -j 0 --assert --timing --x-assign unique --x-initial unique`
(waveform tracing is omitted — the harness writes its own binary trace). The
`--x-initial`/`--x-assign unique` X-propagation flags make missing resets /
uninitialised signals surface as trace mismatches. Because that flag set is
`-Wno-fatal` (matching pico-gs), a **separate strict `--lint-only -Wall` pass**
(warnings fatal, no pragmas) enforces the lint-clean guarantee on every generated
module.

### 7.5 (Future, explicitly deferred) custom place-and-route

The "iteratively collapse a high-dimensional network of springs onto the 2-D
FPGA layout" idea is real: it is **analytical / force-directed placement**
(lineage: Kraftwerk → electrostatics-based placers like RePlAce). Motivation
(per the user): build/iteration speed via multithreading and possibly GPU for the
spring model. **Caveats to record so scope is honest:**

- Springs give a good *continuous* placement but **not legalization** (snapping
  to discrete legal sites without overlap).
- Springs say **nothing about routing**, which on FPGAs is the genuinely hard
  half: fixed, scarce routing resources require **negotiated-congestion routing**
  (PathFinder; nextpnr's `router2`).
- So the real project = placement-via-springs **+ legalization + a real router**,
  and the router is where the difficulty lives.

Deferred. Not in early milestones.

### 7.6 Physical pin mapping (SETTLED shape; `examples/go_board.pins`)

Because the functional surface (§2.1) has no top-level Verilog ports, the bridge
from physical pins to the design is a dedicated **pin-map file**, a superset of
the familiar nextpnr PCF idiom:

```
set_io <pin#> <dir> <path> [low]
```

- `<dir>` is `clock` | `in` | `out`.
- `<path>` resolves against the clock-root `tick` signature: an input parameter
  (`btn.inc`), or a field of the returned state addressed by name and bit/array
  index (`seg[0][6]`). A `clock` line binds a pin to a **domain** (`sys`), not a
  field.
- the optional `low` flag marks an active-low board signal, so polarity is
  resolved once at the boundary and the source logic stays written active-high.

The compiler consumes this to (1) synthesize a top-level Verilog wrapper with one
port per pin, wiring each port to/from the addressed bit, and (2) emit the
concrete nextpnr PCF (with the generated port names) as a build artifact. This
keeps the Sinter → Verilog → Yosys → nextpnr flow (§7.2) intact while letting the
source stay portless.

**Realized in F5 (`sinter build --pins <map>`).** The wrapper module is named
after the pin-map file stem; each wrapper port is a single 1-bit pin named by the
sanitized path (`btn.inc`→`btn_inc`, `seg[0][6]`→`seg_0_6`), and the PCF maps that
port name to the pin number. Paths resolve against the elaborated leaf names:
`btn.inc`→ inner input `btn_inc`; `seg[0][6]`→ bit 6 of observation output
`seg_0_q`. A trailing `[i]` is **always** the bit index (a leading `[i]` indexes
the array), resolved directly so a path can never silently bind a sibling
register. Because the map has **no reset pin**, the wrapper synthesizes a
**power-on reset** — a counter (init via a configuration-time `initial`, held low
until it saturates: 15 clocks) drives the inner `rst_n`. Polarity for `low` pins
is inverted once at the boundary. To stay Verilator-`-Wall`-clean with no
pragmas, *every* observation output is connected (an output left unrouted would
be `PINMISSING`/`PINCONNECTEMPTY`), 1-bit taps are not bit-selected, and bits not
sent to a pin are tied into a single `_unused`-named sink (exempt from
`UNUSEDSIGNAL`). The wrapper rejects a path flattening to a reserved internal
name (`rst_n`/`por_count`/…) and a physical pin bound twice.

---

## 8. Prior Art to Mine (read before fixing signatures)

| Project | What to take | Relation |
|---|---|---|
| **Spade** | Rust-flavored standalone HDL; **pipeline-as-type** already built; cycle-to-cycle model; custom compiler + LSP + build system; combinatorial-by-default with `reg` for sequential | Closest overall prior art; has the *pipeline* half but **not** the *multiplex* half |
| **Clash** | `state -> state` Mealy model lifted by a `mealy` combinator; bit-accurate by construction | The §2 transition-function model |
| **Filament** | **Timeline types** for safe pipelining / latency contracts | Directly relevant to §3.4 latency-as-type; read before fixing signatures |
| **Bluespec** | Guarded atomic rules; scheduler proves one-rule-fires; cleanest existing model for "one arm fires/cycle, exclusion proven" | The §3.2 multiplex exclusion side |
| **SpinalHDL / Chisel** | Mature `Stream`/`Flow` channel patterns; elastic interfaces | §3.1 backpressure, §6.6 channels |
| **Veryl** | Rust-syntax HDL; transpiles to SystemVerilog | Syntax / tooling reference |

**Novel slice** (worth stating in the README): nobody known has **both**
pipeline and multiplex as first-class function colors **unified by latency
contracts** (Filament-style) with a const/runtime selector binding-time knob.
Spade has pipeline-as-type; Bluespec has the rule-exclusion side; Filament has
the timeline types — Sinter combines them.

---

## 9. Suggested Milestones

1. **M0 — Spec skeleton.** Lock the name, file extension, CLI name. Write the
   normative grammar spec (EBNF) and resolve the §6.1 sign-bit convention and the
   §4.4 / §6.9 open questions on paper. Choose the §7.1 parser strategy.
2. **M1 — Core front end.** Lexer + error-tolerant parser → lossless tree → AST;
   width inference; single-driver + acyclicity checks; fixed-point + `iN`/`uN`
   types; mode-named narrowing casts; `?`/valid; `Option`/`enum`.
3. **M2 — Rust simulator backend.** Combinational `fn` + clock-root only; binary
   trace emission. Establish the trace-diff CI harness early.
4. **M3 — Verilog backend.** Same-source dual lowering; wire up the
   sim-vs-Verilog trace-diff oracle (§7.4) into CI.
5. **M4 — Staged colors.** `pipeline fn` (free-running `Valid<T>` default;
   elastic `Stream<T>` opt-in) and `multiplex fn` (const/runtime selector;
   `Static`/`Bounded`/`Dynamic` latency; explicit shared resources). Clock-domain
   phantom types + CDC primitives. Memories with port-count + read-latency
   contract.
6. **M5 — Tooling polish.** tree-sitter grammar + VS Code LSP (incremental,
   error-tolerant). Build system / dependency management (cf. Spade).
7. **Future — nextpnr path / native synth / spring placer.** Distinct, deferred.

### 9.1 Functional-surface lowering — ACTIVE milestone (F1–F5)

The functional surface (§2.1) parses today (`examples/go_board_counter.snt`).
This milestone makes it **lower** to the cycle-accurate Rust simulator *and*
synthesizable SystemVerilog that agree under the §7.4 oracle — graduating the Go
Board example from "parses" to a passing conformance test.

**Strategy.** Lower the functional surface to the **existing flat `ir::Module`**
(ports / regs / topo-sorted lets / `out_drives` / `reg_updates`) so the simulator,
Verilog backend, and oracle are reused unchanged. All new work is a front-half
elaboration: state-struct → leaf registers, `match` → `if`-mux, method/free-fn
calls → inlined bodies (single-step rule), struct/tuple/array values → scalarized
leaves (SROA). The IR gains only a mode-named `Cast` node and mixed-width
arithmetic (operand extension); everything else lowers to the existing nodes.

| Phase | Scope | Oracle status |
|---|---|---|
| **F1** | Resolve + type-check: symbol tables, name resolution, width inference, cast checking, single-clock-domain, single-step, acyclicity. No lowering. | unchanged; `check` reports real diagnostics |
| **F2** | Elaborate to `ir::Module`: state flattening, SROA, free-fn inline, `Cast`+mixed-width IR extension. | a small functional example passes the oracle |
| **F3** | enum encoding + `match`→mux, sub-state method inlining, nested state, arrays, tuple destructuring. | **the Go Board example passes the oracle** |
| **F4** | ✅ fixed-point `uI.F`/`iI.F` arithmetic (`+`/`-`/`*`, the `*` operator, IR `Shl`/`Shr`) erasing to integers; the full `wrap`/`sat`/`trunc`/`round` cast set (§6.3); Verilog emits explicit operand extension. | fn_fixed/fn_sat/fn_round pass the oracle |
| **F5** | ✅ pin map (`.pins`) → top-level Verilog wrapper + nextpnr PCF: `sinter build --pins` resolves each `set_io` path against the elaborated module, synthesizes a one-port-per-pin wrapper (power-on reset, polarity inversion for `low`, observation outputs tapped/sunk lint-clean) and emits the matching PCF. | `go_board.pins` wrapper synthesizes (Yosys) and lints clean (Verilator `-Wall`) |

**Locked decisions (this milestone):**
- The oracle observes **all leaf state registers** (full-state trace), plus the
  `Outputs` value for a `(Self, Outputs)` transition function.
- The clock-root `tick` may return `-> Self` (outputs are observed state fields)
  **or** `-> (Self, Outputs)` (a distinct output channel; cf. §4.4).
- Struct reset/default values const-eval over **literals and `const` references**
  (const-fn-in-default deferred).
- **Single clock domain only** (phantom-domain CDC deferred); memories and the
  `pipeline`/`multiplex` colors are out of scope (M4/§3, §6.6).

### 9.2 Source modules & visibility — next milestone

A single `.snt` file does not scale to a real design, and a shared codebase needs
enforced interface boundaries. This milestone adds the source-level **module
system** (§10) — the structural floor the `pipeline`/`multiplex` work (§3) will
sit on. It is deliberately sequenced **before** those colors: it is orthogonal to
the compute model, low-risk (Rust's design is proven), and it stabilizes the
cross-file name resolution the staged-color semantics will build on. Scope is
**in-project multi-file + visibility only**; the external dependency/package
layer is deferred.

| Phase | Scope | Done when |
|---|---|---|
| **S1** | ✅ Grammar + AST: `mod` / `use` / `pub` (private-by-default `Visibility` on items + fields), `use` paths with `self` / `super` / `root` qualifiers + `*` glob; the Rust-2018 file loader (`foo.snt`, children in `foo/bar.snt`, never `mod.snt`) producing a `ModuleTree`. (The single-file compile path rejects `mod` / `use` until S2/S3.) | `load_project` builds the tree; corpus + loader tests pass |
| **S2** | ✅ Cross-module name resolution + **item** visibility: `resolve_project` lowers the module tree to one flat program with canonical (`mod::name`) names, resolving `use` (bare = root-relative, `self`/`super`/`root`, `::*` globs with Rust shadowing precedence) and qualified paths, enforcing private-by-default item access (reachable iff `pub` or from a descendant). Same-named items in different modules coexist; the `#[clock]` root struct is required to be in the root module. Struct **field** visibility (read + construction) and **method-call** visibility are enforced too — a private field/method is reachable only from its struct's module or a descendant. `pub use` re-export outward and pub-*path* privacy are deferred. | F1 checks pass across files; private item/field/method access is rejected (`compile_project`) |
| **S3** | ✅ Pipeline integration: `sinter check` / `build` / `sim` route through `compile_project` (load + resolve + check + elaborate); a single file with no `mod`s behaves exactly as before. | the Go Board split across files elaborates to **byte-identical** SystemVerilog and an identical reference trace (refactor-invariance) |

The S3 bit-for-bit check is the key correctness lever: splitting a design across
files is a pure refactor with zero effect on the elaborated IR or the trace —
verified by `tests/project.rs` (single-file vs `tests/fixtures/go_board_split/`).

### 9.3 Pipeline functions — ACTIVE milestone (PL1–PL3)

The first staged function color (DESIGN §3.1). Settled surface (this milestone):

- **`pipeline fn`** with **bare `yield;`** stage boundaries; locals live across a
  boundary are auto-registered. Outputs are **final-only** (the tail); mid-pipeline
  `yield expr;` taps are not part of the model (the parsed form is rejected).
- **Driving:** a pipeline instance is ordinary state — a field of the pipeline's
  type in the state struct, advanced one beat per cycle via a generated
  `advance(self, in…, in_valid: u1) -> (Self, Out, u1)`, reusing the sub-state +
  `(new_state, output)` machinery (§2.2/§4 explicit ownership). The honest Mealy
  step lives in this *generated* method; the source `fn` stays straight-line. This
  is the concrete erasure of the conceptual `advance(Option<In>) -> Option<Out>`:
  validity is a trailing `u1` on both sides until the `Option<T>` sugar lands.
- **Validity = `Option<T>`** (DESIGN §6.5): `None` is a bubble, `Some` a datum.
  This *replaces* a separate `Valid<T>` — they were the same "maybe a value"
  concept. It erases to a valid bit + payload, which is exactly the concrete
  `in_valid`/output-`u1` above. The `Option<T>` *type* (with `Some`/`None`,
  `match`, and `?` bubble-propagation, §6.4) needs type-parameter support Sinter
  lacks and is its own milestone; the pipeline uses the concrete representation
  meanwhile.
- **Reset is fully defined:** valid bits → empty, payload registers → zero, so
  the fill region is defined-but-ignored (oracle-safe). The output is `None` for
  the first `latency` cycles after reset; fill latency and bubbles share the one
  valid-bit mechanism.
- **Backpressure (phase 1) = the global-stall tier (§3.1)** and it falls out of
  the model for free: the caller reads the output's validity and either commits
  the advanced state or **holds** the old one (a guarded hold, §6.4) — freezing
  the whole pipeline. Elastic `Stream<T>` (per-stage ready/valid + skid buffers)
  is the deferred opt-in tier.

| Phase | Scope | Done when |
|---|---|---|
| **PL1** | ✅ Grammar + AST + parse: `pipeline`/`multiplex` function colors, `yield;` / `yield expr;` stage statements. (Compiler rejects them as not-yet-lowered.) | a `pipeline fn` parses into the AST |
| **PL2** | ✅ Semantic checks: a `pipeline fn` type-checks across `yield` barriers (type-transparent); structural rules — free function only (no `self`/method), top-level `yield` only (no nesting), ≥1 stage; the hazard rule holds structurally (no `self` ⇒ inputs/consts/locals only); a pipeline cannot be called directly (instantiate + advance, PL3); `multiplex` and `yield`-taps are rejected as deferred. | a well-formed pipeline type-checks; misuse is rejected |
| **PL3a** | ✅ Elaborate the **free-running** pipeline by **desugaring** a `pipeline fn` to a synthesized instance `struct` (a valid bit per stage + a register for every value live across each `yield`, SROA-flattened into the parent) and a generated `advance(self, in…) -> (Self, Out, u1)` — the existing sub-instance / `(new_state, output)` / method-inlining machinery then lowers it with no IR/sim/Verilog changes. Validity is concrete: a trailing `u1` (Option erased to valid-bit + payload); output is invalid for the first `latency` cycles (defined-`0` reset = "None during fill"); `v0` next-state is the literal `1` (free-running — input validity is PL3b). Carried `let`-locals need an explicit cast or type annotation to pin their register width (phase-1). Through the sim↔Verilog oracle (`examples/fn_mac_pipeline.snt`). | a single-output pipeline example passes the oracle bit-for-bit |
| **PL3b** | ✅ Input bubbles: `advance` gains a trailing `in_valid: u1` (`v0` next-state = `in_valid` instead of the literal `1`), so `in_valid = 0` injects a bubble the valid-bit chain carries to the output `latency` cycles later — a bubble in ⇒ a bubble out at the right latency; a free-running caller passes `1`. `in_valid` is a reserved pipeline name. (`?`-propagation needs a real `Option<T>` type and is promoted to its own milestone — see below.) | a gated pipeline with random `in_valid` matches the oracle bit-for-bit |
| **PL3c** | ✅ Global-stall backpressure as a **guarded hold** — it falls out of the owned-instance model with **no new compiler support**: the parent re-commits the *previous* instance instead of the advanced one (`if en { Top { inst: inst2, … } } else { Top { inst: self.inst, … } }`), so every flattened pipeline register's next-state becomes `mux(en, advanced, current)` — freezing the whole pipe together when `en = 0`. (Per-stage elastic `Stream<T>` backpressure is the deferred opt-in tier.) Through the sim↔Verilog oracle with random `en` (`examples/fn_pipeline_stall.snt`). | a stalled pipeline holds its state, oracle-checked |

Deferred beyond PL3: elastic `Stream<T>` (per-stage ready/valid + skid buffers)
and the §3.5 `Bounded`/`Dynamic` latency prover. Mid-pipeline `yield expr` taps
are **cut** from the model (final-output-only). The `Option<T>` type + the `?`
operator are their own milestone (§9.4); the `multiplex` color is §9.5.

### 9.4 `Option<T>` type — ACTIVE milestone (O1–O2)

A first-class **`Option<T>`** (DESIGN §6.4/§6.5): the "maybe a value" type that
underlies the pipeline validity model, broadly useful beyond pipelines (channel
reads, lookups, guarded results). Settled surface:

- **Syntax (Rust-style):** the type `Option<T>`; the constructors `Some(x)` and
  `None`; `match o { Some(v) => …, None => … }`; (O2) the `?` operator.
  `Option` / `Some` / `None` are reserved keywords (program-wide, including the
  otherwise-frozen legacy `module` surface, since both share one identifier rule).
- **`T` is any concrete type** — scalar, fixed-point, `struct`, `enum`, tuple, or
  array. `Option<T>` **erases** to a 2-leaf aggregate `(is_some: u1, payload: T)`,
  reusing the existing SROA / flatten / mux machinery, so nothing in the IR,
  simulator, or Verilog backend changes. `None` is `(0, zero(T))` — a fully
  defined reset/payload (oracle-safe); the payload of a `None`/non-taken arm is
  computed but gated by `is_some`.
- **`match` lowers to a mux** on `is_some` (the same per-leaf multiplexing as any
  match); exhaustiveness requires `Some` and `None` (or a `_`). `Some(v)` binds
  the payload; `None` matches the empty case. Option patterns are refutable, so
  they are a `match`-only form (not a `let` binding).
- **`?` (O2)** is *valid-bit propagation*, not control flow (§6.4): in a
  combinational Option-returning body, `let v = e?` yields `e`'s payload
  unconditionally and AND-reduces `e`'s `is_some` into the result's validity. It
  is only valid in an `Option`-returning context; `reg <= compute()?` is rejected
  (a register has no "skip"; use a guarded hold).
- **Not in scope:** general user-defined generics and payload-bearing `enum`s
  (`Option` is the built-in one-variant-payload special case, monomorphized per
  `T`); retrofitting the pipeline `advance` to return `Option<Out>` instead of the
  concrete `(payload, u1)` is a later follow-on.

| Phase | Scope | Done when |
|---|---|---|
| **O1** | The `Option<T>` type + `Some`/`None` + `match`-on-Option, in combinational code: grammar/AST/parse, type checking (typing `Some`/`None`, exhaustive `match` with payload binding), and elaboration (erase to `(is_some, payload)`; `match` → mux). Through the sim↔Verilog oracle. | an `Option` example (construct + match) passes the oracle bit-for-bit |
| **O2** | ✅ The postfix `?` operator (`Expr::Try`): in an `Option`-returning combinational body, `e?` yields `e`'s payload unconditionally and AND-reduces `e`'s `is_some` into the function's result validity (no control flow, DESIGN §6.4) — elaboration accumulates the `?` validity bits per inlined function and folds them into its `Option` result. Rejected outside an `Option`-returning function (so a register next-state cannot use it — a register has no "skip"; use a guarded hold) and on a non-`Option` operand. | a `?`-using helper (validity AND-reduction) passes the oracle (`examples/fn_option_try.snt`) |

### 9.5 `multiplex fn` — ACTIVE milestone (M1)

The second staged function color (DESIGN §3.2/§3.3): the **time-folded** dual of
the pipeline. Same surface — `yield`-separated phases, an owned instance +
`advance` — but it folds in time. Settled M1 surface:

- **`multiplex fn`** with `yield` phase boundaries; const selector (a phase
  counter `ph`). Where a pipeline runs every stage every cycle (a depth-`N`
  register chain, 1 result/cycle), a multiplex processes one datum at a time over
  `N+1` phases using **one register per carried value** (written in its producing
  phase, held otherwise) plus the small counter — **latency `N`, throughput
  ~1/(N+1), static** (§3.4 `Static`). Reuses the pipeline liveness/desugar
  machinery (`multiplex_desugar` mirrors `pipeline_desugar`).
- **Operator sharing — the one-DSP win (§3.2):** each phase may use **at most one
  `*`** (rejected otherwise); the desugar hoists the per-phase multiply operands
  into phase-muxes and emits a single shared multiply. Because the elaborator
  inlines `let`s by value, an IR-level multiply-CSE pass (`ir_cse::share_muls`,
  run only for multiplex designs) then collapses the (SROA-duplicated) shared
  multiply into **one physical multiplier**. Mutual exclusion (one phase/cycle)
  makes the sharing free/structural — no cost model.
- **Driving** is free-running like the pipeline: `advance(self, in…, in_valid: u1)
  -> (Self, Out, u1)`; inputs sampled at phase 0, `valid = (ph == N)`. Defined
  reset (`ph = 0`, every latch 0). `ph` / `shared` / `in_valid` are reserved names.

| Phase | Scope | Done when |
|---|---|---|
| **M1** | ✅ Time-folded `multiplex fn` (const selector) + **one shared multiplier** via operand-hoisting + IR multiply-CSE. One register per carried value + a phase counter; `≤1 *` per phase (rejected otherwise). Through the sim↔Verilog oracle (`examples/fn_dot2_multiplex.snt` power-of-two period, `fn_dot3_multiplex.snt` non-power-of-two wrap-to-0). | a dot-product multiplex passes the oracle bit-for-bit with exactly one multiplier in the IR |

Deferred beyond M1: `k`-way operator sharing (>1 unit), sharing cheap ops, a clean
per-phase operand-width check, and the **runtime selector** (data-dependent FSM,
§3.4 `Bounded`/`Dynamic` with the §3.5 latency-bound prover).

### 9.6 Memories — ACTIVE milestone (MEM1)

The first **ported memory** primitive (DESIGN §6.6/§6.9): a `mem<T, N>` struct
field that lowers to inferred block RAM, distinct from the `[T; N]`
vector-of-registers. Settled MEM1 surface and semantics:

- **`mem<T, N>`** is a struct field whose type is parsed/stored as a fully
  general `MemType { elem, depth, read_ports, write_ports }` with the port counts
  **fixed at 1R1W** in MEM1 (any other count rejected "not yet lowered"), so
  `mem<T, N, R, W>` is purely additive later and no source breaks. `T` is a
  scalar/erasable integer (width ≤ 64); `N` (depth) must be a power of two so the
  address mask is exact in both backends.
- **`value()` / `next()` access** — a memory is a sub-state-machine, accessed
  method-style (reusing `Expr::MethodCall`):
  - **`value() -> T`** reads the memory-owned read-data register (a pure
    accessor); and
  - **`next(raddr, we, waddr, wdata) -> mem<T, N>`** is the transition: schedule a
    read of `raddr` and, when `we`, write `wdata` to `waddr`. It returns the
    memory, threaded into the tail `Self { m: self.m.next(…) }` exactly like a
    sub-FSM step.

  Index-style is rejected (the surface has no statement-level assignment, and
  `m[addr]` is the `[T; N]` read). (The `we`-flag write argument will become an
  `Option<(addr, value)>` once `Option<T>`, §6.5, is available on this line.)
- **Read latency = `Static(1)`, memory-owned register.** `value()` returns the
  value addressed by the `raddr` given to `next()` *last* cycle; the read-data
  register `<m>_rdata` is an ordinary register whose next value reads the pre-edge
  array. This matches the DP16KD output flop and is byte-identical to the
  pipeline's carry-across-a-cycle move — it composes with the single-step
  transition rule with **no timing-model change** (a consumer treats read-data
  like any registered value). `value()` being a pure read means it never consumes
  the single read port — the read is scheduled by `next()`.
- **`READ_FIRST` collision policy.** A read and a write to the same address in the
  same cycle ⇒ the read returns the **old** value. This falls out of the
  simulator's frozen-pre-edge-`env` model for free (the read register samples the
  pre-edge array; the write commits to a scratch clone), needs no comparator, and
  is the Yosys/DP16KD inference default — so sim and the inferred-BRAM Verilog
  agree bit-for-bit (§7.4) by construction.
- **Zero-init contents** (a sim/Verilator contract). Every word resets to `0`; the
  Rust sim seeds the array and the emitted SV initializes it (so Verilator does
  not `X` on a read-before-write). On real silicon the array has no async content
  reset — hardware relies on the read register's reset + write-before-read; only
  `<m>_rdata` resets. The array itself is opaque state (never exposed as a `_q`
  observation port); the oracle diffs the auto-exposed `<m>_rdata_q` output.
- **Verilog.** Emit `logic [W-1:0] <m> [0:N-1];`; the read-data register's next
  value is the combinational array read (`<m>_rdata_next = <m>[raddr];` in
  `always_comb`, flopped by the normal async-reset state block, giving the
  registered `Static(1)` read), and the write is a **separate** `always_ff
  @(posedge clk)` (`if (we) <m>[waddr] <= wdata;`) — kept out of the async-reset
  block so BRAM inference survives. Only `<m>_rdata` resets. MEM1 relies on Yosys
  inference (no explicit primitive).
- **Reset / write contract.** Like real block RAM the **array has no content
  reset** — a write commits whenever `we` is asserted, *including during reset*.
  The simulator has no reset cycle (it begins post-reset) and the oracle holds
  inputs at `0` during the reset pulse, so the two agree; the contract is that a
  caller holds **`we = 0` while the design is in reset**. (A synchronous `rst_n`
  gate on the write trips Verilator `SYNCASYNCNET`; a reset-release flag would drop
  the first post-reset write and break the oracle — so the array is simply
  unreset, matching the hardware.)
- **Threading is the single-driver.** A memory's transition (the read schedule +
  the `we`-gated write) is carried as a *pending* effect on the handle `next()`
  returns and committed **only when that handle is threaded into the next-state**
  (`Self { <m>: self.<m>.next(…) }`). A discarded `next()` is inert (no write, no
  read), so "a write-port is driven once per cycle" (§4.4) really is the existing
  single-driver / single-next-state rule — `1R1W` falls out structurally (one
  threaded handle = one read port + one `we`-gated write), with no separate budget
  check. `next()` may not appear inside an `if`/`match` arm (its handle can't be
  muxed — gate the write with `we` and mux the address explicitly); `value()`, a
  pure read, is unrestricted.

| Phase | Scope | Done when |
|---|---|---|
| **MEM1** | ✅ Single-clock **1R1W** synchronous `mem<T, N>` (scalar `T`, power-of-two `N`): `Static(1)` memory-owned registered read, `READ_FIRST` collision, zero-init contents. New `ir::Mem` (guarded out of SROA) + read-data register + write phase; sim/Verilog both commit from the single pre-edge `env`. Through the sim↔Verilog oracle (a delay-line / small-RAM example). | a write+read RAM example round-trips through the oracle bit-for-bit (accounting for the 1-cycle read latency) |

Deferred beyond MEM1: multi-port / true-dual-port (`R`/`W` ≠ 1), read-during-write
*modes* (`WRITE_FIRST`/`NO_CHANGE`), byte-enables, dual-clock memory (the §6.7
phantom-domain interaction), `Bounded`/`Dynamic` read latencies, 0-cycle
LUTRAM/distributed reads (behind an explicit annotation), explicit
`DP16KD`/`PDPW16KD` instantiation (wide / asymmetric / true-dual-port),
and non-scalar (struct/enum/fixed-point) element types. (ROM / init-from-data is
promoted to its own milestone — §9.7.)

### 9.7 ROM — ACTIVE milestone (ROM1)

A **read-only memory** `rom<T, N>` with compile-time contents (DESIGN §6.6/§6.9):
the const dual of `mem<T, N>`. A ROM is just a memory whose array is initialized
from data and never written — so it reuses the entire MEM1 substrate (`ir::Mem`
with its per-word `reset` data, the `Static(1)` read-data register, the inline
array init, the sim↔Verilog oracle). The new mechanism is *getting data in at
compile time*; everything else is MEM1.

A ROM is the **cursor-over-a-const-container** the data/state split suggests: the
container (`ir::Mem.reset`) is fixed at compile time, the cursor (`<m>_rdata`) is
the only mutable state.

Settled ROM1 surface and semantics:

- **`rom<T, N>`** is a struct field — the same `mem` type machinery with the write
  port removed (parsed as `mem` with `write_ports = 0`). `T` is a scalar (≤ 64
  bits), `N` a power of two ≥ 2 (the address masks exactly), exactly as `mem`.
- **`value()` / `next(raddr)`** — the `mem` interface minus the write arguments.
  `value() -> T` reads the read-data register (the word addressed last cycle,
  `Static(1)` latency); `next(raddr) -> rom<T, N>` schedules a read and is threaded
  into the tail `Self { m: self.m.next(raddr) }` (the same threading-is-the-driver
  rule — but a ROM has no write to drive, only the read cursor advances).
- **Contents** are the field's initializer (compile-time const, exactly `N`
  words):
  - an **inline array literal** `rom<u8, 4> = [1, 2, 3, 4]` (reuses array const-
    folding); or
  - **`include_hex!("file.hex")`** — a file of **line-delimited hex words** (one
    per line, optional `0x`/`0X` prefix, blank lines and `//` comments skipped),
    read at compile time **relative to the source `.snt` file** (like Rust's
    `include_str!`). This is a strict subset of the `$readmemh` format, so the
    files are interchangeable with that ecosystem.
- **Compile-time parse, inline emit.** The initializer is parsed *once* into the
  `ir::Mem.reset` data vector, and emitted **inline** in both backends (the Rust
  sim seeds the array; the Verilog `initial` block assigns the data per word, which
  Yosys turns into the BRAM/ROM contents — §9.6). The emitted SystemVerilog is
  **self-contained** — there is no runtime `$readmemh`, so synthesis needs no
  external data file or build-system path plumbing, and the §7.4 oracle holds by
  construction (both backends derive from the one parsed vector).
- **Size / width checks.** The initializer must yield exactly `N` words (a count
  mismatch is a compile error), and each word must fit `T` (an out-of-range value
  is a compile error). Addresses, reads, and reset (`<m>_rdata` resets to 0) work
  exactly as `mem`.

| Phase | Scope | Done when |
|---|---|---|
| **ROM1** | ✅ `rom<T, N>` read-only memory (`value()` / `next(raddr)`) with compile-time contents from an **inline array literal** or **`include_hex!`** (line-delimited hex, source-relative), parsed into `ir::Mem.reset` and emitted inline. Reuses the MEM1 read path; size/width-checked. Through the sim↔Verilog oracle (a lookup-table ROM). | a ROM example reads its initialized contents through the oracle bit-for-bit |

Deferred beyond ROM1: **`include_bin!`** (a raw binary blob — needs a chosen
endianness and an 8-bit-multiple element width to slice into words, so it is a
distinct feature), **inferred depth** (`rom<T, _>` sized by the data), other
container formats (Intel HEX, SREC, `.coe`, `.mif`), an optional runtime
`$readmemh` / generated-data-file emit mode for very large ROMs, and multi-read
ROMs (the shared-data / multiple-cursor split — folds into the §9.6 multi-port
axis).

### 9.8 Channels / FIFO — proposed milestone (CHAN1–CHAN4)

The first **fixed-capacity channel** (§6.6 / §6.11): a queue struct field with the
`mem`-style `value()`/`next()` interface, resolving §11-#5 (v1 = ready/valid +
no-corruption, not deadlock-freedom). Shallow queues are a **source-to-source
desugar** (the `pipeline`/`multiplex fn` mechanism) into `struct { data:[T; N],
head, count }` + a synthesized `next` — SROA-flattened, no new IR, no new backend
emitter — so CHAN1 is a near-mechanical clone of the landed MEM1 path.

| Phase | Scope | Done when |
|---|---|---|
| **CHAN1** | ✅ Single-owner **lossless** `fifo<T, N>` (scalar `T` ≤ 64 b, power-of-two `N`): `next(push: Option<T>, pop: u1)` + pure `front()`/`is_empty()`/`is_full()`/`len()`. Register-backed desugar, SROA-flattened, `Static(0)` combinational `front()`, READ_FIRST same-cycle push+pop, empty/zero reset. `next()` barred from `if`/`match` arms (single-driver); reads unrestricted. | a producer→queue→consumer example (`fn_fifo.snt`) round-trips the §7.4 oracle bit-for-bit |
| **CHAN2** | **Lossy** `ring<T, N>` (overwrite-oldest) on the same desugar; loss policy fixed by the type. `flow<T>` (depth-1 newest-wins) only if an example needs it. | a `ring` overwrite-oldest example oracle-passes; `fifo` goldens unchanged |
| **CHAN3** | Pure directional `Sender<T, N>` / `Receiver<T, N>` views + the §3.1 connection rule: wiring a non-backpressure-aware producer to a `Sender` is a **type error** (where "lossless" earns compile-time teeth for the wired graph). | a cross-module producer/consumer example oracle-passes; an ungated producer is rejected at type-check |
| **CHAN4** | Deep **BRAM-backed opaque** `fifo` IR primitive (guarded out of SROA, new side-vector + Verilog emitter + sim path); `front()` becomes a registered `Static(1)` first-word-fall-through read. Gated on depth. | a deep FIFO oracle-passes accounting for the 1-cycle read latency |

Deferred beyond CHAN4: multi-producer/-consumer arbitration (the §5
`merge_exclusive`/`merge_priority` combinators), almost-full/almost-empty
watermarks, dual-clock (async) FIFOs (the §6.7 phantom-domain interaction — a CDC
primitive), non-scalar element types, and any **deadlock/liveness** guarantee
(§6.6's hard tier). The elastic `Stream<T>` (§3.1) is built *on* CHAN1/CHAN3.

### 9.9 Fault flags — proposed milestone (FLT0–FLT2)

A one-way **sticky fault** mechanism (§6.12): latch a "should never happen"
condition, aggregate to a code naming *which* site, and optionally compile the whole
system out — while keeping the sim ↔ Verilog oracle bit-identical *within* a build
mode.

| Phase | Scope | Done when |
|---|---|---|
| **FLT0** | Sim-only `assert!(pred, "msg")` / `debug_assert!` over `#[clock]`-reachable bodies: a pure `u1` predicate, a sim-node abort gated on `~pred`, **zero** emitted bits in either backend (§6.4 precedent). | an example whose `assert!` fires makes the conformance sim fail, and the emitted SV is byte-identical with and without it |
| **FLT1** | `FaultSet<Fault>` carried into silicon: a built-in parameterized type over a user `enum` (≤ 64 variants, explicit discriminants), `= {}` empty reset; `.raise(cond, F) -> FaultSet<Fault>` threaded advance (single-driver, no-`if`/`match`-arm, chainable `q\|mask`); pure `first() -> Option<Fault>` / `any()` / `contains(F)`; the **observation-only** taint rule in `functional.rs`; erased to a sticky-OR `uN` register (SROA-flattened, `<field>_q` exposed); `--faults on\|off` read once at `ir::Design` build (`off` folds to identity + DCE, sideband tied off). | an example that raises on data-dependent conditions round-trips the oracle bit-for-bit with `--faults=on`, and its functional outputs are identical (and `_q` absent) with `--faults=off` |
| **FLT2** | Static buckets behind a prover: `assert_static!(p)` (SMT discharge, compiled out) and `unreachable!()` (must-prove-dead else compile error), with a **loud** `#[assume]` escape that demotes to a sim-only `debug_assert!`. | an unprovable `assert_static!` is a compile error (or a loud demotion); a provable one emits no bits |

Deferred beyond FLT2: clear/re-arm (a distinct `FaultReg<Fault>` — clearing reopens
the skip-vs-hold question); a `#[fault]` design-level roll-up attribute (the
*edit-invariant* `any()` aggregate only, never an elaboration-ordered code);
non-`u1` (payload-bearing) faults.

### 9.10 Bit manipulation — proposed milestone (BIT1–BIT2)

Surface bit-slice, shift, and concatenation (§6.13) — the highest-leverage datapath
primitives and a prerequisite for instruction decode (§9.12 / the RV32 example).
All width-explicit; the constant-amount cases reuse the IR `Shl`/`Shr` already
exercised by fixed-point (F4).

| Phase | Scope | Done when |
|---|---|---|
| **BIT1** | ✅ Constant-bound bit-slice `x[hi:lo]` (type `u{hi-lo+1}`), constant-amount `<<`/`>>` (folding to IR `Shl`/`Shr`, arithmetic `>>` for signed), and concatenation `a @ b` (type `u{wa+wb}`). Grammar + AST + typecheck + elaborate; no new IR node. | slice/shift/concat examples pass the oracle; a field-extract + reassemble example round-trips |
| **BIT2** | ✅ **Variable** (barrel) shift `x << sh` / `x >> sh` with a runtime (non-literal) `sh`: a new IR `VarShift` node, **same-width** result, arithmetic vs logical by the operand's signedness, `sh >= width` → 0 / sign fill. Reuses the `apply_shl`/`apply_shr` helpers (Rust sim) and a native Verilog barrel shift (`<<` / `>>` / `>>>`). | `fn_barrel.snt` (`SLL`/`SRL`/`SRA` by a runtime amount, `sh` past the width) passes the oracle bit-for-bit |

Deferred: dynamic-bound bit-slice (a barrel shift + mask), bit-reverse / `popcount`
/ `clz` intrinsics, and `sext(x, k)` sugar (expressible via `@` + cast).

### 9.11 Runtime-indexed register file — proposed milestone (REG1)

Runtime indexing of a `[T; N]` vector-of-registers (§6.14) — the register-file
enabler. A runtime read lowers to a combinational N:1 mux tree; a runtime write (one
field of the returned state) to per-element guarded holds under a 1:N write-index
decode. Reuses the SROA leaf-register machinery; no new IR primitive.

| Phase | Scope | Done when |
|---|---|---|
| **REG1** | ✅ Runtime read `a[i]` (combinational N:1 mux tree) and functional-update write `a.set(i, v)` (per-element `mux(i==k, v, a[k])` — the guarded-hold decode when gated by an `if we`) on a vector-of-registers, `i` a runtime index; multiple runtime reads = multiple mux trees. Purely `functional` + `elaborate` (reuses `Index` / `MethodCall` syntax — no new AST). | a 2R1W register-file example (`fn_regfile.snt`, hardwired-zero slot) round-trips the oracle bit-for-bit |

Deferred: the BRAM-backed 2R1W path (`mem<T, N, R, W>`, the §9.6 multi-port axis),
and byte-enable / sub-word masked writes (§9.6) needed for sub-word stores.

### 9.12 RV32 soft core — proposed flagship example (sequencing)

A 5-stage RV32I core with block-RAM work RAM is the intended flagship — the largest
program the language will have expressed (today's corpus tops out ~200 lines) and
the forcing function that motivated §9.10 / §9.11. It is **not** expressible until
those land; it is sequenced deliberately, each step a passing oracle example before
the next:

1. **§9.10 BIT1/BIT2** — bit-slice/shift/concat for instruction-field extraction,
   immediate assembly, and the `SLL`/`SRL`/`SRA` barrel shifter.
2. **§9.11 REG1** — the `x0`-hardwired 2-read/1-write register file.
3. **§9.8 CHAN1** — a skid buffer between stages (also generally useful).
4. **§9.9 FLT1** — illegal-instruction / misaligned-access faults.
5. **Stepping-stone core:** a single-cycle (or 3-stage) **RV32I subset** — Harvard
   (`rom` + `include_hex!` instruction memory, `mem<u32, N>` data memory), word-only
   `LW`/`SW`, no CSRs/traps — proving the datapath end-to-end.
6. **Capstone:** the full 5-stage core with hazard detection + forwarding (explicit
   stage-register `struct` fields).

**Note — the pipeline color is the wrong tool.** A CPU pipeline touches shared
mutable state (register file, forwarding) and reintroduces RAW/WAW/WAR hazards,
which §3.1 keeps a `pipeline fn` pure of by construction (it takes no `self`). The
core is a hand-rolled Mealy machine of explicit pipeline-register fields in the
`#[clock]` root.

**Note — stimulus.** The §7.4 oracle drives uniform-random inputs per port each
cycle, which would scramble instruction fetch. The core takes its program from a
`rom`/`include_hex!` and minimal `tick` inputs; a directed / program-driven stimulus
harness (beyond the random default) is a companion tooling item worth adding for any
large example.

---

## 10. Source Organization — Modules & Visibility (SETTLED shape)

> **Realized (S1–S3, §9.2).** Parsing + the Rust-2018 file loader (S1), cross-
> module name resolution + `pub`/private visibility for items, struct *fields*,
> and *methods* (S2), and CLI/pipeline integration with proven refactor-
> invariance (S3) are implemented. `pub use` re-export outward and full pub-*path*
> privacy remain deferred.

The functional surface (§2.1) is authored as `struct` state + `impl` methods +
free `fn`s. Past a toy size, those must span multiple files with enforced
interface boundaries (especially once more than one person edits a design).
Sinter adopts a **Rust-style module tree**, deliberately split into two layers —
only the first is in scope now:

1. **In-project modules** (this milestone): a tree of source modules within one
   project, with visibility.
2. **External units** (deferred): distributable / dependency packages — Rust's
   "crate" role. The name is intentionally *not* fixed yet (candidates lean
   hardware-native: "core" / "block" / "library"); naming waits until dependency
   resolution is actually built.

**Modules and file layout (Rust 2018).** Every `.snt` file is a module. The
**root module** is the file passed to `sinter build` / `check` (no mandated name;
convention `main.snt` or `<project>.snt`). A `mod foo;` declaration pulls in a
sibling `foo.snt`; that module's own children live in a sibling directory as
`foo/bar.snt` (declared `mod bar;` inside `foo.snt`). There is **no `mod.snt`**
entry-file form — mirroring the compiler's own "never `mod.rs`" rule. Inline
`mod foo { … }` blocks are also allowed for small groupings.

**Paths and imports.** Items are referenced by `::`-separated paths and brought
into scope with `use`:
- `self::` — the current module; `super::` — the parent.
- `root::` — the project root module (the absolute prefix; named `root` rather
  than borrowing Rust's `crate`, since the package layer is deferred and
  unnamed).
- `use root::debounce::Debounce;` imports a name; `use self::display::*;` is a
  glob (allowed). Re-exports (`pub use`) compose a module's public surface.

**Visibility (private by default).** Every item — `struct`, `enum`, `fn`,
`const`, **and struct fields** — is private unless marked `pub`. A private item
is visible within its defining module and that module's descendants (Rust
semantics); `pub` widens it to the parent / outside. Default-private forces
authors to design an explicit public interface for each reusable block — the
encapsulation a multi-person codebase needs. The model is kept **minimal**: just
`pub` vs private for now. Finer-grained forms (`pub(in path)` / `pub(super)`) are
deferred until the external-unit layer exists.

**`impl` is co-located with its type.** An `impl T` block must live in the module
that defines `T` (a mild divergence from Rust, which permits impls anywhere in
the crate). This keeps field-visibility checking sound: the type checker derives
the *accessing* module from the receiver type, so a cross-module `impl` would
otherwise read every private field of the type's module as same-module. Lifting
this (attributing visibility to the impl's own module) is a future option.

```
// main.snt — the root module (passed to `sinter build`)
mod debounce;
mod display;

use root::debounce::Debounce;

pub struct Top { /* … the #[clock] root lives here … */ }
```
```
// debounce.snt
pub struct Debounce {     // exported
    count: u20 = 0,       // field private by default
    level: u1  = 0,
}
impl Debounce {
    pub fn step(self, raw: u1) -> Debounce { /* … */ }  // public interface
    fn saturated(self) -> u1 { /* … */ }                // module-local helper
}
```

**Interaction with the rest of the language.**
- **One synthesizable top, unchanged.** The unique `#[clock(domain = …)]` root
  (§2.1) lives in the root module; there is still exactly one. The module tree
  organizes *source*, not the elaborated flat IR.
- **Pin maps unaffected (§7.6).** A `.pins` file still resolves against the
  single elaborated top; multi-file spreads the *sources* feeding that top, not
  the top itself. The dotted/indexed `set_io` paths are unchanged.
- **Legacy `module` form excluded.** The frozen Verilog-like `module` surface
  stays single-file; modules apply to the functional surface only. (This also
  keeps the `mod` keyword clear of the legacy `module` keyword, which is being
  retired.)
- **Name resolution generalizes the F1 tables.** The existing symbol tables
  extend to module-qualified resolution; nothing in the type / width / cast
  machinery changes.

**Deferred (out of scope this milestone):** the external dependency / package
layer and its name; finer-grained visibility (`pub(in …)`); and any
version / build-manifest format (cf. the M5 build-system line in §9).

---

## 11. Settled Decisions vs Open Questions (quick index)

**Settled:**
- Name = Sinter; ext `.snt`; CLI binary `sinter`; crate & org `sinter-lang`.
- One invocation = one clock cycle; clock-root attribute; the **unique**
  `#[clock(domain = …)]` function is the synthesizable top (§2.1).
- Surface syntax = state `struct` (nestable) with inline reset values
  (`field: T = expr`) + a `self`-taking transition method; combinational
  sub-steps return a `(new_state, outputs)` tuple (§2.1, §2.3). The legacy
  `module` form migrates onto this. Canonical artifact:
  `examples/go_board_counter.snt`.
- Single-driver rule generalized to registers, write-ports, and instances.
- Acyclic per-cycle combinational graph; feedback only through registers.
- Hierarchical state ownership (not flatten-to-root).
- Width-inferred arithmetic; narrowing = explicit mode-named cast (no default).
  Signed multiply is total: `iN × iN → i(2N)`, no min×min special case at the
  multiply — saturation, if any, happens at the explicit cast (§6.2).
- Fixed-point spelled `uI.F` / `iI.F` (dotted); `I` includes the sign bit, total
  width = `I + F` (§6.1). Word-scaled integers deferred (fold into the
  fixed-point binary-point position if revived).
- Enum encoding via `#[encoding(binary|onehot|gray|johnson)]` (default binary),
  plus optional explicit discriminants for protocol matching (§6.5).
- Integer literals untyped by default; optional Rust-style width suffix
  (`_uN`/`_iN`); `0x`/`0b` forms and `_` separators (§6.10).
- `?` = validity propagation only; register writes are guarded holds.
- Merge is always a named combinator; resource sharing is always an explicit
  instance; exclusivity discharged structurally.
- Arrays split into vector-of-registers vs ported memory.
- Fixed-capacity channels = the `fifo` (lossless) / `ring` (overwrite-oldest) /
  `flow` (depth-1) family, `next(push: Option<T>, pop)` + `front()`, threaded like
  `mem` (single-driver); loss policy is in the type; v1 exposes ready/valid and
  guarantees no-corruption, not deadlock-freedom; `Sender`/`Receiver` are
  directional capability views over one owner, not two drivers (§6.11 / §9.8).
- Faults are threaded `FaultSet<Fault>` state (sticky one-hot, `.raise` / `first()`),
  never an implicit global bus; `assert!` / `debug_assert!` are sim-only,
  `unreachable!()` must prove dead; a diagnostic (fault-read) value may reach only
  the trace or a fault sideband, making `--faults=off` provably functional-neutral
  (§6.12 / §9.9).
- Bit-slice `x[hi:lo]`, `<<` / `>>` (constant folds to IR `Shl`/`Shr`; variable = a
  barrel-shift IR node), and concat `a @ b` are the datapath bit primitives (§6.13).
  Runtime `[T; N]` indexing lowers to a mux tree / guarded-hold decode (register
  files), distinct from BRAM multi-port `mem` (§6.14 / §9.10 / §9.11).
- Clock domain as phantom type; CDC crossing without a primitive is a type error.
- Bare `/` forbidden; division via handshaked multi-cycle primitive.
- `pipeline`/`multiplex` as two function colors; selector binding time sets
  latency class; three latency points (`Static`/`Bounded`/`Dynamic`).
- Backends ordered sim → Verilog → (later) nextpnr; native synth is separate.
- Physical pin mapping = PCF-superset binding pins to dotted state/input paths +
  bit/array index + optional `low` polarity; the compiler synthesizes the
  top-level Verilog wrapper and emits the concrete PCF (§7.6).
- Source organization = Rust-style **`mod`** tree; Rust-2018 file layout
  (`foo.snt`, children in `foo/bar.snt`, never `mod.snt`); `use` imports over
  `self::` / `super::` / `root::` paths; **private-by-default**, `pub` to export
  (items *and* struct fields). One `#[clock]` top in the root module. External
  dependency/package layer (and its name) deferred (§10).
- Parser is NOT a single generated grammar driving all tools; grammar is the
  spec/oracle.

**Open (decide on paper before/at M0):**
1. §4.4 — *generator* (multi-cycle, yield + done) signature: separate
   yield/return channels vs collapsed `Step` sum. (The §2.3 combinational
   sub-step shape is now settled as a `(new_state, outputs)` tuple.)
2. §3.5 — prove `Bounded` latency from the transition graph (retain scheduling
   leverage) vs treat all runtime selectors as `Dynamic`.
3. §6.9 — memory as latency-contracted stateful instance vs distinct primitive.
4. §7.1 — tree-sitter shared layer vs rowan + hand-written error-recovering
   parser.
5. §6.6 — **RESOLVED (§6.11 / §9.8):** v1 channels expose ready/valid and guarantee
   no-corruption, not deadlock-freedom (the liveness tier is deferred).
