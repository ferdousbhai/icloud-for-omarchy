#!/usr/bin/env python3
"""Containment guard and helpers for tests/live/push_itest.sh.

Subcommands (all print one line of verdict; exit 0 = ok, 1 = refuse):

  plan  <vault> <folder> <prefix> <ids-file> <status.json> <dry.json> <expect> [allow-refused]
        Refuses unless every entry in both plans is confined to <folder>/,
        names a file whose base name starts with <prefix>, and (for anything
        but a create) belongs to a note id listed in <ids-file>. <expect> is a
        comma list of kinds the plan must contain exactly (order-free), or "-"
        to skip that check.
  dedupe <vault>
        Removes untracked note files that are byte-identical copies of a
        tracked file with the same apple-note-id (see README: clone writes one
        duplicate). Prints only a count.
  ids <vault> <folder> <prefix>
        Prints the apple-note-id of every note file in <folder> whose name
        starts with <prefix>.
  folder-clean <vault> <folder> <prefix>
        Refuses if <folder> holds a note file not starting with "itest-".
  same-notes <dirA> <dirB>
        Refuses unless both folders hold the same notes (by apple-note-id)
        with byte-identical contents. File names may differ: in in-body title
        mode neither push nor pull renames a retitled note's file (as in
        icloud-md), while a fresh clone names it by title.
  pull <pull.json> <expect-conflict:0|1>
        Checks a pull summary's conflict list is empty (0) or not (1).
"""

import json
import os
import sys


def note_id(path):
    try:
        with open(path, encoding="utf-8") as f:
            lines = f.read().split("\n")
    except OSError:
        return None
    if not lines or lines[0] != "---":
        return None
    for line in lines[1:]:
        if line == "---":
            break
        if line.startswith("apple-note-id:"):
            return line.split(":", 1)[1].strip()
    return None


def load_state(vault):
    with open(os.path.join(vault, ".icloud-md", "state.json"), encoding="utf-8") as f:
        return json.load(f)


def refuse(msg):
    print("REFUSE: " + msg)
    sys.exit(1)


def plan(vault, folder, prefix, ids_file, status_path, dry_path, expect, allow_refused=False):
    with open(status_path, encoding="utf-8") as f:
        status = json.load(f)
    with open(dry_path, encoding="utf-8") as f:
        dry = json.load(f)
    ids = set()
    if os.path.exists(ids_file):
        ids = {l.strip() for l in open(ids_file, encoding="utf-8") if l.strip()}
    state = load_state(vault)
    by_file = {v["file"]: k for k, v in state["notes"].items()}

    if status.get("entries") != dry.get("entries"):
        refuse("status and push --dry-run plans differ")
    entries = dry.get("entries", [])
    ok_res = {"ready", "noop"} | ({"refused", "conflict"} if allow_refused else set())
    for i, e in enumerate(entries):
        where = f"entry {i} ({e.get('kind')})"
        if e.get("resolution") not in ok_res:
            refuse(f"{where}: resolution {e.get('resolution')}")
        if e.get("kind") == "createFolder":
            if e.get("file").rstrip("/") != folder:
                refuse(f"{where}: folder outside containment")
            continue
        for key in ("file", "previousFile", "pendingRename"):
            p = e.get(key)
            if p is None:
                continue
            if not p.startswith(folder + "/") or "/" in p[len(folder) + 1 :]:
                refuse(f"{where}: {key} outside {folder}/")
            if not os.path.basename(p).startswith(prefix):
                refuse(f"{where}: {key} lacks this run's prefix")
        if e["kind"] == "create":
            nid = note_id(os.path.join(vault, e["file"]))
            if nid is not None:
                refuse(f"{where}: create of a file that already carries an apple-note-id")
            continue
        nid = by_file.get(e.get("previousFile") or e["file"]) or note_id(os.path.join(vault, e["file"]))
        if nid is None or nid not in ids:
            refuse(f"{where}: note is not one this run created")
    kinds = sorted(e["kind"] for e in entries)
    if expect != "-":
        want = sorted(k for k in expect.split(",") if k)
        if kinds != want:
            refuse(f"plan kinds {kinds} != expected {want}")
    print(f"OK: {len(entries)} entr{'y' if len(entries) == 1 else 'ies'} {kinds}, all contained")


def dedupe(vault):
    state = load_state(vault)
    tracked = {v["file"]: k for k, v in state["notes"].items()}
    removed = 0
    for root, dirs, files in os.walk(vault):
        dirs[:] = [d for d in dirs if not d.startswith(".")]
        for name in files:
            if not name.endswith(".md"):
                continue
            path = os.path.join(root, name)
            rel = os.path.relpath(path, vault)
            if rel in tracked:
                continue
            nid = note_id(path)
            if nid is None or nid not in state["notes"]:
                continue
            twin = os.path.join(vault, state["notes"][nid]["file"])
            if os.path.exists(twin) and open(twin, "rb").read() == open(path, "rb").read():
                os.remove(path)
                removed += 1
    print(f"removed {removed} untracked duplicate(s)")


def ids(vault, folder, prefix):
    d = os.path.join(vault, folder)
    if not os.path.isdir(d):
        return
    for name in sorted(os.listdir(d)):
        if name.startswith(prefix) and name.endswith(".md"):
            nid = note_id(os.path.join(d, name))
            if nid:
                print(nid)


def folder_clean(vault, folder):
    d = os.path.join(vault, folder)
    if not os.path.isdir(d):
        print("OK: folder absent")
        return
    foreign = [n for n in os.listdir(d) if not n.startswith("itest-")]
    if foreign:
        refuse(f"{folder} holds {len(foreign)} entry(ies) without an itest- prefix")
    print(f"OK: folder present with {len(os.listdir(d))} itest leftover(s)")


def same_notes(a, b):
    def load(d):
        out = {}
        for name in os.listdir(d) if os.path.isdir(d) else []:
            path = os.path.join(d, name)
            nid = note_id(path)
            if nid is None:
                refuse(f"{path} has no apple-note-id")
            out[nid] = (name, open(path, "rb").read())
        return out
    na, nb = load(a), load(b)
    if set(na) != set(nb):
        refuse(f"note ids differ: {sorted(set(na) ^ set(nb))}")
    for nid in na:
        if na[nid][1] != nb[nid][1]:
            refuse(f"{nid}: contents differ ({na[nid][0]!r} vs {nb[nid][0]!r})")
    renamed = [f"{na[i][0]!r} -> {nb[i][0]!r}" for i in na if na[i][0] != nb[i][0]]
    print(f"OK: {len(na)} note(s) byte-identical" + (f"; names differ: {renamed}" if renamed else ""))


def pull(path, expect_conflict):
    with open(path, encoding="utf-8") as f:
        s = json.load(f)
    has = bool(s.get("conflicts"))
    if has != (expect_conflict == "1"):
        refuse(f"pull conflicts={len(s.get('conflicts', []))}, expected {'some' if expect_conflict == '1' else 'none'}")
    print(f"OK: pull added={s['added']} updated={s['updated']} merged={s['merged']} removed={s['removed']} conflicts={len(s['conflicts'])}")


if __name__ == "__main__":
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "plan":
        plan(*args[:7], allow_refused=len(args) > 7 and args[7] == "allow-refused")
    elif cmd == "dedupe":
        dedupe(*args)
    elif cmd == "ids":
        ids(*args)
    elif cmd == "folder-clean":
        folder_clean(args[0], args[1])
    elif cmd == "same-notes":
        same_notes(*args)
    elif cmd == "pull":
        pull(*args)
    else:
        sys.exit(f"unknown subcommand {cmd}")
