#!/usr/bin/env bash
set -euo pipefail

# Mirror the resilient Ubuntu setup used by the primary release graph, while
# keeping this lane focused on the Tauri DEB + strict Snap verifier only.
if [[ -f /etc/apt/apt-mirrors.txt ]]; then
  sudo sed -i '\|^http://azure\.archive\.ubuntu\.com/ubuntu/|d' /etc/apt/apt-mirrors.txt
  if grep -q '^http://azure\.archive\.ubuntu\.com/ubuntu/' /etc/apt/apt-mirrors.txt; then
    echo 'Azure apt mirror remained enabled after sanitization' >&2
    exit 1
  fi
fi

apt_options=(
  -o Acquire::Retries=3
  -o Acquire::http::Timeout=20
  -o Acquire::https::Timeout=20
  -o Acquire::Languages=none
  -o Dpkg::Use-Pty=0
)
sudo apt-get "${apt_options[@]}" update
sudo DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}" install -y \
  appstream file libayatana-appindicator3-dev librsvg2-dev libwebkit2gtk-4.1-dev \
  patchelf squashfs-tools xdg-utils

# Snapd verifies the Store's signed assertions. Store revisions are per CPU
# architecture: pin the reviewed architecture-specific revision, not merely a
# moving channel, so a builder upgrade is an explicit source change.
case "$(dpkg --print-architecture)" in
  amd64) snapcraft_revision=18514 ;;
  arm64) snapcraft_revision=18519 ;;
  *)
    echo "unsupported native Snapcraft builder architecture: $(dpkg --print-architecture)" >&2
    exit 1
    ;;
esac
if snap list snapcraft >/dev/null 2>&1; then
  sudo snap refresh snapcraft --revision="$snapcraft_revision"
else
  sudo snap install snapcraft --classic --revision="$snapcraft_revision"
fi
if [[ "$(snapcraft --version)" != "snapcraft 9.0.1" ]]; then
  echo "unexpected Snapcraft version: $(snapcraft --version)" >&2
  exit 1
fi

# Snapcraft treats every content plug with a default-provider as a build Snap
# too, even when the package only consumes it at runtime. Preload the exact
# signed Store providers here so the subsequent non-root destructive pack can
# inspect them without trying (and failing) to invoke snapd itself. They are
# not copied into our artifact; the final Snap still consumes them through its
# declared content interfaces at runtime.
for provider in mesa-2404 gtk-common-themes gnome-46-2404; do
  if ! snap list "$provider" >/dev/null 2>&1; then
    sudo snap install "$provider"
  fi
done
