#!/bin/bash
# Install icloud-notes on Omarchy (or any Arch Linux) from its signed package
# repository, and keep it updating with the system:
#
#   curl -fsSL https://ferdousbhai.com/icloud-notes/install.sh | sudo bash
#
# Every step is idempotent, so re-running is safe. It trusts the package-signing
# key (checked against the fingerprint pinned below), adds the repository,
# installs an Omarchy hook that restores the repository after
# `omarchy refresh pacman` rewrites /etc/pacman.conf, and installs the app.
set -euo pipefail

REPO=icloud-notes
RELEASES=https://github.com/ferdousbhai/icloud-notes/releases/latest/download
SIGNING_KEY_FINGERPRINT=35C47A06567940B6796B4D0F9B3C7BDF85268B31
CONF=/etc/pacman.d/$REPO.conf
INCLUDE="Include = $CONF"

if (( EUID != 0 )); then
  echo "Run this as root:  curl -fsSL https://ferdousbhai.com/icloud-notes/install.sh | sudo bash" >&2
  exit 1
fi
if ! command -v pacman >/dev/null; then
  echo "pacman not found: this installer is for Omarchy and other Arch Linux systems." >&2
  exit 1
fi
if [[ ! $SIGNING_KEY_FINGERPRINT =~ ^[0-9A-F]{40}$ ]]; then
  echo "This copy of install.sh has no signing key pinned; nothing was changed." >&2
  exit 1
fi

echo "Trusting the $REPO package-signing key ($SIGNING_KEY_FINGERPRINT)"
key=$(mktemp)
trap 'rm -f "$key"' EXIT
curl -fsSL "$RELEASES/$REPO-signing-key.asc" -o "$key"
if ! gpg --batch --with-colons --show-keys "$key" 2>/dev/null | grep -q "^fpr:*:$SIGNING_KEY_FINGERPRINT:"; then
  echo "The downloaded key does not match the pinned fingerprint; nothing was changed." >&2
  exit 1
fi
pacman-key --add "$key"
pacman-key --lsign-key "$SIGNING_KEY_FINGERPRINT"

echo "Adding the [$REPO] repository"
cat >"$CONF" <<EOF
[$REPO]
SigLevel = Required DatabaseRequired
Server = $RELEASES
EOF
grep -qxF "$INCLUDE" /etc/pacman.conf || printf '\n%s\n' "$INCLUDE" >>/etc/pacman.conf

# `omarchy refresh pacman` copies Omarchy's default pacman.conf over ours, then
# runs the user's pre-refresh-pacman hooks. This hook puts the Include back.
user=${SUDO_USER:-}
home=$(getent passwd "${user:-root}" | cut -d: -f6)
if [[ -n $user && -d $home/.config/omarchy ]]; then
  hook_dir=$home/.config/omarchy/hooks/pre-refresh-pacman.d
  group=$(id -gn "$user")
  install -d -o "$user" -g "$group" "$hook_dir"
  cat >"$hook_dir/$REPO" <<EOF
#!/bin/bash
# Restore the [$REPO] repository after Omarchy rewrote /etc/pacman.conf.
grep -qxF '$INCLUDE' /etc/pacman.conf || printf '\\n%s\\n' '$INCLUDE' | sudo tee -a /etc/pacman.conf >/dev/null
EOF
  chown "$user:$group" "$hook_dir/$REPO"
  chmod 755 "$hook_dir/$REPO"
  echo "Installed the Omarchy pre-refresh-pacman hook"
fi

echo "Installing $REPO"
pacman -Sy
# omarchy-pkg-add is Omarchy's own wrapper around pacman -S; using it where it
# exists keeps the install consistent with `omarchy pkg add`.
if command -v omarchy-pkg-add >/dev/null; then
  omarchy-pkg-add "$REPO"
else
  pacman -S --needed --noconfirm "$REPO"
fi

cat <<EOF

Done. Launch "Notes (iCloud)" from the app launcher (Super + Space).
Syncing needs the icloud-md CLI:  npm install -g icloud-md   (Node.js 20+)
Updates arrive with the rest of the system through: omarchy update
EOF
