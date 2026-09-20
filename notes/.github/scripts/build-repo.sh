#!/bin/bash
# Build the package from this checkout, sign it, and write a one-package
# pacman repository into dist/ for the release workflow to publish.
# Needs PACKAGING_GPG_KEY (an ASCII-armored secret key) in the environment.
set -euo pipefail
cd "$(dirname "$0")/../.."

GNUPGHOME=$(mktemp -d)
export GNUPGHOME
gpg --batch --quiet --import <<<"$PACKAGING_GPG_KEY"
fingerprint=$(gpg --batch --list-secret-keys --with-colons | awk -F: '/^fpr/ { print $10; exit }')

# install.sh pins the fingerprint users trust; a mismatch means the secret
# and the installer disagree, and nothing installed this way would verify.
pinned=$(sed -n 's/^SIGNING_KEY_FINGERPRINT=//p' install.sh)
if [[ $pinned != "$fingerprint" ]]; then
  echo "install.sh pins $pinned but PACKAGING_GPG_KEY is $fingerprint" >&2
  exit 1
fi
export GPGKEY=$fingerprint

rm -rf dist
mkdir dist
(cd pkgbuild && PKGDEST="$PWD/../dist" makepkg --force --sign)

cd dist
repo-add --sign --verify icloud-notes.db.tar.gz ./*.pkg.tar.zst
# repo-add leaves the names pacman asks for (icloud-notes.db, .files and
# their .sig) as symlinks, which a GitHub release cannot hold: copy them.
for name in db files; do
  rm -f "icloud-notes.$name" "icloud-notes.$name.sig"
  cp "icloud-notes.$name.tar.gz" "icloud-notes.$name"
  cp "icloud-notes.$name.tar.gz.sig" "icloud-notes.$name.sig"
done
gpg --batch --armor --export "$fingerprint" >icloud-notes-signing-key.asc
ls -l
