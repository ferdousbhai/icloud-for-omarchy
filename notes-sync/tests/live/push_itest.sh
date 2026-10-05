#!/usr/bin/env bash
# Live write test against a REAL iCloud Notes account. See tests/live/README.md.
# Never run by default: requires ICLOUD_NOTES_SYNC_LIVE=1.
set -uo pipefail

[[ ${ICLOUD_NOTES_SYNC_LIVE:-} == 1 ]] || { echo "refusing: set ICLOUD_NOTES_SYNC_LIVE=1 to run live writes" >&2; exit 2; }
ACCOUNT=${ICLOUD_NOTES_SYNC_ITEST_ACCOUNT:?set ICLOUD_NOTES_SYNC_ITEST_ACCOUNT to an Apple ID or dsid}
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=${ICLOUD_NOTES_SYNC_BIN:-$REPO/../target/release/icloud-notes-sync}  # the workspace target dir
FOLDER=${ICLOUD_NOTES_SYNC_ITEST_FOLDER:-icloud-notes-sync-itest}
RUN=$(date +%Y%m%d%H%M%S)
PREFIX="itest-$RUN "
WORK=${ICLOUD_NOTES_SYNC_ITEST_WORKROOT:-$HOME/.cache/icloud-apps-test/push-live}/run-$RUN
case "$WORK/" in "$HOME/Documents/"*) echo "refusing: work root under ~/Documents" >&2; exit 2 ;; esac
LOG=$WORK/logs
IDS=$WORK/created-ids.txt
SUMMARY=$WORK/summary.tsv
GUARD=(python3 "$REPO/tests/live/guard.py")
mkdir -p "$LOG"
touch "$IDS"
printf 'n\tlabel\texit\tms\n' >"$SUMMARY"
n=0

[[ -x $BIN ]] || { echo "build first: cargo build --release" >&2; exit 2; }
echo "account=$ACCOUNT folder=$FOLDER run=$RUN work=$WORK"

die() { echo "FAIL: $*" | tee -a "$SUMMARY"; echo "scratch kept at $WORK"; exit 1; }
step() { echo; echo "== $*"; echo "## $*" >>"$SUMMARY"; }

# run <label> <dir> <cmd...>: stdout -> logs/NN-label.json, stderr -> .err; sets RC and OUT.
run() {
  local label=$1 dir=$2
  shift 2
  n=$((n + 1))
  local f
  f=$(printf '%s/%02d-%s' "$LOG" "$n" "$label")
  local t0
  t0=$(date +%s%3N)
  (cd "$dir" && "$@") >"$f.json" 2>"$f.err"
  RC=$?
  local ms=$(($(date +%s%3N) - t0))
  OUT=$f.json
  printf '%s\t%s\t%s\t%s\n' "$n" "$label" "$RC" "$ms" >>"$SUMMARY"
  echo "  [$label] exit=$RC ${ms}ms"
}
want_rc() { for c in "$@"; do [[ $RC == "$c" ]] && return 0; done; die "exit $RC, expected $* (see $OUT)"; }
guard() { "${GUARD[@]}" "$@" | tee -a "$SUMMARY"; return "${PIPESTATUS[0]}"; }

# Clones are read-only, so a transient CloudKit failure (HTTP 5xx, exit 1) is retried once.
clone_with() {
  local label=$1 dir=$2
  shift 2
  run "$label" "$WORK" "$@" --json clone --account "$ACCOUNT" --non-interactive "$dir"
  if [[ $RC == 1 ]]; then
    mv "$WORK/$dir" "$WORK/$dir.failed-$n" 2>/dev/null
    run "$label-retry" "$WORK" "$@" --json clone --account "$ACCOUNT" --non-interactive "$dir"
  fi
  want_rc 0
}
port_clone() { clone_with "clone-$1" "$1" "$BIN"; guard dedupe "$WORK/$1"; }

# plan_check <vault> <expected kinds|-> [allow-refused]: status + dry-run, both logged, then the guard.
plan_check() {
  local v=$1 expect=$2 allow=${3:-}
  run status "$v" "$BIN" --json status; want_rc 0 3
  local s=$OUT
  run push-dry-run "$v" "$BIN" --json push --dry-run; want_rc 0 3
  guard plan "$v" "$FOLDER" "$PREFIX" "$IDS" "$s" "$OUT" "$expect" $allow || die "containment guard refused the plan"
}

# guarded_push <vault> <expected kinds|->
guarded_push() {
  plan_check "$1" "$2"
  run push "$1" "$BIN" --json push; want_rc 0
  "${GUARD[@]}" ids "$1" "$FOLDER" "$PREFIX" >>"$IDS"
  sort -u -o "$IDS" "$IDS"
}

expect_clean() { run status-clean "$1" "$BIN" --json status; want_rc 0; }

pull() { run pull "$1" "$BIN" --json pull; want_rc 0; guard pull "$OUT" "$2" || die "pull conflict expectation"; }

# verify <label> <vault>: a fresh clone (a separate vault, so nothing the
# pushing vault remembers) must hold the vault's notes byte for byte.
verify() {
  local label=$1 v=$2
  port_clone "fresh-$label"
  local o="$WORK/fresh-$label/$FOLDER"
  guard same-notes "$v/$FOLDER" "$o" || die "$label: vault folder differs from a fresh clone"
  echo "  verified: $(find "$o" -name "$PREFIX*" | wc -l) run note(s); vault == fresh clone by note id" | tee -a "$SUMMARY"
  ORC=$o
}

has() { grep -qF -- "$2" "$1" || die "expected text missing in $1: $2"; }
hasnt() { ! grep -qF -- "$2" "$1" || die "unexpected text in $1: $2"; }
edit() { # edit <file> <old line> <new line>: exact whole-line replacement, must hit once
  python3 - "$@" <<'EOF' || die "edit failed on $1"
import sys
path, old, new = sys.argv[1:4]
lines = open(path, encoding="utf-8").read().split("\n")
hits = [i for i, l in enumerate(lines) if l == old]
if len(hits) != 1:
    sys.exit(f"{len(hits)} matches for {old!r}")
lines[hits[0]] = new
open(path, "w", encoding="utf-8").write("\n".join(lines))
EOF
}

A=$WORK/vaultA
B=$WORK/vaultB
N1="${PREFIX}note one.md"

step "1. clone, create folder + note, push"
port_clone vaultA
guard folder-clean "$A" "$FOLDER" || die "containment folder holds foreign notes"
expect1="createFolder,create"
[[ -d $A/$FOLDER ]] && expect1="create"
mkdir -p "$A/$FOLDER"
cat >"$A/$FOLDER/$N1" <<EOF
# ${PREFIX}note one

Plain text line alpha.

## Checklist heading

- [ ] unchecked item
- [x] checked item

Some **bold** and *italic* text with a [link](https://example.com/itest).

Line omega.
EOF
cp "$A/$FOLDER/$N1" "$LOG/note1-as-written.md"
guarded_push "$A" "$expect1"
diff "$LOG/note1-as-written.md" "$A/$FOLDER/$N1" >"$LOG/note1-roundtrip.diff"
verify s1 "$A"
has "$ORC/$N1" "https://example.com/itest"
expect_clean "$A"

step "2. edit note one (append, reformat), push"
F1="$A/$FOLDER/$N1"
edit "$F1" "Plain text line alpha." "**Plain text line alpha.**"
edit "$F1" "- [ ] unchecked item" "- [x] unchecked item"
printf '\nAppended line one.\n\nAppended line two.\n' >>"$F1"
guarded_push "$A" "update"
verify s2 "$A"
has "$ORC/$N1" "**Plain text line alpha.**"
has "$ORC/$N1" "Appended line two."
cp "$F1" "$LOG/note1-before-echo-pull.md"
pull "$A" 0
cmp -s "$F1" "$LOG/note1-before-echo-pull.md" && echo "  pull after own push left note one byte-identical" | tee -a "$SUMMARY" \
  || echo "  NOTE: pull after own push rewrote note one (see logs)" | tee -a "$SUMMARY"
expect_clean "$A"

step "3. create note two, push; retitle, push; rename file, push"
N2="${PREFIX}note two.md"
printf '# %snote two\n\nSecond note body.\n' "$PREFIX" >"$A/$FOLDER/$N2"
guarded_push "$A" "create"
verify s3a "$A"
edit "$A/$FOLDER/$N2" "# ${PREFIX}note two" "# ${PREFIX}note two retitled"
guarded_push "$A" "-"
ls "$A/$FOLDER" >"$LOG/s3b-files.txt"
verify s3b "$A"
# In-body title mode: the retitle does not rename the local file (push or
# pull); a fresh clone names it by the new title.
[[ -f "$A/$FOLDER/$N2" ]] || die "retitle renamed the local file"
has "$ORC/${PREFIX}note two retitled.md" "# ${PREFIX}note two retitled"
pull "$A" 0
[[ -f "$A/$FOLDER/$N2" ]] || die "pull renamed the local file"
mv "$A/$FOLDER/$N2" "$A/$FOLDER/${PREFIX}note two renamed.md"
guarded_push "$A" "-"
ls "$A/$FOLDER" >"$LOG/s3c-files.txt"
verify s3c "$A"
expect_clean "$A"

step "4a. concurrent edits on different lines: clean merge"
port_clone vaultB
edit "$A/$FOLDER/$N1" "**Plain text line alpha.**" "**Plain text line alpha.** A-edit"
guarded_push "$A" "update"
edit "$B/$FOLDER/$N1" "Appended line two." "Appended line two. B-edit"
pull "$B" 0
has "$B/$FOLDER/$N1" "A-edit"
has "$B/$FOLDER/$N1" "B-edit"
hasnt "$B/$FOLDER/$N1" "<<<<<<<"
guarded_push "$B" "update"
verify s4a "$B"
has "$ORC/$N1" "A-edit"
has "$ORC/$N1" "B-edit"

step "4b. concurrent edits on the same line: conflict markers, resolve, push"
pull "$A" 0
edit "$A/$FOLDER/$N1" "Appended line one." "Appended line one A-conflict."
guarded_push "$A" "update"
edit "$B/$FOLDER/$N1" "Appended line one." "Appended line one B-conflict."
pull "$B" 1
has "$B/$FOLDER/$N1" "<<<<<<<"
cp "$B/$FOLDER/$N1" "$LOG/note1-with-markers.md"
plan_check "$B" "update" allow-refused
python3 -c 'import json,sys; e=json.load(open(sys.argv[1]))["entries"]; sys.exit(0 if e and all(x["resolution"] in ("refused", "conflict") for x in e) else 1)' "$OUT" \
  || die "the plan would push a file with conflict markers"
python3 - "$B/$FOLDER/$N1" <<'EOF' || die "resolving markers"
import re, sys
p = sys.argv[1]
t = open(p, encoding="utf-8").read()
t, k = re.subn(r"(?ms)^<<<<<<<[^\n]*\n.*?^>>>>>>>[^\n]*\n", "Appended line one A-conflict B-conflict resolved.\n", t)
assert k == 1, k
open(p, "w", encoding="utf-8").write(t)
EOF
guarded_push "$B" "update"
verify s4b "$B"
has "$ORC/$N1" "Appended line one A-conflict B-conflict resolved."
pull "$A" 0
expect_clean "$A"
expect_clean "$B"

step "6. edits on different lines in two vaults; the second pushes without pulling: merged in one push"
edit "$A/$FOLDER/$N1" "Line omega." "Line omega. A-six"
guarded_push "$A" "update"
edit "$B/$FOLDER/$N1" "## Checklist heading" "## Checklist heading B-six"
guarded_push "$B" "update"
has "$B/$FOLDER/$N1" "A-six"
verify s6 "$B"
has "$ORC/$N1" "A-six"
has "$ORC/$N1" "B-six"
expect_clean "$B"
pull "$A" 0
expect_clean "$A"

step "7. a copy of a note's file is a new note"
C1="${PREFIX}note one copy.md"
cp "$A/$FOLDER/$N1" "$A/$FOLDER/$C1"
guarded_push "$A" "create"
python3 "$REPO/tests/live/guard.py" ids "$A" "$FOLDER" "$PREFIX" | sort | uniq -d | grep -q . && die "the copy kept the original's id"
verify s7 "$A"
rm "$A/$FOLDER/$C1"
guarded_push "$A" "delete"
expect_clean "$A"

step "8. emptying a note moves it to Recently Deleted"
N3="${PREFIX}note three.md"
printf '# %snote three\n\nTo be emptied.\n' "$PREFIX" >"$A/$FOLDER/$N3"
guarded_push "$A" "create"
id3=$(python3 -c 'import sys; sys.path.insert(0, sys.argv[1]); import guard; print(guard.note_id(sys.argv[2]))' "$REPO/tests/live" "$A/$FOLDER/$N3")
printf -- '---\napple-note-id: %s\n---\n' "$id3" >"$A/$FOLDER/$N3"
guarded_push "$A" "delete"
[[ -e "$A/$FOLDER/$N3" ]] && die "the emptied file is still there"
verify s8 "$A"
expect_clean "$A"

step "9. a subfolder: made empty, renamed, a note moved in with an edit, deleted with its note"
S1="$FOLDER/${PREFIX}sub"
S2="$FOLDER/${PREFIX}sub renamed"
mkdir "$A/$S1"
guarded_push "$A" "createFolder"
port_clone fresh-s9a
[[ -d "$WORK/fresh-s9a/$S1" ]] || die "the empty subfolder isn't in iCloud"
mv "$A/$S1" "$A/$S2"
guarded_push "$A" "renameFolder"
port_clone fresh-s9b
[[ -d "$WORK/fresh-s9b/$S2" && ! -e "$WORK/fresh-s9b/$S1" ]] || die "the subfolder wasn't renamed in iCloud"
N4="${PREFIX}note four.md"
printf '# %snote four\n\nFour body.\n' "$PREFIX" >"$A/$FOLDER/$N4"
guarded_push "$A" "create"
mv "$A/$FOLDER/$N4" "$A/$S2/$N4"
printf '\nMoved and edited.\n' >>"$A/$S2/$N4"
guarded_push "$A" "move"
port_clone fresh-s9c
has "$WORK/fresh-s9c/$S2/$N4" "Moved and edited."
cmp -s "$A/$S2/$N4" "$WORK/fresh-s9c/$S2/$N4" || die "the moved note differs from a fresh clone"
expect_clean "$A"
rm -r "$A/$S2"
guarded_push "$A" "delete,deleteFolder"
port_clone fresh-s9d
[[ ! -e "$WORK/fresh-s9d/$S2" ]] || die "the deleted subfolder is still in iCloud"
expect_clean "$A"

step "5. delete both notes, push (folder stays)"
rm "$A/$FOLDER/$N1" "$A/$FOLDER/${PREFIX}note two renamed.md"
guarded_push "$A" "delete,delete"
port_clone fresh-s5
for d in "$WORK/fresh-s5"; do
  left=$(find "$d/$FOLDER" -name "$PREFIX*" 2>/dev/null | wc -l)
  [[ $left == 0 ]] || die "$d still holds $left run note(s)"
  echo "  $(basename "$d"): run notes gone; folder dir $([[ -d $d/$FOLDER ]] && echo present || echo absent)" | tee -a "$SUMMARY"
done
expect_clean "$A"

echo
echo "PASS run=$RUN (scratch at $WORK; empty '$FOLDER' left in place)"
echo "PASS" >>"$SUMMARY"
