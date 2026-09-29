#!/usr/bin/env python3
"""Regenerate tests/differential/expected/ from icloud-md.

For every scenario in scenarios.json (or only those named on the command
line), prepare a vault exactly as tests/cli_differential.rs does, run
icloud-md through run-node.sh on the scenario's cassette, and store what it
did: exit code, stdout (the out-dir path replaced by @OUT@), the request log,
the resulting vault, and mtimes.json (the deterministic mtimes of the
vault's files - ones set from note dates or by the setup, not wall-clock
writes).

    tests/differential/regen.py [scenario ...]

Needs the icloud-md clone with node_modules (see README.md, ICLOUD_MD).
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
EXPECTED = os.path.join(HERE, "expected")
STATE_DIR = ".icloud-md"


def apply_edit(vault, edit):
    path = os.path.join(vault, edit["file"])
    if edit.get("delete"):
        os.remove(path)
    elif "write" in edit:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(edit["write"])
    elif "append" in edit:
        with open(path, "a", encoding="utf-8") as f:
            f.write(edit["append"])
    elif "replace" in edit:
        old, new = edit["replace"]
        with open(path, encoding="utf-8") as f:
            text = f.read()
        if old not in text:
            raise SystemExit(f"{edit['file']}: {old!r} not found")
        with open(path, "w", encoding="utf-8") as f:
            f.write(text.replace(old, new, 1))
    elif "json" in edit:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
        for key, value in edit["json"].items():
            if value is None:
                data.pop(key, None)
            else:
                data[key] = value
        with open(path, "w", encoding="utf-8") as f:
            f.write(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    else:
        raise SystemExit(f"unknown edit {edit}")


def set_mtimes(vault, ms):
    for root, dirs, files in os.walk(vault):
        dirs[:] = [d for d in dirs if not (root == vault and d == STATE_DIR)]
        for name in files:
            os.utime(os.path.join(root, name), ns=(ms * 1_000_000, ms * 1_000_000))


def prepare(scenario, defaults, out):
    vault = os.path.join(out, "vault")
    if "vaultFrom" in scenario:
        shutil.copytree(os.path.join(EXPECTED, scenario["vaultFrom"], "vault"), vault)
    for edit in scenario.get("edits", []):
        apply_edit(vault, edit)
    if os.path.isdir(vault):
        set_mtimes(vault, scenario.get("setupMtimeMs", defaults["setupMtimeMs"]))
    return vault


def subst(text, out, vault):
    return text.replace("@VAULT@", vault).replace("@OUT@", out)


def deterministic_mtimes(vault, started):
    mtimes = {}
    for root, dirs, files in os.walk(vault):
        dirs[:] = [d for d in dirs if not (root == vault and d == STATE_DIR)]
        for name in files:
            path = os.path.join(root, name)
            ms = os.stat(path).st_mtime_ns // 1_000_000
            if ms < started - 5000:
                mtimes[os.path.relpath(path, vault)] = ms
    return dict(sorted(mtimes.items()))


def run(scenario, defaults):
    name = scenario["name"]
    out = tempfile.mkdtemp(prefix=f"icloud-md-{name}-")
    vault = prepare(scenario, defaults, out)
    cwd = subst(scenario.get("cwd", "@OUT@"), out, vault)
    args = [subst(a, out, vault) for a in scenario["args"]]
    started = int(time.time() * 1000)
    now = str(scenario.get("now", defaults["now"]))
    cmd = [os.path.join(HERE, "run-node.sh"), os.path.join(HERE, "cassettes", scenario["cassette"]), out,
           "--now", now, "--cwd", cwd, "--", *args]
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)

    dest = os.path.join(EXPECTED, name)
    shutil.rmtree(dest, ignore_errors=True)
    os.makedirs(dest)
    shutil.copy(os.path.join(out, "exit"), os.path.join(dest, "exit"))
    with open(os.path.join(out, "stdout"), encoding="utf-8") as f:
        stdout = f.read().replace(out, "@OUT@")
    with open(os.path.join(dest, "stdout.json"), "w", encoding="utf-8") as f:
        f.write(stdout)
    if os.path.exists(os.path.join(out, "requests.json")):
        shutil.copy(os.path.join(out, "requests.json"), os.path.join(dest, "requests.json"))
    if os.path.isdir(vault):
        shutil.copytree(vault, os.path.join(dest, "vault"))
        with open(os.path.join(dest, "mtimes.json"), "w", encoding="utf-8") as f:
            f.write(json.dumps(deterministic_mtimes(vault, started), indent=2) + "\n")
    with open(os.path.join(out, "exit"), encoding="utf-8") as f:
        print(f"{name}: exit {f.read().strip()}")
    shutil.rmtree(out)


def main():
    with open(os.path.join(HERE, "scenarios.json"), encoding="utf-8") as f:
        manifest = json.load(f)
    wanted = set(sys.argv[1:])
    for scenario in manifest["scenarios"]:
        if not wanted or scenario["name"] in wanted:
            run(scenario, manifest["defaults"])


if __name__ == "__main__":
    main()
