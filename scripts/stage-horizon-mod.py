#!/usr/bin/env python3
"""Stages the Horizon native mod package: horizon-mod/ plus copies of the terra crates under
crates/, with workspace-inherited manifest keys made explicit, so every path stays inside the
package as the Horizon launcher requires. Tests, benches and dev-dependencies are left out.

The output directory is replaced only if this script created it: it must carry the ownership
marker written at staging time. Filesystem roots, the home directory, the repository, its
ancestors and its source trees, destinations overlapping the sources, symbolic-link destinations
and existing directories without the marker are refused before anything is touched. The package
is built in a fresh temporary directory beside the destination and then swapped into place, so a
failed staging leaves the previous package intact. There is no flag that bypasses these checks."""

import os
import re
import shutil
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MARKER = ".terraplusminus-staging"
MARKER_TEXT = "terraplusminus staged Horizon mod package; safe for stage-horizon-mod.py to replace\n"
# Inside the repository, only these directories may hold staged packages.
STAGING_AREAS = ("artifacts", "target")
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


class Refused(Exception):
    pass


def _inside(path, parent):
    return path == parent or parent in path.parents


def validate_destination(argument, root=ROOT, home=None):
    """The resolved destination, or Refused with the reason. Never modifies anything."""
    home = Path(home if home is not None else Path.home()).resolve()
    given = Path(argument)
    if given.is_symlink():
        raise Refused(f"{argument} is a symbolic link")
    output = given.resolve()
    if output.parent == output:
        raise Refused(f"{output} is a filesystem root")
    if output == home or _inside(home, output):
        raise Refused(f"{output} is the home directory or contains it")
    if _inside(root, output):
        raise Refused(f"{output} is the repository or one of its ancestors")
    sources = [root / "horizon-mod", root / "crates", root / "LICENSE"]
    for source in sources:
        if _inside(output, source) or _inside(source, output):
            raise Refused(f"{output} overlaps the staging source {source}")
    if _inside(output, root):
        relative = output.relative_to(root)
        if relative.parts[0] not in STAGING_AREAS or len(relative.parts) < 2:
            raise Refused(f"{output} is inside the repository but not under one of {', '.join(a + '/<name>' for a in STAGING_AREAS)}")
    if output.exists() or output.is_symlink():
        if not output.is_dir():
            raise Refused(f"{output} exists and is not a directory")
        marker = output / MARKER
        if marker.is_symlink() or not marker.is_file() or marker.read_text(encoding="utf-8", errors="replace") != MARKER_TEXT:
            raise Refused(f"{output} exists and was not created by this script (no {MARKER}); remove it yourself or choose another directory")
    return output


def build_package(output):
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    shutil.copytree(ROOT / "horizon-mod", output, ignore=shutil.ignore_patterns(".horizon", "target"), dirs_exist_ok=True)
    for crate in CRATES:
        source = ROOT / "crates" / crate
        destination = output / "crates" / crate
        destination.mkdir(parents=True)
        for part in ("src", "resources"):
            if (source / part).is_dir():
                shutil.copytree(source / part, destination / part)
        manifest = rewrite_manifest((source / "Cargo.toml").read_text(encoding="utf-8"), workspace)
        tomllib.loads(manifest)
        (destination / "Cargo.toml").write_text(manifest, encoding="utf-8")
    for directory in ("assets/terraplusminus", "data/terraplusminus"):
        (output / directory).mkdir(parents=True, exist_ok=True)
    shutil.copy(ROOT / "LICENSE", output / "LICENSE")
    (output / MARKER).write_text(MARKER_TEXT, encoding="utf-8")


def stage(argument):
    output = validate_destination(argument)
    output.parent.mkdir(parents=True, exist_ok=True)
    fresh = Path(tempfile.mkdtemp(prefix=f".{output.name}.staging-", dir=output.parent))
    try:
        build_package(fresh)
    except BaseException:
        shutil.rmtree(fresh)
        raise
    retired = None
    if output.exists():
        # Checked again right before the swap: only an owned package is ever retired.
        validate_destination(argument)
        retired = Path(tempfile.mkdtemp(prefix=f".{output.name}.retired-", dir=output.parent))
        os.rename(output, retired / "package")
    os.rename(fresh, output)
    if retired is not None:
        shutil.rmtree(retired)
    return output


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: stage-horizon-mod.py <output directory>")
    try:
        print(stage(sys.argv[1]))
    except Refused as reason:
        raise SystemExit(f"stage-horizon-mod.py: refusing to stage: {reason}")


if __name__ == "__main__":
    main()
