# Grammar corpus

Spec-by-example tests for the Sinter surface grammar. Each `.snt` file isolates
one feature of the language; together they document what `sinter-syntax` accepts
and rejects, and they regression-guard the [`grammar/sinter.pest`](../../src/grammar/sinter.pest)
PEG against the AST it produces. The harness lives in [`../corpus.rs`](../corpus.rs).

## Layout

- **`pass/`** — programs that must parse. Each `foo.snt` has a sibling golden
  `foo.ast` holding the pretty-printed AST it parses to. The example therefore
  shows both the source *and* exactly how the parser interprets it; any drift
  fails the test.
- **`fail/`** — programs that must be rejected. Each may carry a single
  `//~ ERROR <substring>` annotation in a comment; the harness asserts that
  parsing fails *and* the error message contains that substring, so a case
  fails for the reason it claims to, not by accident.

## Adding a case

Drop a `.snt` file into `pass/` or `fail/` — the harness discovers it by
globbing the directory, no Rust changes required. For a `pass/` case, generate
its golden AST (see below). For a `fail/` case, add a `//~ ERROR` line naming a
stable fragment of the expected message (`syntax error`, `invalid type`, ...).

## Regenerating golden ASTs

After an intentional grammar or AST change, rewrite the `pass/` goldens and
review the diffs:

```sh
SINTER_BLESS=1 cargo test -p sinter-syntax --test corpus
git diff crates/sinter-syntax/tests/corpus
```

Treat an unexpected `.ast` diff the same as a failing test — it means the parse
result changed.
