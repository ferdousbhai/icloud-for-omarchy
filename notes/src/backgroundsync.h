#ifndef BACKGROUNDSYNC_H
#define BACKGROUNDSYNC_H

class QTextStream;

// `icloud-notes --sync`: the app's sync (push, then pull) with no window,
// for the systemd user timer that keeps the vault current while Notes is
// closed. It goes through NotesBackend, so conflict handling, reporting a
// refused session to icloud-session and the one retry all apply. Skips
// (0) when there is no vault, nobody is signed in to icloud-session (or it
// is unknown), or the app or another sync holds the vault's lock. Returns
// 0 once both halves worked, non-zero when either failed. What it did is
// written to `out`. Needs a Q(Core|Gui)Application.
int runBackgroundSync(QTextStream &out);

// Adds the places npm, mise, bun, nvm and volta install icloud-md to PATH
// when it is not on it already (a systemd unit gets a bare PATH).
void findIcloudMd();

#endif
