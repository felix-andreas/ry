"""Turns a typed benchmark program into a package and a script that uses it.

The package's `R/program.R` holds the typed harness and the program's top-level definitions (its
functions, aliases and types), and `use.R` holds every other top-level expression, after a
`library()` call that attaches the package. Installing the package and running `use.R` times the
program's functions as an installed package runs them, which is how the `ry_typed_file` engine
measures the typed code file.

    python3 tools/bench/typed_package.py PROGRAM.typed.R OUT_DIR [--name NAME]
"""

import argparse
import pathlib
import re

HARNESS = pathlib.Path(__file__).with_name("harness.typed.R")
DEFINITION = re.compile(r"^(?:[A-Za-z.][\w.]*\s*(?:<-|=)\s*function\b|(?:alias|type)\s+[A-Z])")
CONTINUES = re.compile(r"(?:<-|=|\+|-|\*|/|,|\|>|%[^%]*%|&&|\|\||&|\||\(|\{|\[|~|else)\s*$")
RETURN_TYPE = re.compile(r"\s*(?:->\s*[A-Za-z_][\w.]*(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>|\{[^{}]*\})?)?\s*$")


def chunks(text):
    """The program's top-level expressions, each with the comment lines just above it."""
    result, current, codes, comments, depth = [], [], [], [], 0
    for line in text.splitlines():
        code = strip_comment(line)
        if not current and (not code.strip()):
            if line.strip():
                comments.append(line)
            else:
                comments.clear()
            continue
        current.append(line)
        codes.append(code)
        depth += bracket_balance(code)
        if depth <= 0 and not CONTINUES.search(code.rstrip()) and not awaits_body("\n".join(codes)):
            result.append((comments, current))
            current, codes, comments, depth = [], [], [], 0
    if current:
        result.append((comments, current))
    return result


def awaits_body(code):
    """Whether the code ends in a function header, `function(x)` or `function(x) -> T`, whose body
    starts on a later line."""
    for match in re.finditer(r"\bfunction\s*\(", code):
        close = match.end() - 1 + closing_paren(code[match.end() - 1:])
        if close >= match.end() and RETURN_TYPE.fullmatch(code[close + 1:]):
            return True
    return False


def closing_paren(code):
    depth = 0
    for index, char in enumerate(code):
        depth += {"(": 1, ")": -1}.get(char, 0)
        if depth == 0:
            return index
    return -1


def strip_comment(line):
    quote = None
    for index, char in enumerate(line):
        if quote:
            if char == "\\":
                continue
            if char == quote:
                quote = None
        elif char in "\"'`":
            quote = char
        elif char == "#":
            return line[:index]
    return line


def bracket_balance(code):
    balance, quote, escaped = 0, None, False
    for char in code:
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
        elif char in "\"'`":
            quote = char
        elif char in "([{":
            balance += 1
        elif char in ")]}":
            balance -= 1
    return balance


def split(program):
    definitions, rest = [], []
    for comments, lines in chunks(program.read_text()):
        target = definitions if DEFINITION.match(lines[0]) else rest
        target.extend(comments + lines)
    return definitions, rest


def write(program, out, name):
    definitions, rest = split(program)
    package = out / name
    (package / "R").mkdir(parents=True, exist_ok=True)
    (package / "DESCRIPTION").write_text(
        f"Package: {name}\nVersion: 1.0.0\nTitle: {name}\nDescription: The definitions of {program.name}.\n"
        "License: MIT\n")
    (package / "NAMESPACE").write_text('exportPattern("^[^.]")\n')
    (package / "R" / "program.R").write_text(HARNESS.read_text() + "\n".join(definitions) + "\n")
    (package / "ry.toml").write_text("[check]\ntyping = true\nstrict = true\n")
    (out / "use.R").write_text(f'library({name}, lib.loc = normalizePath("library"))\n' + "\n".join(rest) + "\n")
    return package


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("program", type=pathlib.Path)
    parser.add_argument("out", type=pathlib.Path)
    parser.add_argument("--name")
    args = parser.parse_args()
    name = args.name or re.sub(r"[^A-Za-z0-9.]", ".", args.program.name.removesuffix(".typed.R"))
    write(args.program, args.out, name)


if __name__ == "__main__":
    main()
