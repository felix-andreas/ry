# ry for Zed

Zed extension for [ry](https://github.com/felix-andreas/ry): a type checker and a fast runtime for
R, written in Rust.

Not yet published to Zed's extension registry. Install it manually as a dev extension.

## Install

Zed has no built-in R support, so install the [R extension](https://zed.dev/extensions/r) first.

<!-- prettier-ignore -->
1. Install a [Rust toolchain](https://rustup.rs) — Zed compiles dev extensions to WebAssembly itself.
2. Clone the repository:
   ```bash
   git clone https://github.com/felix-andreas/ry
   ```
3. In Zed, run `zed: install dev extension` from the command palette (`ctrl-shift-p` / `cmd-shift-p`)
   and select the `editors/zed` directory of the clone.

Zed rebuilds the extension whenever you reload it, so pull and run `zed: reload extensions` to update.

## The ry binary

The extension finds the language server in this order, and stops at the first hit:

<!-- prettier-ignore -->
1. The `ry` **path in your LSP settings** (below).
2. **`ry` on your `PATH`**.
3. The **latest GitHub release**, downloaded and cached automatically.

On Linux you need nothing extra. The latest release, 0.4.0-beta, has no macOS or Windows archive,
so on those platforms put `ry` on your `PATH` or set its path in your LSP settings. Point it at your
own build with:

```json
// settings.json
{
  "lsp": {
    "ry": {
      "binary": {
        "path": "/absolute/path/to/ry",
        "arguments": ["server"]
      }
    }
  }
}
```

## Annotation highlighting

Zed highlights R with tree-sitter, which sees a `#:` annotation as an ordinary comment. The distinct
colors come from the server as LSP semantic tokens, which Zed leaves off by default:

```json
// settings.json
{
  "languages": {
    "R": {
      "semantic_tokens": "combined"
    }
  }
}
```

See the [language server documentation](https://ry-lang.org/reference/configuration#editor-settings) for every setting.
