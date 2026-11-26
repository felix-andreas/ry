# typed

Read this before you change the typed core. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **A checked `List.from_r` (or `List.from_r_or`, `List.from_r_missing`) gives the checker's type in every tier, by widening or by an error.** The preflight passes the checker's item type to the runtime (`.ry_items`), which lets typed code compile before any call ran. The checker's R types are coarser than run-time storage (`abs(-3:3)` is typed `double` and holds integers), so `arrays::checked_conversion` (and `checked_missing` for results) widens `Bool` and `I64` items as R coerces and raises one error for anything else, and the interpreter and compiled code both go through it. Never compare a checker's R type with run-time storage for equality, and give any new checker fact that compiled code relies on one shared run-time function for both tiers (`.agents/memory/decisions/typed-native.md`, "`ry run --aot` compiles a script's typed code before it starts").
- **Tier switches for typed code:** `RY_JIT=off|on|force|stress`, `--aot`, and `RY_AHEAD_MISS=1`, which drops the closures `--aot` created up front so every sibling guess fails. `typed_scripts_agree_however_they_compile` runs a script every way and compares output.
