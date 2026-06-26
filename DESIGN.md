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

**Key safety rule:** connecting a free-running `Valid<T>` source to a consumer
that needs to stall it is a **type error**, not a silicon bug. (Same move as
clock-domain safety: make the unsafe composition unrepresentable.)

**Pipeline hazard rule (load-bearing for verifiability):** a `pipeline fn` may
carry **per-wavefront** state (the marching variables, private to each datum)
but may **not** touch externally-owned mutable state except through an
explicitly hazard-managed primitive. Keeping pipelines pure dataflow avoids
reintroducing CPU-style RAW/WAW/WAR hazards and forwarding.

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
  - **memory** — port-limited (N reads / M writes per cycle), lowers to BRAM.
    **Port count is part of the memory type.** A read carries a latency
    contract (see OPEN question below).
- **Fixed-capacity channels** — capacity is part of the type, enabling static
  reasoning about backpressure (cf. SpinalHDL `Stream`/`Flow`, Bluespec FIFOs).
  Decide explicitly whether channels *guarantee* anything about deadlock or
  merely expose ready/valid (the former is genuinely hard; v1 likely the latter).
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

### 6.9 OPEN QUESTION (memory model)

Treat a ported memory as one of the generator-style stateful instances **with a
read-latency contract**, or as a **distinct primitive type**? (Recommendation:
distinct primitive type whose interface *is* a latency-contracted stateful
instance — so it composes under the §3.4 latency rules — but whose lowering is
hard-wired to BRAM/`DP16KD`/`PDPW16KD`-style primitives rather than synthesized
from generic state.)

### 6.10 Integer literals (SETTLED)

Integer literals are **untyped by default**; their width is resolved from
context (the surrounding type). A literal may carry an optional **Rust-style
width suffix** — `0b0111111_u7`, `42_i16` — which asserts its type and makes a
width mismatch with context a compile error (useful for bit-pattern constants,
where an off-by-one-bit typo is otherwise easy to miss). Supported forms:
decimal, `0x` hex, and `0b` binary, with `_` permitted as a digit separator.

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
| **F5** | pin map (`.pins`) → top-level Verilog wrapper + nextpnr PCF. | `build` emits a synthesizable top + PCF |

**Locked decisions (this milestone):**
- The oracle observes **all leaf state registers** (full-state trace), plus the
  `Outputs` value for a `(Self, Outputs)` transition function.
- The clock-root `tick` may return `-> Self` (outputs are observed state fields)
  **or** `-> (Self, Outputs)` (a distinct output channel; cf. §4.4).
- Struct reset/default values const-eval over **literals and `const` references**
  (const-fn-in-default deferred).
- **Single clock domain only** (phantom-domain CDC deferred); memories and the
  `pipeline`/`multiplex` colors are out of scope (M4/§3, §6.6).

---

## 10. Settled Decisions vs Open Questions (quick index)

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
- Clock domain as phantom type; CDC crossing without a primitive is a type error.
- Bare `/` forbidden; division via handshaked multi-cycle primitive.
- `pipeline`/`multiplex` as two function colors; selector binding time sets
  latency class; three latency points (`Static`/`Bounded`/`Dynamic`).
- Backends ordered sim → Verilog → (later) nextpnr; native synth is separate.
- Physical pin mapping = PCF-superset binding pins to dotted state/input paths +
  bit/array index + optional `low` polarity; the compiler synthesizes the
  top-level Verilog wrapper and emits the concrete PCF (§7.6).
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
5. §6.6 — do channels guarantee deadlock-freedom or just expose ready/valid (v1).
