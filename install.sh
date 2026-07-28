#!/bin/sh
set -eu

repository="https://github.com/MildyNora/NetworkSandbox"
release_base="$repository/releases/latest/download"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)
    archive="netsandbox-macos-arm64.tar.gz"
    ;;
  Linux-x86_64 | Linux-amd64)
    archive="netsandbox-linux-x86_64.tar.gz"
    ;;
  *)
    printf 'Network Sandbox does not yet publish a binary for %s/%s.\n' \
      "$(uname -s)" "$(uname -m)" >&2
    exit 1
    ;;
esac

download() {
  source_url=$1
  destination=$2
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$source_url" -o "$destination"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$source_url" -O "$destination"
  else
    printf 'Network Sandbox installation requires curl or wget.\n' >&2
    exit 1
  fi
}

temporary_directory=$(mktemp -d)
trap 'find "$temporary_directory" -depth -delete 2>/dev/null || true' EXIT HUP INT TERM

download "$release_base/$archive" "$temporary_directory/$archive"
download "$release_base/SHA256SUMS" "$temporary_directory/SHA256SUMS"

expected=$(awk -v name="$archive" '$2 == name { print $1 }' "$temporary_directory/SHA256SUMS")
if [ -z "$expected" ]; then
  printf 'No checksum was published for %s.\n' "$archive" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$temporary_directory/$archive" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$temporary_directory/$archive" | awk '{ print $1 }')
else
  printf 'Network Sandbox installation requires sha256sum or shasum.\n' >&2
  exit 1
fi

if [ "$actual" != "$expected" ]; then
  printf 'Checksum verification failed for %s.\n' "$archive" >&2
  exit 1
fi

mkdir -p "$temporary_directory/unpacked"
tar -xzf "$temporary_directory/$archive" -C "$temporary_directory/unpacked"

binary_directory=${NETSANDBOX_BIN_DIR:-"$HOME/.local/bin"}
share_directory=${NETSANDBOX_SHARE_DIR:-"$HOME/.local/share/network-sandbox"}
installed_skill="$share_directory/skills/network-sandbox"

mkdir -p "$binary_directory" "$share_directory/skills"
install -m 755 "$temporary_directory/unpacked/netsandbox" "$binary_directory/netsandbox"

if [ -e "$installed_skill" ] || [ -L "$installed_skill" ]; then
  find "$installed_skill" -depth -delete
fi
cp -R "$temporary_directory/unpacked/skills/network-sandbox" "$installed_skill"

link_skill() {
  skill_directory=$1
  skill_link="$skill_directory/network-sandbox"
  mkdir -p "$skill_directory"
  if [ -L "$skill_link" ]; then
    rm "$skill_link"
  elif [ -e "$skill_link" ]; then
    printf 'Kept user-managed skill at %s\n' "$skill_link"
    return
  fi
  ln -s "$installed_skill" "$skill_link"
}

link_skill "${CODEX_SKILLS_DIR:-"$HOME/.codex/skills"}"
link_skill "${AGENT_SKILLS_DIR:-"$HOME/.agents/skills"}"

printf 'Installed Network Sandbox %s\n' "$("$binary_directory/netsandbox" --version | awk '{ print $2 }')"
printf 'CLI:   %s\n' "$binary_directory/netsandbox"
printf 'Skill: %s\n' "$installed_skill"

case ":$PATH:" in
  *":$binary_directory:"*) ;;
  *) printf 'Add %s to PATH before opening a new agent session.\n' "$binary_directory" ;;
esac
