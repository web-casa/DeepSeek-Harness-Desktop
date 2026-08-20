#!/usr/bin/env bash
set -euo pipefail

# Keep the known-flaky Azure apt mirror out when the runner image exposes the
# same ordered mirror file as quality.yml. Older Ubuntu images use sources.list
# directly, so absence is expected and not an error.
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
  appstream binutils cpio file flatpak libayatana-appindicator3-dev \
  librsvg2-dev libwebkit2gtk-4.1-dev ostree patchelf rpm xdg-utils

flatpak remote-add --user --if-not-exists \
  flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user --noninteractive --no-related flathub \
  org.gnome.Platform//49 org.gnome.Sdk//49
