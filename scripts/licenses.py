#!/usr/bin/env python3
"""Checks LICENSES.json against the tree and inventories the Rust dependencies' licences.

  scripts/licenses.py check [--no-deps]    exit 1 when the manifest and the tree disagree
  scripts/licenses.py inventory [--json]   the licence expression of every external crate, per workspace

`check` needs `cargo` (run it inside `nix-shell`) unless --no-deps is given; it reads `cargo metadata --offline
--locked`, so every crate must be in the local registry. See LICENSES.md for the policy.
"""

import argparse
import collections
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "LICENSES.json"
TEXTS = ROOT / "LICENSES"

# Cargo workspaces and the licences each may depend on beyond PERMISSIVE.
WORKSPACES = {
    ".": set(),
    "crates/hook": set(),
    "crates/reports-server": {"AGPL-3.0-or-later"},
}

# MPL-2.0 is file-level copyleft: fine for crates used unmodified, with their notices kept.
PERMISSIVE = {
    "MIT", "MIT-0", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unicode-3.0", "Unlicense", "CC0-1.0",
    "0BSD", "BSL-1.0", "NCSA", "MPL-2.0", "CDLA-Permissive-2.0", "GPL-3.0", "GPL-3.0-or-later",
}
EXCEPTIONS = {"LLVM-exception"}

# Tracked files at the repository root that are tooling or notices, not components.
ROOT_FILES = {
    ".gitignore", ".githooks", "CLAUDE.md", "Cargo.lock", "Cargo.toml", "LICENSE", "LICENSES", "LICENSES.json",
    "LICENSES.md", "clippy.toml", "rustfmt.toml", "keys", "README.md", "CHANGELOG.md", "shell.nix", "index.html",
    "tag-changelog", ".github", "docs", "scripts", "tools",
}

TOKEN = re.compile(r"\(|\)|[A-Za-z0-9.+-]+")


def parse_expression(text):
    """An SPDX expression as nested tuples: ("and"|"or", [..]), ("with", id, exception) or an id string."""
    text = re.sub(r"\s*/\s*", " OR ", text)
    tokens = TOKEN.findall(text)
    position = 0

    def peek():
        return tokens[position] if position < len(tokens) else None

    def take():
        nonlocal position
        position += 1
        return tokens[position - 1]

    def atom():
        token = take()
        if token == "(":
            node = disjunction()
            if take() != ")":
                raise ValueError("unbalanced parenthesis")
            return node
        if peek() and peek().upper() == "WITH":
            take()
            return ("with", token, take())
        return token

    def conjunction():
        items = [atom()]
        while peek() and peek().upper() == "AND":
            take()
            items.append(atom())
        return items[0] if len(items) == 1 else ("and", items)

    def disjunction():
        items = [conjunction()]
        while peek() and peek().upper() == "OR":
            take()
            items.append(conjunction())
        return items[0] if len(items) == 1 else ("or", items)

    node = disjunction()
    if position != len(tokens):
        raise ValueError(f"trailing text in {text!r}")
    return node


def identifiers(node):
    if isinstance(node, str):
        return {node}
    if node[0] == "with":
        return {node[1]}
    return set().union(*(identifiers(child) for child in node[1]))


def allowed(node, extra):
    if isinstance(node, str):
        return node in PERMISSIVE or node in extra
    if node[0] == "with":
        return node[2] in EXCEPTIONS and allowed(node[1], extra)
    if node[0] == "and":
        return all(allowed(child, extra) for child in node[1])
    return any(allowed(child, extra) for child in node[1])


def cargo_packages(workspace):
    result = subprocess.run(
        ["cargo", "metadata", "--offline", "--locked", "--format-version", "1"],
        cwd=ROOT / workspace,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cargo metadata failed in {workspace}: {result.stderr.strip()[-300:]}")
    return [package for package in json.loads(result.stdout)["packages"] if package["source"]]


def tracked_roots():
    out = subprocess.run(["git", "ls-files"], cwd=ROOT, capture_output=True, text=True, check=True).stdout
    return sorted({line.split("/", 1)[0] for line in out.splitlines() if line})


def check_manifest(problems):
    try:
        manifest = json.loads(MANIFEST.read_text())
    except (OSError, ValueError) as error:
        problems.append(f"LICENSES.json: {error}")
        return None
    if manifest.get("schema_version") != 1:
        problems.append("LICENSES.json: schema_version must be 1")
    references = manifest.get("license_references", {})
    components = manifest.get("components", [])
    covered = []
    for component in components:
        name = component.get("name", "<unnamed>")
        for key in ("name", "paths", "license", "review", "provenance"):
            if key not in component:
                problems.append(f"{name}: missing {key}")
        for path in component.get("paths", []):
            covered.append(path)
            if not (ROOT / path).exists():
                problems.append(f"{name}: path {path} does not exist")
        expression = component.get("license", "")
        if expression.startswith("LicenseRef-"):
            if expression not in references:
                problems.append(f"{name}: {expression} is not defined in license_references")
            continue
        try:
            ids = identifiers(parse_expression(expression))
        except ValueError as error:
            problems.append(f"{name}: licence {expression!r}: {error}")
            continue
        for identifier in sorted(ids):
            if not (TEXTS / f"{identifier}.txt").is_file():
                problems.append(f"{name}: LICENSES/{identifier}.txt is missing")
    for root in tracked_roots():
        if root in ROOT_FILES:
            continue
        if not any(path == root or path.startswith(root + "/") or root.startswith(path.rstrip("/") + "/") for path in covered):
            problems.append(f"{root} is tracked but no component covers it")
    return manifest


def check_crates(manifest, problems):
    workspace_license = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["license"]
    by_path = {path: component["license"] for component in manifest["components"] for path in component["paths"]}
    members = ["app", *(f"crates/{name}" for name in ("core", "gog", "ps", "runtime", "report", "hook", "reports-server"))]
    for member in members:
        data = tomllib.loads((ROOT / member / "Cargo.toml").read_text())["package"]
        license_value = data.get("license")
        if license_value is None or isinstance(license_value, dict):
            license_value = workspace_license
        expected = by_path.get(member)
        if expected and expected != license_value:
            problems.append(f"{member}: Cargo.toml says {license_value} but LICENSES.json says {expected}")


def check_dependencies(problems):
    for workspace, extra in WORKSPACES.items():
        for package in cargo_packages(workspace):
            expression = package.get("license")
            label = f"{workspace}: {package['name']} {package['version']}"
            if not expression:
                problems.append(f"{label} declares no licence")
                continue
            try:
                node = parse_expression(expression)
            except ValueError as error:
                problems.append(f"{label}: {error}")
                continue
            if not allowed(node, extra):
                problems.append(f"{label}: {expression!r} has no alternative on the allow-list")


def command_check(arguments):
    problems = []
    manifest = check_manifest(problems)
    if manifest:
        check_crates(manifest, problems)
    if not arguments.no_deps:
        try:
            check_dependencies(problems)
        except (RuntimeError, OSError) as error:
            problems.append(str(error))
    for problem in problems:
        print(problem)
    print("licences: ok" if not problems else f"licences: {len(problems)} problem(s)")
    return 1 if problems else 0


def command_inventory(arguments):
    report = {}
    for workspace in WORKSPACES:
        counts = collections.Counter()
        packages = collections.defaultdict(list)
        for package in cargo_packages(workspace):
            expression = package.get("license") or "(none)"
            counts[expression] += 1
            packages[expression].append(f"{package['name']} {package['version']}")
        report[workspace] = {"expressions": dict(counts.most_common()), "packages": packages}
    if arguments.json:
        print(json.dumps(report, indent=2, sort_keys=True))
        return 0
    for workspace, data in report.items():
        print(f"== {workspace}: {sum(data['expressions'].values())} external crates")
        for expression, count in data["expressions"].items():
            names = data["packages"][expression]
            print(f"  {count:4}  {expression}" + (f"   {', '.join(names)}" if count <= 3 else ""))
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check")
    check.add_argument("--no-deps", action="store_true", help="skip the cargo dependency scan")
    check.set_defaults(run=command_check)
    inventory = commands.add_parser("inventory")
    inventory.add_argument("--json", action="store_true")
    inventory.set_defaults(run=command_inventory)
    arguments = parser.parse_args()
    return arguments.run(arguments)


if __name__ == "__main__":
    sys.exit(main())
