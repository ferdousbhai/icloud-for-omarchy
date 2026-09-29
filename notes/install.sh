#!/bin/bash
# Install icloud-notes on Omarchy (or any Arch Linux) from its signed package
# repository, and keep it updating with the system:
#
#   curl -fsSL https://ferdousbhai.com/icloud-notes/install.sh | sudo bash
#
# Every step is idempotent, so re-running is safe. It trusts the package-signing
# key (checked against the fingerprint pinned below), adds the repositories
# for the app and for what it depends on (icloud-session, the Apple sign-in
# every iCloud app shares, and icloud-notes-sync, the sync engine), installs
# an Omarchy hook per repository that restores it after
# `omarchy refresh pacman` rewrites /etc/pacman.conf, and installs the app.
set -euo pipefail

REPO=icloud-notes
RELEASES=https://github.com/ferdousbhai/icloud-notes/releases/latest/download
SESSION_REPO=icloud-session
SESSION_RELEASES=https://github.com/ferdousbhai/icloud-session/releases/latest/download
SYNC_REPO=icloud-notes-sync
SYNC_RELEASES=https://github.com/ferdousbhai/icloud-notes-sync/releases/latest/download
SIGNING_KEY_FINGERPRINT=35C47A06567940B6796B4D0F9B3C7BDF85268B31

# --- add_signed_repo (shared) ---
# Trust a project's package-signing key (checked against the pinned
# fingerprint), add its signed pacman repository, and keep the repository
# across `omarchy refresh pacman`, which rewrites /etc/pacman.conf from
# Omarchy's defaults and then runs the user's pre-refresh-pacman hooks.
# Works as root (`sudo bash`) or as a desktop user (sudo inside). This text
# is identical in every installer that uses it, and each repository's test
# pins its hash: change it here and in its twins together.
add_signed_repo() {
  local name="$1" release="$2" fingerprint="$3"
  local conf="/etc/pacman.d/$name.conf" include="Include = /etc/pacman.d/$name.conf"
  local sudo='' key user home hook_dir
  (( EUID == 0 )) || sudo=sudo
  key="$(mktemp)"
  if ! curl -fsSL "$release/$name-signing-key.asc" -o "$key"; then
    rm -f "$key"
    echo "Could not download the package-signing key from $release." >&2
    return 1
  fi
  if ! gpg --batch --with-colons --show-keys "$key" 2>/dev/null | grep -q "^fpr:*:$fingerprint:"; then
    rm -f "$key"
    echo "The downloaded key does not match the pinned fingerprint $fingerprint; nothing was changed." >&2
    return 1
  fi
  $sudo pacman-key --add "$key"
  $sudo pacman-key --lsign-key "$fingerprint"
  rm -f "$key"
  printf '[%s]\nSigLevel = Required DatabaseRequired\nServer = %s\n' "$name" "$release" | $sudo tee "$conf" >/dev/null
  grep -qxF "$include" /etc/pacman.conf || printf '\n%s\n' "$include" | $sudo tee -a /etc/pacman.conf >/dev/null
  user="${SUDO_USER:-${USER:-$(id -un)}}"
  home="$(getent passwd "$user" | cut -d: -f6)"
  if [[ -n $home && -d $home/.config/omarchy ]]; then
    hook_dir="$home/.config/omarchy/hooks/pre-refresh-pacman.d"
    install -d -o "$user" -g "$(id -gn "$user")" "$hook_dir"
    printf '%s\n' '#!/bin/bash' \
      "# Restore the [$name] repository after Omarchy rewrote /etc/pacman.conf." \
      "grep -qxF '$include' /etc/pacman.conf || printf '\\n%s\\n' '$include' | sudo tee -a /etc/pacman.conf >/dev/null" \
      > "$hook_dir/$name"
    chown "$user" "$hook_dir/$name"
    chmod 755 "$hook_dir/$name"
  fi
  $sudo pacman -Sy
}
# --- end add_signed_repo ---

if ! command -v pacman >/dev/null; then
  echo "pacman not found: this installer is for Omarchy and other Arch Linux systems." >&2
  exit 1
fi
if [[ ! $SIGNING_KEY_FINGERPRINT =~ ^[0-9A-F]{40}$ ]]; then
  echo "This copy of install.sh has no signing key pinned; nothing was changed." >&2
  exit 1
fi

echo "Adding the [$SESSION_REPO] repository"
add_signed_repo "$SESSION_REPO" "$SESSION_RELEASES" "$SIGNING_KEY_FINGERPRINT"
echo "Adding the [$SYNC_REPO] repository"
add_signed_repo "$SYNC_REPO" "$SYNC_RELEASES" "$SIGNING_KEY_FINGERPRINT"
echo "Adding the [$REPO] repository"
add_signed_repo "$REPO" "$RELEASES" "$SIGNING_KEY_FINGERPRINT"

echo "Installing $REPO"
# Upgrade and install in one transaction. add_signed_repo has just synced
# every repository's database, and installing from those without upgrading
# is Arch's unsupported partial upgrade: a new dependency can need newer
# libraries than the ones installed. (omarchy-pkg-add only runs pacman -S.)
if (( EUID == 0 )); then
  pacman -Syu --needed --noconfirm "$REPO"
else
  sudo pacman -Syu --needed --noconfirm "$REPO"
fi

# Background sync: a systemd user timer syncs every 15 minutes while Notes
# is closed. The package enables it for every user from their next login;
# this starts it now in the desktop user's systemd, not root's.
start_background_sync() {
  local user uid
  local start_cmd='systemctl --user daemon-reload && systemctl --user start icloud-notes-sync.timer'
  user="${SUDO_USER:-${USER:-$(id -un)}}"
  uid="$(id -u "$user" 2>/dev/null)" || return 0
  if (( uid == 0 )) || [[ ! -S /run/user/$uid/bus ]]; then
    echo "Background sync starts at your next login (or now: $start_cmd)."
    return 0
  fi
  local ctl=(env "XDG_RUNTIME_DIR=/run/user/$uid" "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$uid/bus" systemctl --user)
  (( EUID == 0 )) && ctl=(runuser -u "$user" -- "${ctl[@]}")
  if "${ctl[@]}" daemon-reload && "${ctl[@]}" start icloud-notes-sync.timer; then
    echo "Background sync is on (every 15 minutes while Notes is closed)."
  else
    echo "Could not start background sync now; it starts at your next login." >&2
  fi
}
start_background_sync

cat <<EOF

Done. Launch "Notes (iCloud)" from the app launcher (Super + Space).
If you have not signed in to iCloud (here or from another iCloud app), the
app opens Apple's sign-in page when you clone your notes.
Updates arrive with the rest of the system through: omarchy update
EOF
