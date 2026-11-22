#!/usr/bin/env python3
"""Stages the Horizon native mod package: horizon-mod/ plus copies of the terra crates under
crates/, with workspace-inherited manifest keys made explicit, so every path stays inside the
package as the Horizon launcher requires. Tests, benches and dev-dependencies are left out."""

import re
import shutil
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES = ["terra-codec", "terra-projection", "terra-elevation", "terra-osm", "terra-acquisition", "terra-chunk-generation", "terra-server"]


def explicit_lints(workspace):
    lines = []
    for tool in ("rust", "clippy"):
        lines.append(f"[lints.{tool}]")
        for name, level in workspace["workspace"]["lints"][tool].items():
            if isinstance(level, dict):
                lines.append(f'{name} = {{ level = "{level["level"]}", priority = {level["priority"]} }}')
            else:
                lines.append(f'{name} = "{level}"')
        lines.append("")
    return "\n".join(lines)


def rewrite_manifest(text, workspace):
    package = workspace["workspace"]["package"]

    def inherited(match):
        key = match.group(1)
        value = package[key]
        rendered = "true" if value is True else "false" if value is False else f'"{value}"'
        return f"{key} = {rendered}"

    text = re.sub(r"^([a-z-]+)\.workspace = true$", inherited, text, flags=re.MULTILINE)
    text = re.sub(r"^\[lints\]\nworkspace = true\n", explicit_lints(workspace), text, flags=re.MULTILINE)
    text = re.sub(r"^\[dev-dependencies\]\n(?:[^\[].*\n|\n)*", "", text, flags=re.MULTILINE)
    if "workspace = true" in text:
        raise SystemExit("unhandled workspace inheritance left in a staged manifest")
    return text


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: stage-horizon-mod.py <output directory>")
    output = Path(sys.argv[1]).resolve()
    if output.exists():
        shutil.rmtree(output)
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())
    shutil.copytree(ROOT / "horizon-mod", output, ignore=shutil.ignore_patterns(".horizon", "target"))
    for crate in CRATES:
        source = ROOT / "crates" / crate
        destination = output / "crates" / crate
        destination.mkdir(parents=True)
        for part in ("src", "resources"):
            if (source / part).is_dir():
                shutil.copytree(source / part, destination / part)
        manifest = rewrite_manifest((source / "Cargo.toml").read_text(), workspace)
        tomllib.loads(manifest)
        (destination / "Cargo.toml").write_text(manifest)
    for directory in ("assets/terraplusminus", "data/terraplusminus"):
        (output / directory).mkdir(parents=True, exist_ok=True)
    shutil.copy(ROOT / "LICENSE", output / "LICENSE")
    print(output)


main()
