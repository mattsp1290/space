#!/usr/bin/env python3
"""Inventory Cargo metadata. This is evidence for review, not legal clearance."""
import json
import pathlib
import sys


def inventory(metadata):
    members = set(metadata["workspace_members"])
    return {
        "status": "unreviewed; not a complete license-notice bundle",
        "scope": "Cargo metadata packages, including build/dev/transitive dependencies",
        "packages": [
            {
                "name": p["name"],
                "version": p["version"],
                "source": p.get("source"),
                "license": p.get("license"),
                "license_file": p.get("license_file"),
                "repository": p.get("repository"),
                "workspace_member": p["id"] in members,
            }
            for p in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"]))
        ],
    }


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("Usage: dependency-inventory.py METADATA_JSON OUTPUT_JSON")
    data = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8-sig"))
    pathlib.Path(sys.argv[2]).write_text(json.dumps(inventory(data), indent=2) + "\n")
