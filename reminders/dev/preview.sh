#!/usr/bin/env bash
set -Eeuo pipefail

# Pictures of the Reminders window in a nested Hyprland with a private HOME, XDG directories and D-Bus, on the fake
# iCloud of examples/fake_reminders.rs, so nothing of the desktop it is run from is read or shown on: not its
# account, its reminders, its theme, or its windows.
#
#   reminders/dev/preview.sh <out-dir> [theme-dir ...]
#
# Each scene is written as <out-dir>/<scene>-<theme>.png, reached with the app's own keys:
#   lists      the window as it opens (in a narrow window: the sidebar)
#   new        after Ctrl+N (in a narrow window: the reminders)
#   completed  with Show Completed on
#   edit       the first reminder's dialog
#   invalid    the same with a due date that cannot be read
#   toast      a new reminder refused for its due date
#   back       Ctrl+N and then Escape (in a narrow window: the sidebar again)
#   banner     signed out: the cached reminders under the sign-in banner
#   empty      signed out with nothing cached: the banner over an empty list
#   left       after `icloud-reminders leave`: where the reminders are now
#   save       the first reminder renamed in its dialog, and saved with Alt+S
#   complete   the first reminder ticked
# The last two change the fake's reminders for the scenes after them; each theme starts on a fresh fake.
# `theme-dir` holds a `colors.toml` (an Omarchy theme, such as /usr/share/omarchy/themes/catppuccin-latte) and is
# copied in as the scratch desktop's current theme; without one the pictures are named `-default`.
#
#   ICLOUD_REMINDERS_PREVIEW_SCENES  the scenes to take (default: all)
#   ICLOUD_REMINDERS_PREVIEW_SIZE    the size of the window and of the pictures (default 900x640)
#   ICLOUD_REMINDERS_PREVIEW_SETTLE  seconds to wait before each picture (default 2)
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
for binary in icloud-reminders icloud-reminders-app examples/fake_reminders; do
  [[ -x $target/$binary ]] ||
    { echo "preview: build first (cargo build -p icloud-reminders --bins --example fake_reminders)" >&2; exit 1; }
done
for command_name in Hyprland hyprctl grim jq dbus-daemon setsid wtype curl; do
  command -v "$command_name" >/dev/null || { echo "preview: $command_name is required" >&2; exit 1; }
done
for theme in "${themes[@]}"; do
  [[ -z $theme || -f $theme/colors.toml ]] || { echo "preview: $theme has no colors.toml" >&2; exit 1; }
done
read -r -a scenes <<< "${ICLOUD_REMINDERS_PREVIEW_SCENES:-lists new completed edit invalid toast back banner empty left save complete}"
# Ctrl+N puts the keyboard in the line that adds a reminder. From there the header's Show Completed is one stop back,
# and the first reminder two on, past the due date. Its dialog opens with the keyboard in the title, one stop before
# the due date. A reminder's box is the stop after the reminder.
declare -A keys=(
  [lists]=""
  [new]="ctrl+n"
  [completed]="ctrl+n shift+ISO_Left_Tab space"
  [edit]="ctrl+n Tab Tab Return"
  [invalid]="ctrl+n Tab Tab Return Tab text:someday"
  [toast]="ctrl+n text:Tea Tab text:someday Return"
  [back]="ctrl+n Escape"
  [banner]=""
  [empty]=""
  [left]=""
  [save]="ctrl+n Tab Tab Return text:Oatmilk alt+s"
  [complete]="ctrl+n Tab Tab Tab space"
)
for scene in "${scenes[@]}"; do
  [[ -v keys[$scene] ]] || { echo "preview: no scene named $scene" >&2; exit 2; }
done
parent_runtime=${XDG_RUNTIME_DIR:?preview: XDG_RUNTIME_DIR is required}
parent_display=${WAYLAND_DISPLAY:?preview: run this from a Wayland desktop}
parent_socket=$parent_runtime/$parent_display
settle=${ICLOUD_REMINDERS_PREVIEW_SETTLE:-2}
size=${ICLOUD_REMINDERS_PREVIEW_SIZE:-900x640}
[[ $size =~ ^[0-9]+x[0-9]+$ ]] || { echo "preview: ICLOUD_REMINDERS_PREVIEW_SIZE is <width>x<height>" >&2; exit 2; }

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
  [[ -z $root || $root != /tmp/icloud-reminders-preview.* ]] || rm -rf -- "$root"
  [[ -z $runtime || $runtime != /tmp/irp.* ]] || rm -rf -- "$runtime"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Wayland and Hyprland IPC use AF_UNIX paths capped at 108 bytes.
root=$(mktemp -d /tmp/icloud-reminders-preview.XXXXXX)
runtime=$(mktemp -d /tmp/irp.XXXXXX)
chmod 700 "$root" "$runtime"

export HOME=$root/home
# The XDG directories where they are by default: Omarchy writes its current theme under ~/.local/state.
export XDG_CONFIG_HOME=$HOME/.config XDG_STATE_HOME=$HOME/.local/state XDG_DATA_HOME=$HOME/.local/share
export XDG_CACHE_HOME=$HOME/.cache
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
export XDG_RUNTIME_DIR=$runtime
export LC_ALL=C TZ=UTC
unset GTK_THEME

# A fake iCloud with its reminders as seeded, on a port of its own (it says which), and the cache of them the window
# opens with, so that the signed-out scene has reminders under its banner.
fake=""
start_fake() {
  stop "$fake_pid"
  "$target/examples/fake_reminders" 127.0.0.1:0 > "$root/fake.log" 2>&1 &
  fake_pid=$!
  fake=""
  for _ in {1..50}; do
    fake=$(sed -n 's/^Fake iCloud Reminders on //p' "$root/fake.log")
    [[ -z $fake ]] || break
    sleep 0.1
  done
  [[ -n $fake ]] || { echo "preview: the fake iCloud did not start" >&2; cat "$root/fake.log" >&2; exit 1; }
  export ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=$fake
  "$target/icloud-reminders" sync --full --quiet > /dev/null
}

# A session bus with no service directories: nothing can be dbus-activated on it.
printf '%s\n' \
  '<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"' \
  ' "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">' \
  '<busconfig>' \
  '  <type>session</type>' \
  "  <listen>unix:abstract=icloud-reminders-preview-$BASHPID</listen>" \
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
    if [[ $key == text:* ]]; then
      wtype "${key#text:}"
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
  local data=$XDG_DATA_HOME/icloud-reminders
  case $scene in
    banner) curl -fsS -X POST "$fake/fake/sign-out" > /dev/null ;;
    empty)
      curl -fsS -X POST "$fake/fake/sign-out" > /dev/null
      rm -f -- "$data/cache.json"
      ;;
    left) printf '{"version":1,"at":0,"folder":"%s/Documents/Reminders"}\n' "$HOME" > "$data/left-icloud.json" ;;
  esac
  GDK_BACKEND=wayland "$target/icloud-reminders-app" > "$root/app.log" 2>&1 &
  app_pid=$!
  for _ in {1..100}; do
    on_output && break
    kill -0 "$app_pid" 2>/dev/null || break
    sleep 0.1
  done
  # Once the window is drawn and its first sync is shown. The nested compositor's keyboard is the app's: it is the
  # only client there.
  sleep 1.5
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
    banner) curl -fsS -X POST "$fake/mock/reauthenticate" > /dev/null ;;
    empty)
      curl -fsS -X POST "$fake/mock/reauthenticate" > /dev/null
      "$target/icloud-reminders" sync --full --quiet > /dev/null
      ;;
    left) rm -f -- "$data/left-icloud.json" ;;
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
