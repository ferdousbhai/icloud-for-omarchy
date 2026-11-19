#!/usr/bin/env bash
set -Eeuo pipefail

# Pictures of the Photos window in a nested Hyprland with a private HOME, XDG directories and D-Bus, on the fake
# CloudKit of examples/fake_cloudkit.rs, so nothing of the desktop it is run from is read or shown on: not its
# account, its photos, its theme, or its windows.
#
#   photos/dev/preview.sh <out-dir> [theme-dir ...]
#
# Each scene is written as <out-dir>/<scene>-<theme>.png:
#   grid     the window as it opens: the albums beside All Photos by month (in a narrow window: the albums)
#   photos   in a narrow window, All Photos in place of the albums, reached with the keyboard (else as grid)
#   viewer   the newest photo opened
#   prefs    the preferences
#   about    About Photos
#   upload   a file being uploaded: the dialog that cannot be closed until it is done
#   delete   Delete on the photo in the viewer: the question
#   deleted  the same answered: the next photo, and the toast with Undo
#   banner   signed out: the photos already synced under the sign-in banner
# `theme-dir` holds a `colors.toml` (an Omarchy theme, such as /usr/share/omarchy/themes/catppuccin-latte) and is
# copied in as the scratch desktop's current theme; without one the pictures are named `-default`.
#
#   ICLOUD_PHOTOS_PREVIEW_SCENES  the scenes to take (default: all)
#   ICLOUD_PHOTOS_PREVIEW_SIZE    the size of the window and of the pictures (default 1100x760; narrower than 640,
#                                 the window shows one pane)
#   ICLOUD_PHOTOS_PREVIEW_SETTLE  seconds to wait before each picture (default 3: thumbnails arrive)
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
for binary in icloud-photos-app examples/fake_cloudkit; do
  [[ -x $target/$binary ]] ||
    { echo "preview: build first (cargo build -p icloud-photos --bins --example fake_cloudkit)" >&2; exit 1; }
done
for command_name in Hyprland hyprctl grim jq dbus-daemon gdbus setsid wtype magick; do
  command -v "$command_name" >/dev/null || { echo "preview: $command_name is required" >&2; exit 1; }
done
for theme in "${themes[@]}"; do
  [[ -z $theme || -f $theme/colors.toml ]] || { echo "preview: $theme has no colors.toml" >&2; exit 1; }
done
read -r -a scenes <<< "${ICLOUD_PHOTOS_PREVIEW_SCENES:-grid photos viewer prefs about upload delete deleted banner}"
# The newest of the fake's photos, a JPEG.
newest=000A55E7-FAKE-4000-8000-000000000000
parent_runtime=${XDG_RUNTIME_DIR:?preview: XDG_RUNTIME_DIR is required}
parent_display=${WAYLAND_DISPLAY:?preview: run this from a Wayland desktop}
parent_socket=$parent_runtime/$parent_display
settle=${ICLOUD_PHOTOS_PREVIEW_SETTLE:-3}
size=${ICLOUD_PHOTOS_PREVIEW_SIZE:-1100x760}
[[ $size =~ ^[0-9]+x[0-9]+$ ]] || { echo "preview: ICLOUD_PHOTOS_PREVIEW_SIZE is <width>x<height>" >&2; exit 2; }
# `action:<name>[=<string>]` activates the window's action <name> over the scratch session bus, as its menu or a tile
# would; `text:` types (an underscore is a space); the rest are keys. The file chooser takes a path typed into it.
# A narrow window opens on the albums. The keyboard's first stop is the first of them, All Photos, which opens.
photos=""
(( ${size%x*} >= 640 )) || photos="Tab"
declare -A keys=(
  [grid]=""
  [photos]="$photos"
  [viewer]="$photos action:open-asset=$newest"
  [prefs]="action:preferences"
  [about]="action:about"
  [upload]="action:upload text:UPLOAD Return"
  [delete]="$photos action:open-asset=$newest action:viewer-delete"
  [deleted]="$photos action:open-asset=$newest action:viewer-delete Tab Return"
  [banner]=""
)
for scene in "${scenes[@]}"; do
  [[ -v keys[$scene] ]] || { echo "preview: no scene named $scene" >&2; exit 2; }
done

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
  [[ -z $root || $root != /tmp/icloud-photos-preview.* ]] || rm -rf -- "$root"
  [[ -z $runtime || $runtime != /tmp/ipp.* ]] || rm -rf -- "$runtime"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Wayland and Hyprland IPC use AF_UNIX paths capped at 108 bytes.
root=$(mktemp -d /tmp/icloud-photos-preview.XXXXXX)
runtime=$(mktemp -d /tmp/ipp.XXXXXX)
chmod 700 "$root" "$runtime"

export HOME=$root/home
# The XDG directories where they are by default: Omarchy writes its current theme under ~/.local/state.
export XDG_CONFIG_HOME=$HOME/.config XDG_STATE_HOME=$HOME/.local/state XDG_DATA_HOME=$HOME/.local/share
export XDG_CACHE_HOME=$HOME/.cache
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
export XDG_RUNTIME_DIR=$runtime
export LC_ALL=C TZ=UTC
unset GTK_THEME

# What the upload scene sends.
upload=$root/Beach.jpg
magick -size 640x480 gradient:skyblue-khaki "$upload"
keys[upload]=${keys[upload]/UPLOAD/$upload}

# A fake CloudKit with its photos as seeded, on a port of its own (it says which). Each upload takes a minute, so
# the scene finds it at work. The app's catalog and caches go with the last fake, whose sync token they hold, unless
# this one is signed out and answers nothing.
fake=""
start_fake() {
  stop "$fake_pid"
  [[ ${1-} == --signed-out ]] || rm -rf -- "$XDG_DATA_HOME/icloud-photos" "$XDG_CACHE_HOME/icloud-photos"
  "$target/examples/fake_cloudkit" --port 0 --hold-uploads 60 "$@" > "$root/fake.log" 2>&1 &
  fake_pid=$!
  fake=""
  for _ in {1..50}; do
    fake=$(sed -n 's/^Fake CloudKit with [0-9]* assets at //p' "$root/fake.log")
    [[ -z $fake ]] || break
    sleep 0.1
  done
  [[ -n $fake ]] || { echo "preview: the fake CloudKit did not start" >&2; cat "$root/fake.log" >&2; exit 1; }
  export ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=$fake
}

# A session bus with no service directories: nothing can be dbus-activated on it.
printf '%s\n' \
  '<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"' \
  ' "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">' \
  '<busconfig>' \
  '  <type>session</type>' \
  "  <listen>unix:abstract=icloud-photos-preview-$BASHPID</listen>" \
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
      # The app's one window, as GtkApplication exports it on the session bus.
      local window=(--session --dest com.ferdousbhai.IcloudPhotos --object-path /com/ferdousbhai/IcloudPhotos/window/1)
      local name=${key#action:} parameter='[]'
      if [[ $name == *=* ]]; then
        parameter="[<'${name#*=}'>]"
        name=${name%%=*}
      fi
      gdbus call "${window[@]}" --method org.gtk.Actions.Activate "$name" "$parameter" '{}' >/dev/null
      # What it opens (a dialog, the file chooser) is drawn before the next key.
      sleep 2
    elif [[ $key == text:* ]]; then
      local text=${key#text:}
      # A key at a time: the file chooser's completion drops keys that come together.
      wtype -d 30 "${text//_/ }"
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
  GDK_BACKEND=wayland "$target/icloud-photos-app" > "$root/app.log" 2>&1 &
  app_pid=$!
  for _ in {1..100}; do
    on_output && break
    kill -0 "$app_pid" 2>/dev/null || break
    sleep 0.1
  done
  # Once the window is drawn and its first sync is shown. The nested compositor's keyboard is the app's: it is the
  # only client there.
  sleep 3
  press "${keys[$scene]}"
  sleep "$settle"
  kill -0 "$app_pid" 2>/dev/null || { echo "preview: the app exited" >&2; sed -n '1,40p' "$root/app.log" >&2; exit 1; }
  # A picture of an output the window is not on would be blank, and look like a result.
  on_output || { echo "preview: the app's window is not on the output being captured" >&2; hyprctl clients >&2; exit 1; }
  # Hyprland's own notices (it says it was started without its wrapper) would cover the header, and a tooltip
  # where the pointer happens to be would cover a photo: the pointer goes to the bottom left corner.
  hyprctl dismissnotify >/dev/null
  hyprctl monitors -j | jq -r 'first(.[] | select(.name == "preview") | "\(.x + 2) \(.y + .height - 2)")' |
    { read -r x y && hyprctl dispatch "hl.dsp.cursor.move({ x = $x, y = $y })" >/dev/null; }
  sleep 0.5
  /usr/bin/grim -o preview "$picture"
  echo "preview: $picture"
  [[ ! -s $root/app.log ]] || sed -n '1,20p' "$root/app.log" >&2
  stop "$app_pid"
  app_pid=""
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
    # Signed out, over what the scenes before synced: the fake answers 421 until the banner's button is pressed.
    # An upload or a delete changes the fake's photos for the scenes after it; a fresh fake puts them back.
    case $scene in
      banner) start_fake --signed-out ;;
      upload | delete | deleted) start_fake ;;
    esac
    shoot "$scene" "$out/$scene-$name.png"
  done
done
