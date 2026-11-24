#!/usr/bin/env python3
"""Mirror Oracle's published Oracle Linux box metadata into data/oraclelinux/.

Oracle publishes per-box documents at oracle.github.io/vagrant-projects. Those
carry fields this registry's model does not have (status, release dates,
rendered descriptions, sizes, kernel) and lack the architecture fields it does
have, so this narrows them to the catalog schema and derives the architecture
from the box name.

Usage: scripts/oracle_boxes.py [--check]

  --check  write nothing, exit 1 if the tree differs from what Oracle serves
"""

import json
import sys
import urllib.request
from pathlib import Path

BASE = "https://oracle.github.io/vagrant-projects/boxes/oraclelinux"
BOXES = [
    "7",
    "7-btrfs",
    "8",
    "8-aarch64",
    "8-btrfs",
    "9",
    "9-aarch64",
    "9-btrfs",
    "10",
    "10-aarch64",
    "10-btrfs",
]
OUT = Path(__file__).resolve().parent.parent / "data/oraclelinux"
KEEP = ("name", "url", "checksum", "checksum_type")


def fetch(box):
    with urllib.request.urlopen(f"{BASE}/{box}.json", timeout=60) as response:
        return json.load(response)


def convert(box, remote):
    name = f"oraclelinux/{box}"
    if remote["name"] != name:
        sys.exit(f"{box}: Oracle names it {remote['name']!r}, expected {name!r}")
    arm = box.endswith("-aarch64")
    architecture = "arm64" if arm else "amd64"
    label = "Apple Silicon" if arm else "x86_64"
    description = remote["description"].replace(" aarch64", "")

    versions = []
    for version in remote["versions"]:
        providers = []
        for provider in version["providers"]:
            entry = {key: provider[key] for key in KEEP}
            entry["architecture"] = architecture
            entry["default_architecture"] = True
            providers.append(entry)
        versions.append({"version": version["version"], "providers": providers})

    document = {
        "name": name,
        "description": f"{description} ({label})",
        "versions": versions,
    }
    return json.dumps(document, indent=2) + "\n"


def main():
    check = "--check" in sys.argv[1:]
    if not OUT.is_dir():
        sys.exit(f"{OUT} is missing")
    changed = 0

    for box in BOXES:
        rendered = convert(box, fetch(box))
        target = OUT / f"{box}.json"
        if target.exists() and target.read_text() == rendered:
            continue
        changed += 1
        print(f"{'behind' if check else 'updated'}: {target.relative_to(OUT.parents[1])}")
        if not check:
            target.write_text(rendered)

    expected = {f"{box}.json" for box in BOXES}
    for stale in sorted(p.name for p in OUT.glob("*.json") if p.name not in expected):
        print(f"stale: data/oraclelinux/{stale}")
        if not check:
            (OUT / stale).unlink()
        changed += 1

    print(f"{changed} file(s) {'would change' if check else 'written'}")
    if check and changed:
        sys.exit(1)


if __name__ == "__main__":
    main()
