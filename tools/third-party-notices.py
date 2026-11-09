"""Writes THIRD_PARTY_NOTICES: the notices for everything third-party in the released `ry`
binary and its runtime library (libR).

    python3 -I tools/third-party-notices.py            # rewrite THIRD_PARTY_NOTICES
    python3 -I tools/third-party-notices.py --check    # fail if it is stale

The file has three parts. The code translated from GNU R is read from the provenance headers in
`crates/*/src` (the `// GNU R: <file> (<what>)` lines and the copyright lines under them), so a
new header reaches the notices without editing this script. The notices for code that does not
come through Cargo (font metrics, bundled algorithms) are fixed below. The Rust crates are every
package that `ry-lang` or `libr` reaches through normal dependency edges on any target, from
`cargo metadata`, with the licence files each crate ships.

`--check` also enforces the licence rule in LICENSE: a workspace crate that reaches `interp`
through normal or build dependencies must be GPL-2.0-or-later, and no other crate may be.
"""

import json
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "THIRD_PARTY_NOTICES"
SHIPPED_ROOTS = ("ry-lang", "libr")
R_DERIVED_ROOT = "interp"

# The licence chosen from an `A OR B` expression, best first: the shortest GPL-compatible
# permissive terms win, so a dual MIT/Apache crate is taken under MIT.
PREFERENCE = [
    "MIT",
    "MIT-0",
    "BSL-1.0",
    "Zlib",
    "ISC",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "CC0-1.0",
    "Unlicense",
    "Apache-2.0 WITH LLVM-exception",
    "Apache-2.0",
    "MPL-2.0",
    "LGPL-2.1-or-later",
]

# C or C++ libraries that a crate compiles into the binary, with the licence files that come
# with them inside the crate package.
BUNDLED = {
    "zstd-sys": [("zstd (C library)", "zstd/LICENSE")],
    "lz4-sys": [("LZ4 (C library)", "liblz4/lib/LICENSE")],
    "libmimalloc-sys": [
        ("mimalloc (C library)", "c_src/mimalloc/v2/LICENSE"),
        ("mimalloc (C library)", "c_src/mimalloc/v3/LICENSE"),
    ],
}

# pcre2-sys compiles its bundled PCRE2 only when no system libpcre2-8 is found (always on
# Windows and macOS release builds), and its package carries the notices only as comments.
BUNDLED_COMMENTS = {
    "pcre2-sys": [
        ("PCRE2 (C library, when linked statically)", "upstream/src/pcre2_compile.c"),
        ("sljit (PCRE2's JIT compiler, when linked statically)",
         "upstream/deps/sljit/sljit_src/sljitLir.c"),
    ],
}

FIXED_NOTICES = """\
Adobe font metrics
------------------

ry's graphics devices measure text with metrics extracted and reformatted from Adobe's Core 14
AFM files (Helvetica.afm, Helvetica-Bold.afm and Symbol.afm), as GNU R ships them:

    Copyright (c) 1985, 1987, 1989, 1990, 1997 Adobe Systems Incorporated. All Rights Reserved.
    Helvetica is a trademark of Linotype-Hell AG and/or its subsidiaries.

Adobe's terms for the AFM files:

    This file and the 14 PostScript(R) AFM files it accompanies may be used, copied, and
    distributed for any purpose and without charge, with or without modification, provided that
    all copyright notices are retained; that the AFM files are not distributed without this
    file; that all modifications to this file or any of the AFM files are prominently noted in
    the modified file(s); and that this paragraph is not modified. Adobe Systems has no
    responsibility or obligation to support the use of the AFM files.

Adobe Symbol encoding
---------------------

plotmath maps the Adobe Symbol encoding to Unicode as GNU R does. The mapping comes from the
Unicode Consortium's table "Adobe Symbol Encoding to Unicode":

    Copyright (c) 1991-2011 Unicode, Inc. All Rights reserved.

    This file is provided as-is by Unicode, Inc. (The Unicode Consortium). No claims are made as
    to fitness for any particular purpose. No warranties of any kind are expressed or implied.
    The recipient agrees to determine applicability of information provided. If this file has
    been provided on magnetic media by Unicode, Inc., the sole remedy for any claim will be
    exchange of defective media within 90 days of receipt.

    Unicode, Inc. hereby grants the right to freely use the information supplied in this file in
    the creation of products supporting the Unicode Standard, and to make copies of this file in
    any form for internal or external distribution as long as this notice remains attached.

X-splines (Xfig)
----------------

The X-spline behind xspline() follows GNU R's src/main/xspline.c, which is source code from
Xfig 3.2.4 modified for R:

    FIG : Facility for Interactive Generation of figures
    Copyright (c) 1985-1988 by Supoj Sutanthavibul
    Parts Copyright (c) 1989-2002 by Brian V. Smith
    Parts Copyright (c) 1991 by Paul King
    Parts Copyright (c) 1992 by James Tough
    Parts Copyright (c) 1998 by Georg Stemmer
    Parts Copyright (c) 1995 by C. Blanc and C. Schlick

    Any party obtaining a copy of these files is granted, free of charge, a full and
    unrestricted irrevocable, world-wide, paid up, royalty-free, nonexclusive right and license
    to deal in this software and documentation files (the "Software"), including without
    limitation the rights to use, copy, modify, merge, publish and/or distribute copies of the
    Software, and to permit persons who receive copies from any such party to do so, with the
    only requirement being that this copyright notice remain intact.

L-BFGS-B
--------

optim(method = "L-BFGS-B") follows L-BFGS-B 3.0 by Ciyou Zhu, Richard Byrd, Jorge Nocedal and
Jose Luis Morales, which carries this condition for use:

    This software is freely available, but we expect that all publications describing work
    using this software, or all commercial products using it, quote at least one of the
    references given below. This software is released under the BSD License.

    References
      * R. H. Byrd, P. Lu and J. Nocedal. A Limited Memory Algorithm for Bound Constrained
        Optimization, (1995), SIAM Journal on Scientific and Statistical Computing, 16, 5,
        pp. 1190-1208.
      * C. Zhu, R. H. Byrd and J. Nocedal. L-BFGS-B: Algorithm 778: L-BFGS-B, FORTRAN routines
        for large scale bound constrained optimization (1997), ACM Transactions on Mathematical
        Software, 23, 4, pp. 550 - 560.
      * J.L. Morales and J. Nocedal. L-BFGS-B: Remark on Algorithm 778: L-BFGS-B, FORTRAN
        routines for large scale bound constrained optimization (2011), ACM Transactions on
        Mathematical Software, 38, 1.

Mersenne Twister
----------------

The Mersenne-Twister generator follows GNU R's src/main/RNG.c, whose Mersenne-Twister part is

    Copyright (C) 1997, 1999 Makoto Matsumoto and Takuji Nishimura.

    M. Matsumoto and T. Nishimura, "Mersenne Twister: A 623-Dimensionally Equidistributed
    Uniform Pseudo-Random Number Generator", ACM Transactions on Modeling and Computer
    Simulation, Vol. 8, No. 1, January 1998, pp 3--30.

Reference BLAS and LAPACK
-------------------------

On Linux, ry's runtime carries libRblas and libRlapack, built from the reference BLAS and LAPACK
in GNU R's sources (src/extra/blas and src/modules/lapack), whose LAPACK part is distributed
under these terms:

    Copyright (c) 1992-2023 The University of Tennessee and The University
                            of Tennessee Research Foundation.  All rights
                            reserved.
    Copyright (c) 2000-2023 The University of California Berkeley. All
                            rights reserved.
    Copyright (c) 2006-2023 The University of Colorado Denver.  All rights
                            reserved.

    Redistribution and use in source and binary forms, with or without
    modification, are permitted provided that the following conditions are
    met:

    - Redistributions of source code must retain the above copyright
      notice, this list of conditions and the following disclaimer.

    - Redistributions in binary form must reproduce the above copyright
      notice, this list of conditions and the following disclaimer listed
      in this license in the documentation and/or other materials
      provided with the distribution.

    - Neither the name of the copyright holders nor the names of its
      contributors may be used to endorse or promote products derived from
      this software without specific prior written permission.

    The copyright holders provide no reassurances that the source code
    provided does not infringe any patent, copyright, or any other
    intellectual property rights of third parties.  The copyright holders
    disclaim any liability to any recipient for claims brought against
    recipient by any third party for infringement of that parties
    intellectual property rights.

    THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
    "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
    LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
    A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
    OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
    SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
    LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
    DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
    THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
    (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
    OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

Independent JPEG Group
----------------------

This software is based in part on the work of the Independent JPEG Group (through the
jpeg-encoder crate, whose IJG terms are listed with it below).
"""


def main():
    check = "--check" in sys.argv[1:]
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    problems = check_licence_rule(metadata)
    text = render(metadata)
    if check:
        if OUTPUT.read_text() != text:
            problems.append("THIRD_PARTY_NOTICES is stale; run `just notices`")
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        sys.exit(1 if problems else 0)
    for problem in problems:
        print(f"warning: {problem}", file=sys.stderr)
    OUTPUT.write_text(text)


def check_licence_rule(metadata):
    packages = {package["id"]: package for package in metadata["packages"]}
    members = set(metadata["workspace_members"])
    by_name = {packages[member]["name"]: member for member in members}
    edges = {
        node["id"]: [
            dep["pkg"]
            for dep in node["deps"]
            if dep["pkg"] in members
            and any(kind["kind"] in (None, "build") for kind in dep["dep_kinds"])
        ]
        for node in metadata["resolve"]["nodes"]
        if node["id"] in members
    }
    derived = {by_name[R_DERIVED_ROOT]}
    changed = True
    while changed:
        changed = False
        for member, deps in edges.items():
            if member not in derived and any(dep in derived for dep in deps):
                derived.add(member)
                changed = True
    problems = []
    for member in sorted(members, key=lambda member: packages[member]["name"]):
        package = packages[member]
        licence = package.get("license") or ""
        gpl = "GPL-2.0-or-later" in licence
        if member in derived and not gpl:
            problems.append(
                f"{package['name']} links GNU R-derived code, so its licence must be "
                f"GPL-2.0-or-later, not {licence or 'unset'}"
            )
        if member not in derived and gpl:
            problems.append(
                f"{package['name']} links no GNU R-derived code, so it must keep a "
                f"permissive licence, not {licence}"
            )
    return problems


def render(metadata):
    sections = [
        HEADER,
        heading("1. Code translated from GNU R"),
        r_derived_section(),
        heading("2. Other notices"),
        FIXED_NOTICES,
        heading("3. Rust crates and the C libraries they bundle"),
        crates_section(metadata),
    ]
    return "\n".join(section.rstrip() + "\n" for section in sections)


HEADER = """\
Third-party notices for ry
==========================

The released `ry` program, with ry's runtime library (libR) built into it, contains the
third-party code listed below. ry itself is distributed under the terms in LICENSE. This file
is generated by `just notices` (tools/third-party-notices.py) from Cargo.lock, the crates'
own licence files and the provenance headers in ry's sources; do not edit it by hand.
"""


def heading(title):
    return f"\n{title}\n{'=' * len(title)}\n"


def r_derived_section():
    entries = defaultdict(list)
    paths = sorted(
        [*(ROOT / "crates").glob("*/src/**/*.rs"), *(ROOT / "crates").glob("*/src/**/*.R")]
    )
    for path in paths:
        comment = "#" if path.suffix == ".R" else "//"
        pattern = re.compile(rf"^{comment} GNU R: (\S+) \((.*)\)$")
        lines = path.read_text().splitlines()
        if not lines or not lines[0].startswith(f"{comment} SPDX-License-Identifier"):
            continue
        relative = path.relative_to(ROOT).as_posix()
        for index, line in enumerate(lines):
            if not line.startswith(comment):
                break
            match = pattern.match(line)
            if not match:
                continue
            notices = []
            for follower in lines[index + 1:]:
                if not follower.startswith(f"{comment}     "):
                    break
                notices.append(follower[len(f"{comment}     "):])
            entries[relative].append((match.group(1), match.group(2), notices))
    out = [
        "These files of ry's runtime are modified translations into Rust, or modified copies, of",
        "parts of GNU R's sources, distributed under the GNU General Public License, version 2 or",
        "later, as GNU R is. GNU R is Copyright (C) The R Core Team and the authors named below;",
        "its sources are at https://www.r-project.org.",
        "",
    ]
    for relative, sources in entries.items():
        out.append(relative)
        for source, covers, notices in sources:
            out.append(f"    GNU R {source} ({covers})")
            out.extend(f"        {notice}" for notice in notices)
        out.append("")
    return "\n".join(out)


def crates_section(metadata):
    packages = {package["id"]: package for package in metadata["packages"]}
    members = set(metadata["workspace_members"])
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    roots = [member for member in members if packages[member]["name"] in SHIPPED_ROOTS]
    shipped = set()
    stack = list(roots)
    while stack:
        current = stack.pop()
        if current in shipped:
            continue
        shipped.add(current)
        for dep in nodes[current]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    third_party = sorted(
        (packages[package_id] for package_id in shipped if packages[package_id]["source"]),
        key=lambda package: (package["name"], package["version"]),
    )

    # licence id -> normalized text -> (text, [crate labels])
    texts = defaultdict(dict)
    # licence id -> [(crate label, repository, copyright lines, has own text)]
    members_of = defaultdict(list)
    extras = []
    for package in third_party:
        label = f"{package['name']} {package['version']}"
        directory = Path(package["manifest_path"]).parent
        expression = package.get("license")
        chosen = choose(expression) if expression else ["LicenseRef-file"]
        files = licence_files(directory, package.get("license_file"))
        used = set()
        for licence in chosen:
            copyright_lines = []
            own = False
            for name, content in files:
                if licence not in classify(name, content, chosen):
                    continue
                used.add(name)
                own = True
                copyright_lines.extend(copyright_of(content))
                key = normalize(content)
                entry = texts[licence].setdefault(key, (strip_copyright(content), []))
                entry[1].append(label)
            members_of[licence].append(
                (label, package.get("repository") or "", unique(copyright_lines), own)
            )
        for name, content in files:
            if name in used or not is_extra(name, content, chosen):
                continue
            extras.append((f"{label}: {name}", content))
        for title, relative in BUNDLED.get(package["name"], []):
            path = directory / relative
            if path.is_file():
                extras.append((f"{label}: {title}, {relative}", path.read_text()))
        for title, relative in BUNDLED_COMMENTS.get(package["name"], []):
            path = directory / relative
            if path.is_file():
                extras.append((f"{label}: {title}, {relative}", licence_comment(path.read_text())))

    out = [
        f"{len(third_party)} crates from crates.io are built into ry. Each crate is listed under",
        "the licence it is used under (for a choice such as `MIT OR Apache-2.0`, the first",
        "permissive licence of the choice), with the copyright lines from its licence files.",
        "The licence texts follow each list; a crate that ships no text of its own is covered",
        "by the text shown for its licence.",
        "",
    ]
    for licence in sorted(members_of, key=lambda licence: (-len(members_of[licence]), licence)):
        crates = members_of[licence]
        title = f"{licence} ({plural(len(crates), 'crate')})"
        out.append(title)
        out.append("-" * len(title))
        out.append("")
        for label, repository, copyright_lines, own in crates:
            line = f"- {label}"
            if repository:
                line += f" <{repository}>"
            if not own:
                line += " (no licence text in its package)"
            out.append(line)
            out.extend(f"    {copyright}" for copyright in copyright_lines)
        out.append("")
        variants = sorted(texts.get(licence, {}).values(), key=lambda entry: -len(entry[1]))
        for number, (text, users) in enumerate(variants, start=1):
            if len(variants) > 1:
                out.append(f"Text {number} of {len(variants)} for {licence}, used by "
                           f"{plural(len(users), 'crate')}:")
                out.append("")
            out.extend(f"    {line}".rstrip() for line in text.strip("\n").splitlines())
            out.append("")
    if extras:
        title = "Additional notices"
        out.append(title)
        out.append("-" * len(title))
        out.append("")
        seen = set()
        for label, content in extras:
            key = normalize(content)
            if key in seen:
                out.append(f"{label}: the same text as above.")
                out.append("")
                continue
            seen.add(key)
            out.append(label)
            out.append("")
            out.extend(f"    {line}".rstrip() for line in content.strip("\n").splitlines())
            out.append("")
    return "\n".join(out)


def choose(expression):
    """The licences a crate is used under, from its SPDX expression."""
    tokens = re.findall(r"\(|\)|[^\s()]+", expression.replace("/", " OR "))
    position = 0

    def parse_or():
        nonlocal position
        options = [parse_and()]
        while position < len(tokens) and tokens[position] == "OR":
            position += 1
            options.append(parse_and())
        return min(options, key=lambda option: max(rank(licence) for licence in option))

    def parse_and():
        nonlocal position
        licences = parse_atom()
        while position < len(tokens) and tokens[position] == "AND":
            position += 1
            licences = licences + parse_atom()
        return licences

    def parse_atom():
        nonlocal position
        token = tokens[position]
        position += 1
        if token == "(":
            licences = parse_or()
            position += 1
            return licences
        if position < len(tokens) and tokens[position] == "WITH":
            exception = tokens[position + 1]
            position += 2
            return [f"{token} WITH {exception}"]
        return [token]

    return unique(parse_or())


def rank(licence):
    return PREFERENCE.index(licence) if licence in PREFERENCE else len(PREFERENCE)


def licence_files(directory, licence_file):
    prefixes = ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE", "UNLICENSE")
    files = []
    for path in sorted(directory.iterdir()):
        if path.is_file() and path.name.upper().startswith(prefixes):
            files.append((path.name, path.read_text(errors="replace")))
    if licence_file:
        path = directory / licence_file
        if path.is_file() and all(name != path.name for name, _ in files):
            files.append((path.name, path.read_text(errors="replace")))
    return files


def classify(name, content, chosen):
    """The licence ids a licence file holds the text of."""
    upper = name.upper()
    for suffix, licence in (
        ("MIT", "MIT"),
        ("APACHE", "Apache-2.0"),
        ("ZLIB", "Zlib"),
        ("BSL", "BSL-1.0"),
        ("BOOST", "BSL-1.0"),
        ("UNICODE", "Unicode-3.0"),
        ("ISC", "ISC"),
        ("BSD-3", "BSD-3-Clause"),
        ("BSD", "BSD-3-Clause" if "Neither the name" in content else "BSD-2-Clause"),
        ("CC0", "CC0-1.0"),
        ("MPL", "MPL-2.0"),
        ("IJG", "IJG"),
    ):
        if suffix in upper.split("LICEN", 1)[-1]:
            return matching(licence, chosen)
    if upper.startswith("UNLICENSE"):
        return matching("Unlicense", chosen)
    found = []
    if "Apache License" in content and "Version 2.0" in content:
        found.append("Apache-2.0")
    if "Permission is hereby granted, free of charge" in content:
        found.append("MIT")
    if "Redistribution and use in source and binary forms" in content:
        found.append("BSD-3-Clause" if "Neither the name" in content or "neither the name" in content
                     else "BSD-2-Clause")
    if "Permission to use, copy, modify, and/or distribute" in content or (
        "Permission to use, copy, modify, and distribute" in content and "ISC" in content
    ):
        found.append("ISC")
    if "This software is provided 'as-is'" in content:
        found.append("Zlib")
    if "Boost Software License" in content:
        found.append("BSL-1.0")
    if "UNICODE LICENSE" in content.upper() and "UNICODE" in content:
        found.append("Unicode-3.0")
    if "Mozilla Public License" in content:
        found.append("MPL-2.0")
    if "CC0" in content or "Creative Commons Legal Code" in content:
        found.append("CC0-1.0")
    if "This is free and unencumbered software" in content:
        found.append("Unlicense")
    if "bzip2" in content and "Julian" in content:
        found.append("bzip2-1.0.6")
    result = []
    for licence in found:
        result.extend(matching(licence, chosen))
    return result


def matching(licence, chosen):
    """`licence` as it appears in `chosen`, so an Apache text also covers its LLVM exception."""
    return [option for option in chosen if option == licence or option.startswith(licence + " ")]


def is_extra(name, content, chosen):
    """Whether a licence file that matched no chosen licence still carries a notice to keep."""
    if classify(name, content, PREFERENCE + ["bzip2-1.0.6"]):
        return False
    return len(content) > 400 or "Copyright" in content


# A copyright line; "(c)" alone also opens list items in licence texts, so it counts only
# before a year.
COPYRIGHT = re.compile(r"\s*(Copyright\b|©|\(c\)\s*\d)", re.IGNORECASE)


def copyright_of(content):
    return [
        line.strip()
        for line in content.splitlines()
        if COPYRIGHT.match(line)
        and "[yyyy]" not in line
        and "{yyyy}" not in line
        and "<year>" not in line
    ]


def strip_copyright(content):
    return "\n".join(
        line for line in content.splitlines()
        if not COPYRIGHT.match(line)
        or "[yyyy]" in line or "{yyyy}" in line or "<year>" in line
    )


def normalize(content):
    return " ".join(strip_copyright(content).split())


def licence_comment(source):
    """The first C comment of `source` that holds licence terms."""
    for match in re.finditer(r"/\*(.*?)\*/", source, re.DOTALL):
        if "Redistribution" in match.group(1):
            lines = (line.strip().lstrip("*").strip() for line in match.group(1).splitlines())
            return "\n".join(lines).strip("\n-")
    return ""


def plural(count, noun):
    return f"{count} {noun}" if count == 1 else f"{count} {noun}s"


def unique(items):
    seen = set()
    result = []
    for item in items:
        if item not in seen:
            seen.add(item)
            result.append(item)
    return result


if __name__ == "__main__":
    main()
