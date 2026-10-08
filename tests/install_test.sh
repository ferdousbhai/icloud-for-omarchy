#!/usr/bin/env bash
# Run install.sh and the generated per-app installers for real, with
# pacman, pacman-key, curl and gpg stubbed, inside a user and mount
# namespace where /etc/pacman.conf, /etc/pacman.d and /root are scratch
# copies: nothing on this machine changes. Covers the default package
# lists, named packages, an unknown name, re-running, the migration from
# the old [icloud-notes] repository. First, outside the namespace, checks
# the add_signed_repo hash of install.sh and every generated installer;
# only the installer runs are skipped where unprivileged namespaces are off.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail=0
check() { # check <description> <command...>
  local what=$1
  shift
  if "$@"; then echo "ok $what"; else echo "FAIL $what"; fail=1; fi
}
not() { ! "$@"; }

if [[ ${ICLOUD_INSTALL_TEST_INNER:-} != 1 ]]; then
  # add_signed_repo is shared verbatim with Ghost's installer
  # (ferdousbhai/ghost); both repositories pin its hash, so a change to one
  # copy fails here until the twin matches.
  bin/make-installers "$work/dist" >/dev/null
  hash_of() { sed -n '/^# --- add_signed_repo (shared) ---$/,/^# --- end add_signed_repo ---$/p' "$1" | sed '1d;$d' | sha256sum | cut -d' ' -f1; }
  for f in install.sh "$work"/dist/install-*.sh; do
    check "add_signed_repo hash in $(basename "$f")" [ "$(hash_of "$f")" = "$(cat tests/add_signed_repo.sha256)" ]
  done
  if ((fail)); then
    echo "add_signed_repo drifted from its pinned hash: update tests/add_signed_repo.sha256 here and the twin in ferdousbhai/ghost together" >&2
    exit 1
  fi
  if ! unshare -rm true 2>/dev/null; then
    echo "skip install_test: unprivileged user namespaces are unavailable" >&2
    exit 0
  fi
  rm -rf "$work"
  exec env ICLOUD_INSTALL_TEST_INNER=1 unshare -rm "$root/tests/install_test.sh"
fi

# Ubuntu CI has no pacman mount targets. Prepare a scratch /etc inside the
# private mount namespace while preserving account lookups used by hooks.
if [[ ! -f /etc/pacman.conf || ! -d /etc/pacman.d ]]; then
  mkdir -p "$work/etc/pacman.d"
  for name in passwd group nsswitch.conf; do
    [[ ! -f /etc/$name ]] || cp "/etc/$name" "$work/etc/$name"
  done
  touch "$work/etc/pacman.conf"
  mount --bind "$work/etc" /etc
fi

# Stubs: pacman logs its arguments, curl "downloads" an empty key, gpg
# reports the pinned fingerprint for it.
mkdir -p "$work/bin"
cat >"$work/bin/pacman" <<'STUB'
#!/bin/sh
echo "pacman $*" >>"$PACMAN_LOG"
STUB
cat >"$work/bin/omarchy-pkg-add" <<'STUB'
#!/bin/sh
echo "omarchy-pkg-add $*" >>"$PACMAN_LOG"
STUB
cat >"$work/bin/pacman-key" <<'STUB'
#!/bin/sh
exit 0
STUB
cat >"$work/bin/curl" <<'STUB'
#!/bin/sh
while [ $# -gt 0 ]; do [ "$1" = -o ] && : >"$2"; shift; done
STUB
# Keep existing x86 regression cases deterministic on native ARM hosts.
cat >"$work/bin/uname" <<'STUB'
#!/bin/sh
echo x86_64
STUB
fingerprint=$(sed -n 's/^SIGNING_KEY_FINGERPRINT=//p' install.sh)
printf '#!/bin/sh\necho "fpr:::::::::%s:"\n' "$fingerprint" >"$work/bin/gpg"
chmod 755 "$work/bin/"*
export PATH="$work/bin:$PATH" PACMAN_LOG="$work/pacman.log" SUDO_USER=root
bin/make-installers "$work/dist" >/dev/null

# A fresh scratch /etc and /root for each scenario.
setup() {
  local s="$work/scene"
  umount -q /etc/pacman.conf /etc/pacman.d /root 2>/dev/null || true
  rm -rf "$s" "$PACMAN_LOG"
  mkdir -p "$s/pacman.d" "$s/root/.config/omarchy/hooks/pre-refresh-pacman.d"
  printf '[options]\nArchitecture = auto\n\n[core]\nInclude = /etc/pacman.d/mirrorlist\n' >"$s/pacman.conf"
  mount --bind "$s/pacman.conf" /etc/pacman.conf
  mount --bind "$s/pacman.d" /etc/pacman.d
  mount --bind "$s/root" /root
}
hooks=/root/.config/omarchy/hooks/pre-refresh-pacman.d
# What the last install asked for, whichever way it installed (omarchy-pkg-add on Omarchy).
installed() { grep -E '^(pacman -Syu|omarchy-pkg-add) ' "$PACMAN_LOG" | tail -1 | sed -E 's/^(pacman -Syu --needed --noconfirm|omarchy-pkg-add) //'; }
count_include() { grep -cxF "Include = /etc/pacman.d/$1.conf" /etc/pacman.conf || true; }

setup
bash install.sh >"$work/out" 2>&1
check "install.sh with no arguments installs every app" [ "$(installed)" = "icloud-notes icloud-photos icloud-findmy" ]
check "the repository config is written" grep -qx 'Server = https://github.com/ferdousbhai/icloud-for-omarchy/releases/latest/download' /etc/pacman.d/icloud-for-omarchy.conf
check "the Include line is added" [ "$(count_include icloud-for-omarchy)" = 1 ]
check "the Omarchy hook is installed" [ -x "$hooks/icloud-for-omarchy" ]
check "Notes' background sync is mentioned" grep -q 'Background sync' "$work/out"
bash install.sh >/dev/null 2>&1
check "re-running keeps one Include line" [ "$(count_include icloud-for-omarchy)" = 1 ]
check "on Omarchy it installs with omarchy-pkg-add" grep -q '^omarchy-pkg-add icloud-notes' "$PACMAN_LOG"
check "on Omarchy it never runs pacman -Syu (Omarchy's update guard aborts it)" not grep -q '^pacman -Syu' "$PACMAN_LOG"

# Plain Arch: no Omarchy commands anywhere on PATH, so one pacman -Syu transaction.
setup
mkdir -p "$work/plain"
cp "$work/bin/pacman" "$work/bin/pacman-key" "$work/bin/curl" "$work/bin/gpg" "$work/bin/uname" "$work/plain/"
for tool in bash sh sed grep mktemp rm tee id getent cut install chown chmod cat env dirname basename tr sort head tail; do
  ln -sf "$(command -v "$tool")" "$work/plain/$tool"
done
PATH="$work/plain" bash install.sh >"$work/plain.out" 2>&1 || true
check "without Omarchy it installs in one pacman -Syu" grep -qx 'pacman -Syu --needed --noconfirm icloud-notes icloud-photos icloud-findmy' "$PACMAN_LOG"

for app in notes photos findmy; do
  setup
  bash "$work/dist/install-$app.sh" >"$work/out" 2>&1
  check "install-$app.sh installs only icloud-$app" [ "$(installed)" = "icloud-$app" ]
done
check "install-findmy.sh does not start Notes' sync" not grep -q 'Background sync' "$work/out"

setup
bash "$work/dist/install-photos.sh" icloud-session icloud-notes >/dev/null 2>&1
check "named packages override the default" [ "$(installed)" = "icloud-session icloud-notes" ]

# The sync engine is inside icloud-notes now, not a package to name.
setup
status=0
bash install.sh icloud-notes-sync >/dev/null 2>&1 || status=$?
check "icloud-notes-sync is no longer a package" [ "$status" = 64 ]

setup
status=0
bash install.sh icloud-nope >/dev/null 2>&1 || status=$?
check "an unknown package exits 64" [ "$status" = 64 ]
check "an unknown package changes nothing" [ ! -e /etc/pacman.d/icloud-for-omarchy.conf ]

# Migration: the released [icloud-notes] (conf, Include, hook).
setup
printf '[icloud-notes]\nServer = x\n' >/etc/pacman.d/icloud-notes.conf
printf '\nInclude = /etc/pacman.d/icloud-notes.conf\n' >>/etc/pacman.conf
touch "$hooks/icloud-notes" "$hooks/unrelated"
touch /etc/pacman.d/unrelated.conf
bash install.sh icloud-notes >"$work/out" 2>&1
check "migration: no [icloud-notes] Include left" [ "$(count_include icloud-notes)" = 0 ]
check "migration: no icloud-notes.conf left" [ ! -e /etc/pacman.d/icloud-notes.conf ]
check "migration: no icloud-notes hook left" [ ! -e "$hooks/icloud-notes" ]
check "migration: [icloud-for-omarchy] is in place" [ "$(count_include icloud-for-omarchy)" = 1 ]
check "migration: the new hook is in place" [ -x "$hooks/icloud-for-omarchy" ]
check "migration: other repositories and hooks are kept" grep -qxF 'Include = /etc/pacman.d/mirrorlist' /etc/pacman.conf
check "migration: unrelated pacman.d files are kept" [ -e /etc/pacman.d/unrelated.conf ]
check "migration: unrelated hooks are kept" [ -e "$hooks/unrelated" ]
check "migration: says what it removed" grep -q 'Removing the old \[icloud-notes\] repository' "$work/out"
check "migration: then installs" [ "$(installed)" = "icloud-notes" ]

umount -q /etc/pacman.conf /etc/pacman.d /root 2>/dev/null || true
if (( fail )); then
  echo "FAIL install_test" >&2
  exit 1
fi
echo "ok install_test"
