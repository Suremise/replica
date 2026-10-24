"""Audit the licences of the pinned restic exe and write third_party/restic/LICENSES.md and NOTICES.txt.

Build-only, run by hand when the restic pin moves (after scripts/fetch-restic.ps1):

    python -I scripts/audit-restic-licences.py [--cache DIR] [--json FILE]

It reads the Go build info embedded in third_party/restic/bin/restic.exe (its `dep` lines: each
linked module, its version and its h1 hash), downloads each module's zip at exactly that version
from proxy.golang.org, checks the zip against the h1 hash, and reads every licence-like file in
it, vendored ones in subfolders included. It also scans source file headers for licence notices.
Anything it can't classify as permissive or MPL-2.0 is listed under "Needs a decision", and the
script exits 1.

NOTICES.txt is restic's part of THIRD-PARTY-NOTICES.txt (scripts/third-party-notices.ps1): restic's
own licence (from its release source tarball, checked against the release's SHA256SUMS), the Go
standard library's licence and patent grant (at the Go version the exe was built with), and the
text of every licence and NOTICE file found in the modules, each once, with the modules it covers.
"""

import argparse
import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import urllib.request
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RESTIC = os.path.join(ROOT, "third_party", "restic")
PERMISSIVE = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC"}
EXCEPTION = {"MPL-2.0"}
LICENCE_FILE = re.compile(
    r"(^|/)(LICEN[CS]E|COPYING|NOTICE|PATENTS|UNLICENSE|COPYRIGHT)[^/]*$|(^|/)[^/]*\.LICEN[CS]E$", re.I
)
HEADER = re.compile(
    r"Mozilla Public|General Public License|Creative Commons|Eclipse Public|SPDX-License-Identifier:\s*([^\s*]+)",
    re.I,
)


def escape(path):
    """Go module proxy case-escaping: each capital becomes '!' and its lower case."""
    return re.sub(r"[A-Z]", lambda m: "!" + m.group(0).lower(), path)


def dirhash_h1(zf):
    """golang.org/x/mod/sumdb/dirhash.Hash1 of a module zip."""
    names = sorted(n for n in zf.namelist() if not n.endswith("/"))
    lines = "".join(f"{hashlib.sha256(zf.read(n)).hexdigest()}  {n}\n" for n in names)
    return "h1:" + base64.b64encode(hashlib.sha256(lines.encode()).digest()).decode()


def classify(text):
    """The licences a licence file's text contains. MPL-2.0's own text names the GPL family
    (its "Secondary Licenses"), so the GPL family is only reported when MPL isn't found."""
    found = []
    mpl = "Mozilla Public License" in text and "2.0" in text
    if mpl:
        found.append("MPL-2.0")
    if "Apache License" in text and "Version 2.0" in text:
        found.append("Apache-2.0")
    if "Permission is hereby granted, free of charge" in text:
        found.append("MIT")
    if "Redistribution and use in source and binary forms" in text:
        third = re.search(r"Neither the name|names of its\s+contributors|name of the copyright holder", text)
        found.append("BSD-3-Clause" if third else "BSD-2-Clause")
    if re.search(r"Permission to use, copy, modify, and(/or)? distribute this software for any", text):
        found.append("ISC")
    if not mpl:
        for name, pattern in (
            ("LGPL", "Lesser General Public License"),
            ("AGPL", "Affero General Public License"),
            ("GPL", "GNU General Public License"),
        ):
            if pattern in text:
                found.append(name)
                break
    for name, pattern in (
        ("CC", "Creative Commons"),
        ("EPL", "Eclipse Public License"),
        ("Unlicense", "released into the public domain"),
    ):
        if pattern in text:
            found.append(name)
    if "Additional IP Rights Grant (Patents)" in text:
        found.append("Go patent grant")
    return found


def audit(exe, cache):
    data = open(exe, "rb").read()
    if re.search(rb"\n=>\t", data):
        sys.exit("The exe has replace directives; this script does not handle them.")
    if b"\nmod\tgithub.com/restic/restic\t" not in data:
        sys.exit("No restic build info found in the exe.")
    pinned = re.search(r"^version=(\S+)$", open(os.path.join(RESTIC, "VERSION")).read(), re.M).group(1)
    reported = subprocess.run([exe, "version"], capture_output=True, text=True, check=True).stdout
    if not reported.startswith(f"restic {pinned} "):
        sys.exit(f"The exe reports '{reported.strip()}', not the pinned {pinned}: run fetch-restic.ps1 -Force.")
    deps = sorted(set(re.findall(rb"\ndep\t([^\t\n]+)\t([^\t\n]+)\t(h1:[A-Za-z0-9+/=]+)", data)))
    modules = []
    for mod, ver, h1 in deps:
        mod, ver, h1 = mod.decode(), ver.decode(), h1.decode()
        url = f"https://proxy.golang.org/{escape(mod)}/@v/{escape(ver)}.zip"
        path = os.path.join(cache, f"{escape(mod).replace('/', '_')}@{ver}.zip")
        if not os.path.exists(path):
            urllib.request.urlretrieve(url, path)
        zf = zipfile.ZipFile(path)
        if dirhash_h1(zf) != h1:
            sys.exit(f"{mod}@{ver}: the zip does not match the exe's hash {h1}.")
        prefix = f"{mod}@{ver}/"
        files, headers = [], {}
        for name in sorted(zf.namelist()):
            rel = name[len(prefix):]
            if LICENCE_FILE.search(rel):
                raw = zf.read(name)
                text = raw.decode("utf-8", "replace")
                files.append(
                    {
                        "path": rel,
                        "sha256": hashlib.sha256(raw).hexdigest(),
                        "licences": classify(text),
                        "text": text,
                    }
                )
            elif rel.endswith((".go", ".s", ".c", ".h")):
                for m in HEADER.finditer(zf.read(name)[:3000].decode("utf-8", "replace")):
                    headers.setdefault(m.group(1) or m.group(0), []).append(rel)
        modules.append(
            {
                "module": mod,
                "version": ver,
                "h1": h1,
                "zip": url,
                "files": files,
                "headers": {k: len(v) for k, v in headers.items()},
            }
        )
    go = re.search(r" compiled with (go[0-9.]+) ", reported)
    if not go:
        sys.exit(f"Couldn't tell which Go built the exe from '{reported.strip()}'.")
    return pinned, go.group(1), modules


def fetch(url, cache, name):
    path = os.path.join(cache, name)
    if not os.path.exists(path):
        urllib.request.urlretrieve(url, path)
    return open(path, "rb").read()


def restic_licence(version, cache):
    """restic's LICENSE, from its source tarball, checked against the release's SHA256SUMS."""
    base = f"https://github.com/restic/restic/releases/download/v{version}"
    tarball = f"restic-{version}.tar.gz"
    sums = fetch(f"{base}/SHA256SUMS", cache, f"restic-{version}-SHA256SUMS").decode()
    want = re.search(rf"^([0-9a-f]{{64}})\s+\*?{re.escape(tarball)}$", sums, re.M)
    if not want:
        sys.exit(f"The release's SHA256SUMS doesn't list {tarball}.")
    data = fetch(f"{base}/{tarball}", cache, tarball)
    if hashlib.sha256(data).hexdigest() != want.group(1):
        sys.exit(f"{tarball} doesn't match the release's SHA256SUMS.")
    import io
    import tarfile

    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        member = tar.extractfile(f"restic-{version}/LICENSE")
        return member.read().decode("utf-8")


def go_licences(go, cache):
    """The Go standard library's LICENSE and PATENTS at the Go version that built the exe."""
    out = []
    for name in ("LICENSE", "PATENTS"):
        url = f"https://raw.githubusercontent.com/golang/go/{go}/{name}"
        out.append(fetch(url, cache, f"{go}-{name}").decode("utf-8"))
    return out


def render_notices(version, go, modules, restic_text, go_texts):
    """restic's part of THIRD-PARTY-NOTICES: every licence and notice text, each once."""
    rule = "-" * 80
    lines = [
        "PART 2: RESTIC",
        "",
        f"Replica ships the official, unmodified restic {version} exe for Windows amd64 (restic.exe),",
        "which it runs to make and read versioned backups. restic and the Go modules linked into it",
        "are under the licences below. One module is under MPL-2.0, as noted with its source.",
        "",
        f"restic {version}: BSD-2-Clause. Source: https://github.com/restic/restic/releases/download/v{version}/restic-{version}.tar.gz",
        "",
        restic_text.rstrip(),
        "",
        rule,
        f"The Go standard library and runtime ({go}): BSD-3-Clause, with Google's patent grant.",
        f"Source: https://github.com/golang/go/tree/{go}",
        "",
    ]
    for text in go_texts:
        lines += [text.rstrip(), ""]
    for m in modules:
        if "MPL-2.0" in top_licences(m):
            lines += [
                rule,
                f"{m['module']} {m['version']} is under the Mozilla Public License 2.0 (its text is below).",
                f"Its exact source, as linked into restic.exe: {m['zip']} ({m['h1']})",
                "",
            ]
    texts = {}
    for m in modules:
        for f in m["files"]:
            entry = texts.setdefault(f["sha256"], {"text": f["text"], "users": []})
            entry["users"].append(f"{m['module']} {m['version']} ({f['path']})")
    for entry in sorted(texts.values(), key=lambda e: e["users"][0]):
        lines += [rule, "Used by:"]
        lines += [f"  - {u}" for u in entry["users"]]
        lines += ["", entry["text"].rstrip(), ""]
    return "\n".join(lines) + "\n"


def is_notice(path):
    name = path.rsplit("/", 1)[-1].upper()
    return name.startswith(("NOTICE", "PATENTS"))


def top_licences(m):
    """The module's own licence: what its root licence files say."""
    found = {lic for f in m["files"] if "/" not in f["path"] and not is_notice(f["path"]) for lic in f["licences"]}
    return sorted(found)


def problems(m):
    out = []
    if not any(f for f in m["files"] if "/" not in f["path"] and not is_notice(f["path"])):
        out.append("no licence file at its root")
    for f in m["files"]:
        if not f["licences"] and not is_notice(f["path"]) and not f["path"].lower().endswith(".csv"):
            out.append(f"`{f['path']}` not recognised")
        for lic in f["licences"]:
            if lic not in PERMISSIVE | EXCEPTION | {"Go patent grant"}:
                out.append(f"`{f['path']}`: {lic}")
    for key in m["headers"]:
        if key not in PERMISSIVE | EXCEPTION:
            out.append(f"source headers: {key}")
    return out


def render(restic_version, modules):
    mpl = [m for m in modules if "MPL-2.0" in top_licences(m)]
    issues = {m["module"]: problems(m) for m in modules if problems(m)}
    v = restic_version
    lines = [
        "# restic's licences",
        "",
        f"Replica bundles the **unmodified** restic {v} exe for Windows amd64: the one approved",
        "exception to the MIT-or-equally-permissive rule. This file records restic's licence and the licences",
        "of every Go module linked into that exe, at the exact versions linked, for THIRD-PARTY-NOTICES.",
        "",
        "**Generated by `scripts/audit-restic-licences.py`.** Don't edit it by hand: rerun the script when the",
        "pin moves, and review the diff.",
        "",
        f"- **restic {v}:** BSD-2-Clause, © 2014 Alexander Neumann.",
        f"  Licence: <https://github.com/restic/restic/blob/v{v}/LICENSE>.",
        f"  Source: <https://github.com/restic/restic/releases/download/v{v}/restic-{v}.tar.gz>. That is restic's",
        "  own code only: the tarball vendors no dependencies, whose exact sources are the module zips below.",
        "- **The Go standard library and runtime**, in every Go exe: BSD-3-Clause, © The Go Authors, with",
        "  Google's patent grant. <https://go.dev/LICENSE>",
        "",
        "## Result",
        "",
    ]
    if issues:
        lines += ["**Needs a decision:** these modules have licences outside the permissive set and MPL-2.0.", ""]
        lines += [f"- `{k}`: {'; '.join(p)}" for k, p in sorted(issues.items())]
        lines.append("")
    lines += [
        f"{len(modules)} modules are linked. Apart from what's listed above, they are all MIT, Apache-2.0,"
        if issues
        else f"All {len(modules)} linked modules are MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause or ISC, vendored",
        "BSD-2-Clause, BSD-3-Clause or ISC, vendored files included, **except these under MPL-2.0**, the"
        if issues
        else f"files included, **except {len(mpl)} under MPL-2.0**, the approved exception:",
    ]
    if issues:
        lines.append("approved exception:")
    lines.append("")
    for m in mpl:
        lines += [
            f"- **`{m['module']}` {m['version']}:** MPL-2.0, with Exhibit B (\"Incompatible With Secondary",
            "  Licenses\"). MPL-2.0 is file-level copyleft. The unmodified exe may ship so long as",
            "  THIRD-PARTY-NOTICES credits the module and says where its source is. Its exact source is",
            f"  <{m['zip']}> (hash `{m['h1']}`).",
        ]
        for f in m["files"]:
            if "/" in f["path"]:
                lines.append(
                    f"  Inside it, `{f['path']}` is {', '.join(f['licences'])}: code derived from the Go standard library."
                )
    notices = [(m["module"], f["path"]) for m in modules for f in m["files"] if f["path"].upper().startswith("NOTICE")]
    if notices:
        lines += [
            "",
            "THIRD-PARTY-NOTICES must carry these NOTICE files: "
            + ", ".join(f"`{p}` from `{mod}`" for mod, p in notices)
            + ".",
        ]
    lines += [
        "",
        "## How this was checked",
        "",
        "1. The modules and their versions are the `dep` lines of the Go build info embedded in the official exe,",
        f"   from restic's `restic_{v}_windows_amd64.zip` (`VERSION` pins its SHA-256). They are exactly what",
        "   was linked: a module in restic's `go.mod` that isn't listed here isn't in the exe.",
        "2. Each module's zip was downloaded from proxy.golang.org at that version and checked against the `h1:`",
        "   hash in the exe (Go's dirhash), so the files read are the ones the exe was built from.",
        "3. The script read every file in each zip named like LICENSE, COPYING, NOTICE, PATENTS or COPYRIGHT,",
        "   in subfolders too, which catches vendored code under other terms. It also scanned the first",
        "   3,000 bytes of every `.go`, `.s`, `.c` and `.h` file for MPL, GPL, Creative Commons, EPL and",
        "   SPDX notices.",
        "",
        "## Modules",
        "",
        "Each licence file is listed with its path in the module, the licences found in it and the first 12",
        "characters of its SHA-256. The module links go to the exact zip.",
        "",
        "| Module | Version | Licence | Licence files | Source headers |",
        "|---|---|---|---|---|",
    ]
    for m in modules:
        files = "<br>".join(
            f"`{f['path']}` {'+'.join(f['licences']) or '(notice)'} `{f['sha256'][:12]}`" for f in m["files"]
        )
        hdr = ", ".join(f"{k} ({n} files)" for k, n in sorted(m["headers"].items())) or "-"
        lines.append(
            f"| [`{m['module']}`]({m['zip']}) | {m['version']} | {' AND '.join(top_licences(m))} | {files} | {hdr} |"
        )
    return "\n".join(lines) + "\n", issues


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--cache", default=os.path.join(tempfile.gettempdir(), "restic-modcache"))
    p.add_argument("--json", help="also write the raw audit to this file")
    args = p.parse_args()
    os.makedirs(args.cache, exist_ok=True)
    restic_version, go, modules = audit(os.path.join(RESTIC, "bin", "restic.exe"), args.cache)
    md, issues = render(restic_version, modules)
    with open(os.path.join(RESTIC, "LICENSES.md"), "w", encoding="utf-8", newline="\n") as f:
        f.write(md)
    notices = render_notices(
        restic_version,
        go,
        modules,
        restic_licence(restic_version, args.cache),
        go_licences(go, args.cache),
    )
    with open(os.path.join(RESTIC, "NOTICES.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write(notices)
    if args.json:
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(modules, f, indent=1)
    print(
        f"restic {restic_version}: {len(modules)} modules, all hashes verified; "
        + (f"{len(issues)} need a decision" if issues else "nothing beyond permissive and MPL-2.0")
    )
    return 1 if issues else 0


if __name__ == "__main__":
    sys.exit(main())
