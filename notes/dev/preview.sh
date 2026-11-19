#!/usr/bin/env bash
set -Eeuo pipefail

# Pictures of the Notes window in a nested Hyprland with a private HOME, XDG directories and D-Bus, on a vault
# seeded here and the repository's stub engine, so nothing of the desktop it is run from is read or shown on: not its
# notes, its iCloud sign-in, its theme, or its windows.
#
#   notes/dev/preview.sh <out-dir> [theme-dir ...]
#
# Each scene is written as <out-dir>/<scene>-<theme>.png:
#   notes       the demo vault with Groceries open
#   search      the same, "pan" typed into the search box
#   readonly    Groceries as a note the engine will not push
#   conflict    Groceries changed in two places: the version picker
#   unreadable  Groceries with nested conflict markers the picker cannot read
#   attach      Groceries with a picture and a PDF attached
#   bare        an empty vault, not linked to iCloud: the banner and the onboarding dialog
# `theme-dir` holds a `colors.toml` (an Omarchy theme, such as /usr/share/omarchy/themes/catppuccin-latte) and is
# copied in as the scratch desktop's current theme; without one the pictures are named `-default`.
#
#   ICLOUD_NOTES_PREVIEW_APP     the window's binary (default: target/debug/icloud-notes-app)
#   ICLOUD_NOTES_PREVIEW_QT=1    that binary is the Qt app, which has no actions to drive: keys open the note
#   ICLOUD_NOTES_PREVIEW_SCENES  the scenes to take (default: all)
#   ICLOUD_NOTES_PREVIEW_SIZE    the size of the window and of the pictures (default 1100x700; narrower than 1062
#                                the window shows the tree, narrower than 521 one pane at a time)
#   TMPDIR                       where the scratch HOME and vault go (default /tmp)
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
app=${ICLOUD_NOTES_PREVIEW_APP:-$target/icloud-notes-app}
qt=${ICLOUD_NOTES_PREVIEW_QT:-}
[[ -x $app ]] || { echo "preview: build first (cargo build -p icloud-notes --bins), or name the app" >&2; exit 1; }
# The stub forwards vault-info to the real engine, which reads the seeded vault as the app would.
[[ -x $target/icloud-notes-sync ]] || { echo "preview: build first (cargo build -p icloud-notes-sync)" >&2; exit 1; }
for command_name in Hyprland hyprctl grim jq dbus-daemon gdbus setsid wtype magick; do
  command -v "$command_name" >/dev/null || { echo "preview: $command_name is required" >&2; exit 1; }
done
for theme in "${themes[@]}"; do
  [[ -z $theme || -f $theme/colors.toml ]] || { echo "preview: $theme has no colors.toml" >&2; exit 1; }
done
read -r -a scenes <<< "${ICLOUD_NOTES_PREVIEW_SCENES:-notes search readonly conflict unreadable attach bare}"
parent_runtime=${XDG_RUNTIME_DIR:?preview: XDG_RUNTIME_DIR is required}
parent_display=${WAYLAND_DISPLAY:?preview: run this from a Wayland desktop}
parent_socket=$parent_runtime/$parent_display
size=${ICLOUD_NOTES_PREVIEW_SIZE:-1100x700}
[[ $size =~ ^[0-9]+x[0-9]+$ ]] || { echo "preview: ICLOUD_NOTES_PREVIEW_SIZE is <width>x<height>" >&2; exit 2; }
width=${size%x*}

# `action:<name>[=<string>]` activates the window's action <name> over the scratch session bus, as its menu or a
# row would; `text:` types (an underscore is a space); the rest are keys, with modifiers named before them (ctrl+f).
open="action:open-note=Notes/Groceries.md"
if [[ -n $qt ]]; then
  # The Qt window starts with the arrows on its folder list (or tree): to Notes, into it, and its newest note.
  if (( width >= 1062 )); then open="Down Return Return"; else open="Down Right Right Return"; fi
fi
declare -A keys=(
  [notes]="$open"
  [search]="$open ctrl+f text:pan"
  [readonly]="$open"
  [conflict]="$open"
  [unreadable]="$open"
  [attach]="$open"
  [bare]=""
)
for scene in "${scenes[@]}"; do
  [[ -v keys[$scene] ]] || { echo "preview: no scene named $scene" >&2; exit 2; }
done

root=""
runtime=""
hypr_pid=""
app_pid=""
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
tmp=$(realpath -- "${TMPDIR:-/tmp}")
cleanup() {
  local status=$?
  trap - EXIT INT TERM
  stop "$app_pid" "$hypr_pid" "$dbus_pid"
  [[ -z $root || $root != "$tmp"/icloud-notes-preview.* ]] || rm -rf -- "$root"
  [[ -z $runtime || $runtime != /tmp/inp.* ]] || rm -rf -- "$runtime"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

root=$(mktemp -d "$tmp/icloud-notes-preview.XXXXXX")
# Wayland and Hyprland IPC use AF_UNIX paths capped at 108 bytes: the runtime directory is short, in /tmp.
runtime=$(mktemp -d /tmp/inp.XXXXXX)
chmod 700 "$root" "$runtime"

export HOME=$root/home
# The XDG directories where they are by default: Omarchy writes its current theme under ~/.local/state.
export XDG_CONFIG_HOME=$HOME/.config XDG_STATE_HOME=$HOME/.local/state XDG_DATA_HOME=$HOME/.local/share
export XDG_CACHE_HOME=$HOME/.cache
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
export XDG_RUNTIME_DIR=$runtime
export LC_ALL=C.UTF-8 TZ=UTC
unset GTK_THEME
vault=$root/vault
export ICLOUD_NOTES_VAULT=$vault
# The stub engine: nothing reaches iCloud. Its step finds nothing due, so the status line stays as the window starts.
export ICLOUD_NOTES_SYNC_BIN=$here/../tests/stubs/icloud-notes-sync ICLOUD_NOTES_SYNC_REAL=$target/icloud-notes-sync
export ICLOUD_NOTES_SYNC_STUB_STEP=idle

# A file of the vault, its mtime `days` back (the list's order and dates).
put() {
  local relative=$1 days=$2 text=$3
  mkdir -p "$(dirname -- "$vault/$relative")"
  printf '%s' "$text" > "$vault/$relative"
  touch -d "@$(( $(date +%s) - days * 86400 ))" "$vault/$relative"
}
# The vault one scene opens on: the engine's state (three tracked notes) and six notes in four folders.
seed() {
  local scene=$1 readonly_reason="" groceries
  rm -rf -- "$vault"
  mkdir -p "$vault"
  [[ $scene == bare ]] && return
  [[ $scene == readonly ]] &&
    readonly_reason=',"unpublishableReason":"is so large that Apple keeps its text in a separate file, which can'"'"'t be written back yet"'
  put .icloud-notes/state.json 9 '{"layoutVersion":4,"titleMode":"in-body","notes":{'\
'"a":{"file":"Notes/Groceries.md","recordChangeTag":"t","modificationDate":0'"$readonly_reason"'},'\
'"b":{"file":"Notes/Trip ideas.md","recordChangeTag":"t","modificationDate":0},'\
'"c":{"file":"Recipes/Pancakes.md","recordChangeTag":"t","modificationDate":0}}}'
  case $scene in
    unreadable)
      # Markers merged twice (nested), as a real vault once had them.
      groceries=$'---\napple-note-id: a\n---\n\n<<<<<<< local\n# Groceries\nmilk, eggs\n||||||| base\n# Groceries\n=======\n>>>>>>> remote\n\n<<<<<<< local\n<<<<<<< local\n- [ ] oat milk\n||||||| base\n- [ ] coffee\n=======\n- [ ] coffee\n- [ ] maple syrup\n>>>>>>> remote\n||||||| base\n- [ ] coffee\n=======\n>>>>>>> remote\n' ;;
    conflict)
      groceries=$'---\napple-note-id: a\n---\n# Groceries\nmilk, eggs, **sourdough** from [the bakery](https://example.com)\n\n## Weekend\n<<<<<<< local\n- [ ] oat milk\n- [x] coffee\n- [ ] blueberries\n||||||| base\n- [ ] oat milk\n- [ ] coffee\n=======\n- [ ] oat milk\n- [ ] coffee, the *dark* roast\n- [ ] maple syrup\n>>>>>>> remote\n\n> don\'t forget the bags\n' ;;
    *)
      groceries=$'---\napple-note-id: a\n---\n# Groceries\nmilk, eggs, **sourdough** from [the bakery](https://example.com)\n\n## Weekend\n- [ ] oat milk\n- [x] coffee\n- *maybe* `pancake mix`\n\n> don\'t forget the bags\n' ;;
  esac
  if [[ $scene == attach ]]; then
    mkdir -p "$vault/Notes/attachments"
    magick -size 400x300 gradient:skyblue-khaki "$vault/Notes/attachments/Basket.png"
    printf '%%PDF-1.4\n' > "$vault/Notes/attachments/receipt.pdf"
    groceries+=$'![Basket](attachments/Basket.png)\n[receipt.pdf](attachments/receipt.pdf)\n'
  fi
  put Notes/Groceries.md 0 "$groceries"
  put "Notes/Trip ideas.md" 2 $'---\napple-note-id: b\n---\n# Trip ideas\nKyoto in spring for the cherry blossoms.\n| day | plan |\n| 1 | arrive |\n'
  put Recipes/Pancakes.md 5 $'---\napple-note-id: c\n---\n# Pancakes\nflour, eggs, very hot pan\n'
  put Recipes/Desserts/Brownies.md 12 $'# Brownies\ndark chocolate, a pinch of salt\n'
  put "Work/Quarterly plan.md" 1 $'# Quarterly plan\nship the one-pane sidebar\n'
}

# A session bus with no service directories: nothing can be dbus-activated on it, icloud-session included, so the
# sign-in reads as unknown and no sign-in banner shows.
printf '%s\n' \
  '<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"' \
  ' "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">' \
  '<busconfig>' \
  '  <type>session</type>' \
  "  <listen>unix:abstract=icloud-notes-preview-$BASHPID</listen>" \
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
      local window=(--session --dest com.ferdousbhai.IcloudNotes --object-path /com/ferdousbhai/IcloudNotes/window/1)
      local name=${key#action:} parameter='[]'
      if [[ $name == *=* ]]; then
        parameter="[<'${name#*=}'>]"
        name=${name%%=*}
      fi
      gdbus call "${window[@]}" --method org.gtk.Actions.Activate "$name" "$parameter" '{}' >/dev/null
      sleep 1
    elif [[ $key == text:* ]]; then
      local text=${key#text:}
      wtype -d 30 "${text//_/ }"
    else
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
# One scene: its vault, the app from its start, the scene's keys, the picture, and the app stopped again.
shoot() {
  local scene=$1 picture=$2
  seed "$scene"
  if [[ -n $qt ]]; then
    QT_QPA_PLATFORM=wayland "$app" > "$root/app.log" 2>&1 &
  else
    GDK_BACKEND=wayland "$app" > "$root/app.log" 2>&1 &
  fi
  app_pid=$!
  for _ in {1..100}; do
    on_output && break
    kill -0 "$app_pid" 2>/dev/null || break
    sleep 0.1
  done
  # Once the window is drawn and its first sync step is done. The nested compositor's keyboard is the app's: it is
  # the only client there.
  sleep 2
  press "${keys[$scene]}"
  sleep 1.5
  kill -0 "$app_pid" 2>/dev/null || { echo "preview: the app exited" >&2; sed -n '1,40p' "$root/app.log" >&2; exit 1; }
  # A picture of an output the window is not on would be blank, and look like a result.
  on_output || { echo "preview: the app's window is not on the output being captured" >&2; hyprctl clients >&2; exit 1; }
  # Hyprland's own notices would cover the header, and a tooltip where the pointer happens to be would cover a row:
  # the pointer goes to the bottom left corner.
  hyprctl dismissnotify >/dev/null
  hyprctl monitors -j | jq -r 'first(.[] | select(.name == "preview") | "\(.x + 2) \(.y + .height - 2)")' |
    { read -r x y && hyprctl dispatch "hl.dsp.cursor.move({ x = $x, y = $y })" >/dev/null; }
  sleep 0.5
  /usr/bin/grim -o preview "$picture"
  echo "preview: $picture"
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
  for scene in "${scenes[@]}"; do
    shoot "$scene" "$out/$scene-$name.png"
  done
done
