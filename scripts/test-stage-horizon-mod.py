#!/usr/bin/env python3
"""Negative and positive tests for stage-horizon-mod.py (review finding F06).

Dangerous destinations (filesystem root, home, the repository and its ancestors, source trees)
are only ever passed to validate_destination, which never modifies anything. Every end-to-end
run of the staging script uses a disposable directory under a fresh temporary directory, with
HOME pointed inside it. A sentinel file must survive every refused destination."""

import importlib.util
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "stage-horizon-mod.py"
spec = importlib.util.spec_from_file_location("stage", SCRIPT)
stage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stage)

failures = []
checks = 0


def check(name, condition, detail=""):
    global checks
    checks += 1
    if not condition:
        failures.append(f"{name}: {detail}")
    print(("PASS " if condition else "FAIL ") + name)


def refused(argument, home):
    try:
        stage.validate_destination(argument, home=home)
    except stage.Refused as reason:
        return str(reason)
    return None


def run_script(argument, home):
    environment = dict(os.environ, HOME=str(home))
    return subprocess.run([sys.executable, str(SCRIPT), str(argument)], capture_output=True, text=True, env=environment, check=False)


with tempfile.TemporaryDirectory(prefix="stage-test-") as scratch:
    scratch = Path(scratch).resolve()
    home = scratch / "home"
    home.mkdir()

    # Validation only: never run end to end.
    for name, argument in [
        ("filesystem root", "/"),
        ("home", home),
        ("ancestor of home", scratch),
        ("repository root", ROOT),
        ("repository parent", ROOT.parent),
        ("horizon-mod source", ROOT / "horizon-mod"),
        ("inside horizon-mod", ROOT / "horizon-mod" / "staged"),
        ("crates source", ROOT / "crates"),
        ("inside a crate", ROOT / "crates" / "terra-codec" / "out"),
        ("repository docs", ROOT / "docs"),
        ("artifacts itself", ROOT / "artifacts"),
        ("repository .git", ROOT / ".git" / "staged"),
    ]:
        reason = refused(argument, home)
        check(f"refuses {name}", reason is not None, f"{argument} was accepted")
    check("accepts artifacts/<name>", refused(ROOT / "artifacts" / "stage-test-never-created", home) is None)

    # End to end with disposable directories: unrelated files survive every refusal.
    unrelated = scratch / "unrelated"
    unrelated.mkdir()
    (unrelated / "DO_NOT_DELETE.txt").write_text("keep")
    result = run_script(unrelated, home)
    check("refuses an existing directory it did not create", result.returncode != 0 and "was not created by this script" in result.stderr, result.stderr)
    check("unrelated file survives", (unrelated / "DO_NOT_DELETE.txt").read_text() == "keep")

    target = scratch / "real-target"
    target.mkdir()
    (target / "DO_NOT_DELETE.txt").write_text("keep")
    link = scratch / "link"
    link.symlink_to(target)
    result = run_script(link, home)
    check("refuses a symbolic-link destination", result.returncode != 0 and "symbolic link" in result.stderr, result.stderr)
    check("symlink target survives", (target / "DO_NOT_DELETE.txt").read_text() == "keep")

    regular = scratch / "file.txt"
    regular.write_text("keep")
    result = run_script(regular, home)
    check("refuses an existing file", result.returncode != 0 and regular.read_text() == "keep", result.stderr)

    forged = scratch / "forged"
    forged.mkdir()
    (forged / stage.MARKER).write_text("something else\n")
    (forged / "DO_NOT_DELETE.txt").write_text("keep")
    result = run_script(forged, home)
    check("refuses a marker with the wrong content", result.returncode != 0 and (forged / "DO_NOT_DELETE.txt").exists(), result.stderr)

    result = run_script(home, home)
    check("refuses home end to end", result.returncode != 0 and home.is_dir(), result.stderr)

    # Normal staging, then repeat staging over its own output.
    package = scratch / "out" / "terraplusminus"
    first = run_script(package, home)
    check("stages into a new directory", first.returncode == 0 and (package / "Cargo.toml").is_file() and (package / stage.MARKER).is_file(), first.stderr)
    check("stages every crate", all((package / "crates" / crate / "Cargo.toml").is_file() for crate in stage.CRATES))
    (package / "stale.txt").write_text("from the previous staging")
    second = run_script(package, home)
    check("restages over its own output", second.returncode == 0 and (package / "Cargo.toml").is_file(), second.stderr)
    check("restaging replaces the old package", not (package / "stale.txt").exists())
    leftovers = [entry.name for entry in package.parent.iterdir() if entry.name != package.name]
    check("leaves no temporary directories", not leftovers, str(leftovers))

print(f"{checks - len(failures)} of {checks} staging checks passed")
if failures or checks == 0:
    for failure in failures:
        print(f"FAILED: {failure}", file=sys.stderr)
    sys.exit(1)
