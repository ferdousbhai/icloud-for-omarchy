#!/usr/bin/env bash
set -Eeuo pipefail

# Pictures of the Find My window in a nested Hyprland with a private HOME, XDG directories and D-Bus, on the fake
# Find My of examples/fake_findme.rs, so nothing of the desktop it is run from is read or shown on: not its account,
# its devices, its theme, or its windows. The map's tiles come from OpenStreetMap, as in the app.
#
#   findmy/dev/preview.sh <out-dir> [theme-dir ...]
#
# Each scene is written as <out-dir>/<scene>-<theme>.png:
#   devices   the window as it opens: the devices with their batteries, the first located one selected on the map
#             (in a narrow window: the sidebar over the map)
#   map       the sidebar put away with its button: the map alone
#   sound     Play Sound on the selected device: its toast
#   lost      Lost Mode on the selected device: the dialog, Turn On disabled with no phone number
#   filled    the same with a phone number typed: Turn On enabled
#   banner    signed out: the sign-in banner over an empty list
#   password  Find My wants the Apple password: its banner
# `theme-dir` holds a `colors.toml` (an Omarchy theme, such as /usr/share/omarchy/themes/catppuccin-latte) and is
# copied in as the scratch desktop's current theme; without one the pictures are named `-default`.
#
#   ICLOUD_FINDMY_PREVIEW_SCENES  the scenes to take (default: all)
#   ICLOUD_FINDMY_PREVIEW_SIZE    the size of the window and of the pictures (default 1100x720)
#   ICLOUD_FINDMY_PREVIEW_SETTLE  seconds to wait before each picture (default 3: the map loads its tiles)
if (( $# < 1 )); then
  echo "usage: $0 <out-dir> [theme-dir ...]" >&2
  exit 2
fi
out=$(realpath -m -- "$1")
shift
themes=("$@")
(( ${#themes[@]} )) || themes=("")

here=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
target=$(realpath -m -- "$here/../../target/debug")
for binary in icloud-findmy-app examples/fake_findme; do
  [[ -x $target/$binary ]] ||
    { echo "preview: build first (cargo build -p icloud-findmy --bins --example fake_findme)" >&2; exit 1; }
done
for command_name in Hyprland hyprctl grim jq dbus-daemon gdbus setsid wtype curl; do
  command -v "$command_name" >/dev/null || { echo "preview: $command_name is required" >&2; exit 1; }
done
for theme in "${themes[@]}"; do
  [[ -z $theme || -f $theme/colors.toml ]] || { echo "preview: $theme has no colors.toml" >&2; exit 1; }
done
read -r -a scenes <<< "${ICLOUD_FINDMY_PREVIEW_SCENES:-devices map sound lost filled banner password}"
# `action:<name>` activates the window's action <name> over the scratch session bus, as its menu would; the rest are
# keys. The window opens with the keyboard on the sidebar's button; the Lost Mode dialog opens
# with it in the phone number.
declare -A keys=(
  [devices]=""
  [map]="space"
  [sound]="action:play-sound"
  [lost]="action:lost-mode"
  [filled]="action:lost-mode text:+1_555_0100"
  [banner]=""
  [password]=""
)
for scene in "${scenes[@]}"; do
  [[ -v keys[$scene] ]] || { echo "preview: no scene named $scene" >&2; exit 2; }
done
parent_runtime=${XDG_RUNTIME_DIR:?preview: XDG_RUNTIME_DIR is required}
parent_display=${WAYLAND_DISPLAY:?preview: run this from a Wayland desktop}
parent_socket=$parent_runtime/$parent_display
settle=${ICLOUD_FINDMY_PREVIEW_SETTLE:-3}
size=${ICLOUD_FINDMY_PREVIEW_SIZE:-1100x720}
[[ $size =~ ^[0-9]+x[0-9]+$ ]] || { echo "preview: ICLOUD_FINDMY_PREVIEW_SIZE is <width>x<height>" >&2; exit 2; }

root=""
runtime=""
hypr_pid=""
app_pid=""
fake_pid=""
dbus_pid=""
# Only the PIDs this script started.
stop() {
  local pid
  for pid in "$@"; do
    [[ -z $pid ]] || kill -TERM "$pid" 2>/dev/null || true
  done
  for _ in {1..30}; do
    local alive=0
    for pid in "$@"; do
      [[ -z $pid ]] || ! kill -0 "$pid" 2>/dev/null || alive=1
    done
    (( alive == 0 )) && break
    sleep 0.1
  done
  for pid in "$@"; do
    [[ -z $pid ]] || kill -KILL "$pid" 2>/dev/null || true
  done
}
cleanup() {
  local status=$?
  trap - EXIT INT TERM
  stop "$app_pid" "$fake_pid" "$hypr_pid" "$dbus_pid"
  [[ -z $root || $root != /tmp/icloud-findmy-preview.* ]] || rm -rf -- "$root"
  [[ -z $runtime || $runtime != /tmp/ifp.* ]] || rm -rf -- "$runtime"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Wayland and Hyprland IPC use AF_UNIX paths capped at 108 bytes.
root=$(mktemp -d /tmp/icloud-findmy-preview.XXXXXX)
runtime=$(mktemp -d /tmp/ifp.XXXXXX)
chmod 700 "$root" "$runtime"

export HOME=$root/home
# The XDG directories where they are by default: Omarchy writes its current theme under ~/.local/state.
export XDG_CONFIG_HOME=$HOME/.config XDG_STATE_HOME=$HOME/.local/state XDG_DATA_HOME=$HOME/.local/share
export XDG_CACHE_HOME=$HOME/.cache
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
export XDG_RUNTIME_DIR=$runtime
export LC_ALL=C TZ=UTC
unset GTK_THEME

# A fake Find My with its devices as seeded, on a port of its own (it says which).
fake=""
start_fake() {
  stop "$fake_pid"
  "$target/examples/fake_findme" 127.0.0.1:0 > "$root/fake.log" 2>&1 &
  fake_pid=$!
  fake=""
  for _ in {1..50}; do
    fake=$(sed -n 's/^Fake Find My on //p' "$root/fake.log")
    [[ -z $fake ]] || break
    sleep 0.1
  done
  [[ -n $fake ]] || { echo "preview: the fake Find My did not start" >&2; cat "$root/fake.log" >&2; exit 1; }
  export ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=$fake
}

# A session bus with no service directories: nothing can be dbus-activated on it.
printf '%s\n' \
  '<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"' \
  ' "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">' \
  '<busconfig>' \
  '  <type>session</type>' \
  "  <listen>unix:abstract=icloud-findmy-preview-$BASHPID</listen>" \
  '  <keep_umask/>' \
  '  <policy context="default">' \
  '    <allow send_destination="*" eavesdrop="true"/>' \
  '    <allow eavesdrop="true"/>' \
  '    <allow own="*"/>' \
  '  </policy>' \
  '</busconfig>' > "$root/dbus.conf"
mapfile -t dbus_details < <(/usr/bin/dbus-daemon --config-file="$root/dbus.conf" --fork --print-address=1 --print-pid=1)
(( ${#dbus_details[@]} == 2 )) || { echo "preview: could not start the isolated session bus" >&2; exit 1; }
export DBUS_SESSION_BUS_ADDRESS=${dbus_details[0]}
dbus_pid=${dbus_details[1]}

printf '%s\n' \
  "hl.monitor({ output = \"\", mode = \"$size@60\", position = \"auto\", scale = 1 })" \
  'hl.config({' \
  '  general = { gaps_in = 0, gaps_out = 0, border_size = 0 },' \
  '  decoration = { rounding = 0, shadow = { enabled = false }, blur = { enabled = false } },' \
  '  animations = { enabled = false },' \
  '  misc = { disable_hyprland_logo = true, disable_splash_rendering = true },' \
  '})' \
  'hl.window_rule({ match = { class = ".*" }, monitor = "preview" })' > "$root/hyprland.lua"
unset HYPRLAND_INSTANCE_SIGNATURE
/usr/bin/setsid /usr/bin/env WAYLAND_DISPLAY="$parent_socket" /usr/bin/Hyprland --config "$root/hyprland.lua" > "$root/hyprland.log" 2>&1 &
hypr_pid=$!
signature=""
display=""
for _ in {1..120}; do
  kill -0 "$hypr_pid" 2>/dev/null || { echo "preview: nested Hyprland exited during startup" >&2; sed -n '1,80p' "$root/hyprland.log" >&2; exit 1; }
  for instance in "$XDG_RUNTIME_DIR"/hypr/*; do
    [[ -S $instance/.socket.sock && -f $instance/hyprland.lock ]] || continue
    candidate=$(sed -n '2p' "$instance/hyprland.lock")
    [[ -n $candidate && -S $XDG_RUNTIME_DIR/$candidate ]] || continue
    signature=$(basename "$instance")
    display=$candidate
    break 2
  done
  sleep 0.1
done
[[ -n $signature ]] || { echo "preview: nested Hyprland did not publish its sockets" >&2; exit 1; }
export HYPRLAND_INSTANCE_SIGNATURE=$signature WAYLAND_DISPLAY=$display

# The nested compositor's own output is a window on the desktop this runs from, as large as that desktop tiles it.
# The app goes on a headless output instead, whose size is the monitor rule's; the window rule sends it there.
hyprctl output create headless preview >/dev/null

on_output() {
  local output
  output=$(hyprctl monitors -j | jq 'first(.[] | select(.name == "preview") | .id)') &&
    hyprctl clients -j | jq -e --argjson output "${output:-null}" 'any(.[]; .mapped and .monitor == $output)' >/dev/null
}
press() {
  local key
  for key in $1; do
    if [[ $key == action:* ]]; then
      # The app's one window, as GtkApplication exports it on the session bus. A device action is enabled once the
      # first refresh has selected a device; activating it before then would do nothing.
      local window=(--session --dest com.ferdousbhai.IcloudFindMy --object-path /com/ferdousbhai/IcloudFindMy/window/1)
      for _ in {1..100}; do
        gdbus call "${window[@]}" --method org.gtk.Actions.Describe "${key#action:}" | grep -q '^((true,' && break
        sleep 0.1
      done
      gdbus call "${window[@]}" --method org.gtk.Actions.Activate "${key#action:}" '[]' '{}' >/dev/null
    elif [[ $key == text:* ]]; then
      # An underscore types a space: the keys are split on spaces.
      local text=${key#text:}
      wtype "${text//_/ }"
    else
      # The key, with the modifiers named before it held.
      local -a held=()
      while [[ $key == *+* ]]; do
        held+=(-M "${key%%+*}")
        key=${key#*+}
      done
      wtype "${held[@]}" -k "$key"
    fi
    sleep 0.4
  done
}
# One scene: the app from its start, the scene's keys, the picture, and the app stopped again.
shoot() {
  local scene=$1 picture=$2
  case $scene in
    banner) curl -fsS -X POST "$fake/fake/sign-out" > /dev/null ;;
    password) curl -fsS -X POST "$fake/fake/find-my-password" > /dev/null ;;
  esac
  GDK_BACKEND=wayland "$target/icloud-findmy-app" > "$root/app.log" 2>&1 &
  app_pid=$!
  for _ in {1..100}; do
    on_output && break
    kill -0 "$app_pid" 2>/dev/null || break
    sleep 0.1
  done
  # Once the window is drawn and its first refresh is shown. The nested compositor's keyboard is the app's: it is
  # the only client there.
  sleep 2
  press "${keys[$scene]}"
  sleep "$settle"
  kill -0 "$app_pid" 2>/dev/null || { echo "preview: the app exited" >&2; sed -n '1,40p' "$root/app.log" >&2; exit 1; }
  # A picture of an output the window is not on would be blank, and look like a result.
  on_output || { echo "preview: the app's window is not on the output being captured" >&2; hyprctl clients >&2; exit 1; }
  # Hyprland's own notices (it says it was started without its wrapper) would cover the header.
  hyprctl dismissnotify >/dev/null
  /usr/bin/grim -o preview "$picture"
  echo "preview: $picture"
  [[ ! -s $root/app.log ]] || sed -n '1,20p' "$root/app.log" >&2
  stop "$app_pid"
  app_pid=""
  case $scene in
    banner | password) curl -fsS -X POST "$fake/mock/reauthenticate" > /dev/null ;;
  esac
}

mkdir -p "$out"
current=$HOME/.local/state/omarchy/current
for theme in "${themes[@]}"; do
  name=default
  if [[ -n $theme ]]; then
    name=$(basename -- "$(realpath -- "$theme")")
    rm -rf -- "$current"
    mkdir -p "$current/theme"
    cp -- "$theme/colors.toml" "$current/theme/colors.toml"
    printf '%s\n' "$name" > "$current/theme.name"
  fi
  start_fake
  for scene in "${scenes[@]}"; do
    shoot "$scene" "$out/$scene-$name.png"
  done
done
