# Grammar

This is the compiler's grammar, written for the [pest](https://pest.rs)
parser generator. It is the normative definition of Sinter's syntax. Rules
marked `_` are silent, and `@` rules are atomic (no whitespace inside).

```text
{{#include ../../../../crates/sinter-syntax/src/grammar/sinter.pest}}
```
