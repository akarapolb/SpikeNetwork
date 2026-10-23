#!/usr/bin/env python3
"""Watch openmls security advisories (ADR 0025, slice 4).

cargo-deny reads the RustSec database, which has no entry for openmls: its advisories are published only
as GitHub security advisories. So this script asks GitHub for them and fails if

  * the exact version `spike-mls` pins is affected by any advisory, or
  * an advisory appears that is not listed in mls/advisories-reviewed.txt (a person has to read it and
    add it, with a note, before this goes green again).

Usage:  python scripts/openmls_advisories.py [--offline-json FILE]
Set GITHUB_TOKEN to avoid the unauthenticated rate limit. Exit codes: 0 fine, 1 problem found,
2 could not ask GitHub (CI prints a warning for that and does not fail the build on a network blip).
"""
import json
import os
import re
import sys
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
URL = "https://api.github.com/repos/openmls/openmls/security-advisories?per_page=100"


def pinned_version(cargo_toml: str) -> str:
    m = re.search(r'^openmls\s*=\s*\{[^}]*version\s*=\s*"=([0-9]+\.[0-9]+\.[0-9]+)"', cargo_toml, re.M)
    if not m:
        raise SystemExit("mls/Cargo.toml: openmls is not pinned with an exact version (=x.y.z)")
    return m.group(1)


def parse_version(v: str):
    return tuple(int(x) for x in v.split("."))


def affected(version: str, vulnerable_range: str) -> bool:
    """True if `version` satisfies a range like '>= 0.5.0, < 0.8.3' (every clause must hold)."""
    if not vulnerable_range:
        return True  # no range given: assume the worst
    v = parse_version(version)
    for clause in vulnerable_range.split(","):
        m = re.fullmatch(r"\s*(>=|<=|>|<|=)?\s*([0-9]+\.[0-9]+\.[0-9]+)\s*", clause)
        if not m:
            return True  # a range we cannot read: assume the worst
        op, bound = m.group(1) or "=", parse_version(m.group(2))
        ok = {">=": v >= bound, "<=": v <= bound, ">": v > bound, "<": v < bound, "=": v == bound}[op]
        if not ok:
            return False
    return True


def reviewed_ids(text: str):
    return {line.split()[0] for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#")}


def check(version: str, advisories, reviewed):
    problems = []
    for a in advisories:
        ranges = [v.get("vulnerable_version_range") or "" for v in (a.get("vulnerabilities") or [{}])]
        hit = any(affected(version, r) for r in ranges)
        tag = f'{a["ghsa_id"]} ({a.get("severity")}): {a.get("summary")}'
        if hit:
            problems.append(f"AFFECTS the pinned openmls {version}: {tag}")
        if a["ghsa_id"] not in reviewed:
            problems.append(f"NOT REVIEWED yet (read it, then add it to mls/advisories-reviewed.txt): {tag}")
    return problems


def main(argv):
    version = pinned_version(open(os.path.join(ROOT, "mls", "Cargo.toml"), encoding="utf8").read())
    reviewed = reviewed_ids(open(os.path.join(ROOT, "mls", "advisories-reviewed.txt"), encoding="utf8").read())
    if "--offline-json" in argv:
        advisories = json.load(open(argv[argv.index("--offline-json") + 1], encoding="utf8"))
    else:
        req = urllib.request.Request(URL, headers={"Accept": "application/vnd.github+json", "User-Agent": "spike-advisory-watch"})
        if os.environ.get("GITHUB_TOKEN"):
            req.add_header("Authorization", "Bearer " + os.environ["GITHUB_TOKEN"])
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                advisories = json.load(r)
        except Exception as e:  # network, rate limit, API change
            print(f"could not read the openmls advisories from GitHub: {e}")
            return 2
    problems = check(version, advisories, reviewed)
    print(f"openmls pinned at {version}; {len(advisories)} published advisories; {len(reviewed)} reviewed")
    for p in problems:
        print("  " + p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
