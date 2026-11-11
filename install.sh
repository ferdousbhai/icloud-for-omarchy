#!/bin/bash
# Install the iCloud apps for Omarchy (or any Arch Linux) from their signed
# package repository, and keep them updating with the system:
#
#   curl -fsSL https://github.com/ferdousbhai/icloud-for-omarchy/releases/latest/download/install.sh | sudo bash
#   curl -fsSL .../install.sh | sudo bash -s -- icloud-photos        # just one app
#
# With no arguments it installs DEFAULT_PACKAGES below: every app
# (icloud-notes, icloud-photos, icloud-findmy, icloud-drive). Name packages to install only those; icloud-session
# (the Apple sign-in every app shares) can be named too, and comes in as a
# dependency anyway. Each release also carries install-notes.sh,
# install-photos.sh, install-findmy.sh and install-drive.sh: this script with DEFAULT_PACKAGES set to that one
# app (bin/make-installers writes them).
#
# Every step is idempotent, so re-running is safe. It trusts the
# package-signing key (checked against the fingerprint pinned below), adds
# the one [icloud-for-omarchy] repository that holds every package,
# installs an Omarchy hook that restores it after `omarchy refresh pacman`
# rewrites /etc/pacman.conf, removes the [icloud-notes] repository earlier
# Notes releases used, and installs the packages.
set -euo pipefail

REPO=icloud-for-omarchy
RELEASES=${ICLOUD_RELEASES:-https://github.com/ferdousbhai/icloud-for-omarchy/releases/latest/download}
SIGNING_KEY_FINGERPRINT=52130299581DAD685226900CA89CA1A6A1E74251
PACKAGES=(icloud-session icloud-notes icloud-photos icloud-findmy icloud-drive)
# What a run with no arguments installs. bin/make-installers rewrites this
# one line for the per-app installers.
DEFAULT_PACKAGES=(icloud-notes icloud-photos icloud-findmy icloud-drive)
# Repositories of earlier releases: Notes had one of its own.
OLD_REPOS=(icloud-notes)

# --- add_signed_repo (shared) ---
# Trust a project's package-signing key (checked against the pinned
# fingerprint), add its signed pacman repository, and keep the repository
# across `omarchy refresh pacman`, which rewrites /etc/pacman.conf from
# Omarchy's defaults and then runs the user's pre-refresh-pacman hooks.
# Works as root (`sudo bash`) or as a desktop user (sudo inside). Other
# installers carry copies, each pinned by its repository's test; a copy
# is only as safe as its last port, so carry a change to every one.
add_signed_repo() {
  local name="$1" release="$2" fingerprint="$3"
  local conf="/etc/pacman.d/$name.conf" include="Include = /etc/pacman.d/$name.conf"
  local sudo='' key user home hook_dir as_user
  (( EUID == 0 )) || sudo=sudo
  key="$(mktemp)"
  if ! curl -fsSL "$release/$name-signing-key.asc" -o "$key"; then
    rm -f "$key"
    echo "Could not download the package-signing key from $release." >&2
    return 1
  fi
  # Exactly one primary key, and it is the pinned one: a subkey with the
  # pinned fingerprint, or a second primary bundled alongside, is refused.
  if ! gpg --batch --with-colons --show-keys "$key" 2>/dev/null | awk -F: -v pin="$fingerprint" '
    $1 == "fpr" && after_pub { primary = $10 }
    { after_pub = 0 }
    $1 == "pub" { pubs++; after_pub = 1 }
    END { exit !(pubs == 1 && primary == pin) }
  '; then
    rm -f "$key"
    echo "The downloaded key does not match the pinned fingerprint $fingerprint; nothing was changed." >&2
    return 1
  fi
  $sudo pacman-key --add "$key"
  $sudo pacman-key --lsign-key "$fingerprint"
  rm -f "$key"
  printf '[%s]\nSigLevel = Required DatabaseRequired\nServer = %s\n' "$name" "$release" | $sudo tee "$conf" >/dev/null
  # ARM updates replace [omarchy] and everything below its header up to
  # the next section. Keep our Include before repository sections so it
  # cannot be mistaken for part of [omarchy], even on an existing install.
  restore_repo_include() {
    local include=$1 temporary sudo=''
    (( EUID == 0 )) || sudo=sudo
    temporary=$(mktemp)
    awk -v repo_include="$include" '
      $0 == repo_include { next }
      !added && /^[[:space:]]*\[/ && !/^[[:space:]]*\[options\]/ {
        print repo_include
        added = 1
      }
      { print }
      END { if (!added) print repo_include }
    ' /etc/pacman.conf > "$temporary"
    if ! cmp -s "$temporary" /etc/pacman.conf; then
      $sudo tee /etc/pacman.conf < "$temporary" >/dev/null
    fi
    rm -f "$temporary"
  }
  restore_repo_include "$include"
  user="${SUDO_USER:-${USER:-$(id -un)}}"
  home="$(getent passwd "$user" | cut -d: -f6)"
  if [[ -n $home && -d $home/.config/omarchy ]]; then
    # The home is the user's: write there as them, so a symlink in it cannot
    # turn root's write onto a system file.
    as_user=()
    if (( EUID == 0 )) && [[ $(id -u "$user") != 0 ]]; then
      as_user=(runuser -u "$user" --)
    fi
    hook_dir="$home/.config/omarchy/hooks/pre-refresh-pacman.d"
    "${as_user[@]}" mkdir -p "$hook_dir"
    # The user's shell reads the pipe it inherits (root's pipe cannot be
    # reopened by path once runuser drops privileges), and mv replaces the
    # hook file, never writing through a link there.
    {
      printf '%s\n' '#!/bin/bash' 'set -euo pipefail' \
        "# Restore the [$name] repository before Omarchy's ARM source rewrite."
      declare -f restore_repo_include
      printf 'restore_repo_include %q\n' "$include"
    } | "${as_user[@]}" bash -c 'umask 022; t=$(mktemp "$1.XXXXXX") && cat >"$t" && chmod 755 "$t" && mv -fT "$t" "$1"' _ "$hook_dir/$name"
  fi
  $sudo pacman -Sy
}
# --- end add_signed_repo ---

# Earlier releases of Notes came from a repository of its own,
# [icloud-notes], with its own /etc/pacman.d/<name>.conf, Include line and
# Omarchy hook. Its packages now come from [icloud-for-omarchy], so the old
# entries go: pacman would otherwise keep syncing them, and a repository
# listed first wins for a package in both.
remove_old_repos() {
  local sudo='' name conf include user home hook rest
  (( EUID == 0 )) || sudo=sudo
  user="${SUDO_USER:-${USER:-$(id -un)}}"
  home="$(getent passwd "$user" | cut -d: -f6)"
  for name in "${OLD_REPOS[@]}"; do
    conf="/etc/pacman.d/$name.conf"
    include="Include = $conf"
    hook=''
    [[ -n $home ]] && hook="$home/.config/omarchy/hooks/pre-refresh-pacman.d/$name"
    if [[ ! -e $conf && ! ( -n $hook && -e $hook ) ]] && ! grep -qxF "$include" /etc/pacman.conf; then
      continue
    fi
    echo "Removing the old [$name] repository"
    if grep -qxF "$include" /etc/pacman.conf; then
      # Rewritten in place (not sed -i), so the file keeps its owner and mode.
      rest="$(grep -vxF "$include" /etc/pacman.conf || true)"
      printf '%s\n' "$rest" | $sudo tee /etc/pacman.conf >/dev/null
    fi
    $sudo rm -f "$conf"
    if [[ -n $hook ]]; then
      # The hook is in the user's home: remove it as them under sudo.
      if (( EUID == 0 )) && [[ $(id -u "$user") != 0 ]]; then
        runuser -u "$user" -- rm -f "$hook"
      else
        rm -f "$hook"
      fi
    fi
  done
}

# Notes' background sync runs every 15 minutes while Notes is closed.
# The package enables its timer for every user from their next login;
# this starts it now in the desktop user's systemd, not root's.
# Usage: start_timer <timer> <what> <is on, ...>
start_timer() {
  local timer=$1 what=$2 on=$3 user uid
  local start_cmd="systemctl --user daemon-reload && systemctl --user start $timer"
  user="${SUDO_USER:-${USER:-$(id -un)}}"
  uid="$(id -u "$user" 2>/dev/null)" || return 0
  if (( uid == 0 )) || [[ ! -S /run/user/$uid/bus ]]; then
    echo "$what starts at your next login (or now: $start_cmd)."
    return 0
  fi
  local ctl=(env "XDG_RUNTIME_DIR=/run/user/$uid" "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$uid/bus" systemctl --user)
  (( EUID == 0 )) && ctl=(runuser -u "$user" -- "${ctl[@]}")
  if "${ctl[@]}" daemon-reload && "${ctl[@]}" start "$timer"; then
    echo "$what is on ($on)."
  else
    echo "Could not start $what now; it starts at your next login." >&2
  fi
}

# Everything runs from main, called on the last line, so a download cut
# short while piped into bash runs nothing at all.
main() {
  local pkg wanted=("$@")
  if ! command -v pacman >/dev/null; then
    echo "pacman not found: this installer is for Omarchy and other Arch Linux systems." >&2
    exit 1
  fi
  if [[ ! $SIGNING_KEY_FINGERPRINT =~ ^[0-9A-F]{40}$ ]]; then
    echo "This copy of install.sh has no signing key pinned; nothing was changed." >&2
    exit 1
  fi

  (( ${#wanted[@]} )) || wanted=("${DEFAULT_PACKAGES[@]}")
  for pkg in "${wanted[@]}"; do
    if [[ " ${PACKAGES[*]} " != *" $pkg "* ]]; then
      echo "Unknown package '$pkg'. Choose from: ${PACKAGES[*]}" >&2
      exit 64
    fi
  done

  # Keep the existing x86 repository name; ARM needs a separate database.
  case "$(uname -m)" in
    x86_64) ;;
    aarch64)
      OLD_REPOS+=("$REPO")
      REPO+=-aarch64
      ;;
    *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
  esac

  remove_old_repos

  echo "Adding the [$REPO] repository"
  add_signed_repo "$REPO" "$RELEASES" "$SIGNING_KEY_FINGERPRINT"

  echo "Installing ${wanted[*]}"
  if command -v omarchy-pkg-add >/dev/null; then
    # Omarchy's pacman hook aborts a direct `pacman -Syu` (system upgrades go
    # through `omarchy update`), so install the way Omarchy installs its own
    # apps; the next `omarchy update` brings everything current.
    omarchy-pkg-add "${wanted[@]}"
  else
    # Upgrade and install in one transaction. add_signed_repo has just synced
    # every repository's database, and installing from those without upgrading
    # is Arch's unsupported partial upgrade: a new dependency can need newer
    # libraries than the ones installed.
    if (( EUID == 0 )); then
      pacman -Syu --needed --noconfirm "${wanted[@]}"
    else
      sudo pacman -Syu --needed --noconfirm "${wanted[@]}"
    fi
  fi

  if [[ " ${wanted[*]} " == *" icloud-notes "* ]]; then
    start_timer icloud-notes-background.timer "Background sync" "every 15 minutes while Notes is closed"
  fi

  echo
  echo "Done."
  for pkg in "${wanted[@]}"; do
    case $pkg in
      icloud-notes) echo 'Launch "Notes" from the app launcher (Super + Space).' ;;
      icloud-photos) echo 'Launch "Photos" from the app launcher (Super + Space).' ;;
      icloud-findmy) echo 'Launch "Find My" from the app launcher (Super + Space).' ;;
      icloud-drive) echo 'Turn on iCloud Drive (it syncs ~/iCloud Drive, downloading your whole Drive first): icloud-drive start' ;;
    esac
  done
  cat <<EOT
If you have not signed in to iCloud yet, each app offers Apple's sign-in page
(or, from a terminal: icloud-session sign-in). One sign-in serves every app.
Updates arrive with the rest of the system through: omarchy update
EOT
}

main "$@"
