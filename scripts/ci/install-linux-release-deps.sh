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
  appstream binutils file flatpak libarchive-tools libayatana-appindicator3-dev \
  librsvg2-dev libwebkit2gtk-4.1-dev ostree patchelf rpm xdg-utils

flatpak remote-add --user --if-not-exists \
  flathub https://dl.flathub.org/repo/flathub.flatpakrepo

# Flathub authenticates OSTree data with its repository key, but //49 is a
# mutable branch. Pin the reviewed commit per architecture so a runtime move
# blocks release builds until its new contents are reviewed deliberately.
case "$(uname -m)" in
  x86_64)
    flatpak_arch=x86_64
    platform_commit=e51263e53d04900556e2f97ac2b27201f632f604d68d67f50323f9a99389fdb0
    sdk_commit=1df084a5492fbcd01f4d9af1084ade350919de069bbc9974d6dfdb04f47ab60f
    ;;
  aarch64|arm64)
    flatpak_arch=aarch64
    platform_commit=e6a556120286f34a4befd09f8f12096be0475ad170cec89355c48b20144c15d3
    sdk_commit=f98e82bd3cc2ec167c8502b218a4fa4a8e3a0f0c5fa4977b3404c6963b3debfa
    ;;
  *)
    echo "Unsupported Flatpak build architecture: $(uname -m)" >&2
    exit 1
    ;;
esac
flatpak install --user --noninteractive --no-related flathub \
  org.gnome.Platform//49 org.gnome.Sdk//49

actual_platform=$(flatpak info --user --arch="$flatpak_arch" --show-commit org.gnome.Platform//49)
actual_sdk=$(flatpak info --user --arch="$flatpak_arch" --show-commit org.gnome.Sdk//49)
if [[ "$actual_platform" != "$platform_commit" ]]; then
  echo "org.gnome.Platform//49 moved: $actual_platform != reviewed $platform_commit" >&2
  exit 1
fi
if [[ "$actual_sdk" != "$sdk_commit" ]]; then
  echo "org.gnome.Sdk//49 moved: $actual_sdk != reviewed $sdk_commit" >&2
  exit 1
fi
