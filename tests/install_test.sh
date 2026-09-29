#!/usr/bin/env bash
# Run install.sh and the generated per-app installers for real, with
# pacman, pacman-key, curl and gpg stubbed, inside a user and mount
# namespace where /etc/pacman.conf, /etc/pacman.d and /root are scratch
# copies: nothing on this machine changes. Covers the default package
# lists, named packages, an unknown name, re-running, the migration from
# the old per-app repositories, and the add_signed_repo hash of every
# generated installer. Skips (exit 0) where unprivileged namespaces are off.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD"

if [[ ${ICLOUD_INSTALL_TEST_INNER:-} != 1 ]]; then
  if ! unshare -rm true 2>/dev/null; then
    echo "skip install_test: unprivileged user namespaces are unavailable" >&2
    exit 0
  fi
  exec env ICLOUD_INSTALL_TEST_INNER=1 unshare -rm "$root/tests/install_test.sh"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail=0
check() { # check <description> <command...>
  local what=$1
  shift
  if "$@"; then echo "ok $what"; else echo "FAIL $what"; fail=1; fi
}
not() { ! "$@"; }

# Stubs: pacman logs its arguments, curl "downloads" an empty key, gpg
# reports the pinned fingerprint for it.
mkdir -p "$work/bin"
cat >"$work/bin/pacman" <<'STUB'
#!/bin/sh
echo "pacman $*" >>"$PACMAN_LOG"
STUB
cat >"$work/bin/pacman-key" <<'STUB'
#!/bin/sh
exit 0
STUB
cat >"$work/bin/curl" <<'STUB'
#!/bin/sh
while [ $# -gt 0 ]; do [ "$1" = -o ] && : >"$2"; shift; done
STUB
fingerprint=$(sed -n 's/^SIGNING_KEY_FINGERPRINT=//p' install.sh)
printf '#!/bin/sh\necho "fpr:::::::::%s:"\n' "$fingerprint" >"$work/bin/gpg"
chmod 755 "$work/bin/"*
export PATH="$work/bin:$PATH" PACMAN_LOG="$work/pacman.log" SUDO_USER=root

bin/make-installers "$work/dist" >/dev/null
hash_of() { sed -n '/^# --- add_signed_repo (shared) ---$/,/^# --- end add_signed_repo ---$/p' "$1" | sed '1d;$d' | sha256sum | cut -d' ' -f1; }
for f in install.sh "$work"/dist/install-*.sh; do
  check "add_signed_repo hash in $(basename "$f")" [ "$(hash_of "$f")" = "$(cat tests/add_signed_repo.sha256)" ]
done

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
installed() { grep '^pacman -Syu' "$PACMAN_LOG" | tail -1 | sed 's/^pacman -Syu --needed --noconfirm //'; }
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

for app in notes photos findmy; do
  setup
  bash "$work/dist/install-$app.sh" >"$work/out" 2>&1
  check "install-$app.sh installs only icloud-$app" [ "$(installed)" = "icloud-$app" ]
done
check "install-findmy.sh does not start Notes' sync" not grep -q 'Background sync' "$work/out"

setup
bash "$work/dist/install-photos.sh" icloud-session icloud-notes-sync >/dev/null 2>&1
check "named packages override the default" [ "$(installed)" = "icloud-session icloud-notes-sync" ]

setup
status=0
bash install.sh icloud-nope >/dev/null 2>&1 || status=$?
check "an unknown package exits 64" [ "$status" = 64 ]
check "an unknown package changes nothing" [ ! -e /etc/pacman.d/icloud-for-omarchy.conf ]

# Migration: the released [icloud-notes] (conf, Include, hook), plus
# partial leftovers of the others.
setup
printf '[icloud-notes]\nServer = x\n' >/etc/pacman.d/icloud-notes.conf
printf '[icloud-session]\nServer = x\n' >/etc/pacman.d/icloud-session.conf
printf '\nInclude = /etc/pacman.d/icloud-notes.conf\n\nInclude = /etc/pacman.d/icloud-session.conf\n\nInclude = /etc/pacman.d/icloud-notes-sync.conf\n' >>/etc/pacman.conf
touch "$hooks/icloud-notes" "$hooks/icloud-photos" "$hooks/unrelated"
touch /etc/pacman.d/unrelated.conf
bash install.sh icloud-notes >"$work/out" 2>&1
for name in icloud-notes icloud-session icloud-notes-sync icloud-photos icloud-findmy; do
  check "migration: no [$name] Include left" [ "$(count_include "$name")" = 0 ]
  check "migration: no $name.conf left" [ ! -e "/etc/pacman.d/$name.conf" ]
  check "migration: no $name hook left" [ ! -e "$hooks/$name" ]
done
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
