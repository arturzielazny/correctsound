#!/bin/sh
# Installs CorrectSound from its latest release on GitHub: a ready-made binary for this
# computer, checked against its published checksum, into ~/.local/bin (or the directory in
# CORRECTSOUND_DIR). Run it again to update. Nothing else on the system is changed.
#
#   curl -fsSL https://arturzielazny.github.io/correctsound/install.sh | sh

set -eu

repo=arturzielazny/correctsound
dir=${CORRECTSOUND_DIR:-$HOME/.local/bin}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64 | Linux-amd64) target=x86_64-unknown-linux-gnu ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    *)
        echo "There is no ready-made CorrectSound for $(uname -s) on $(uname -m) yet." >&2
        echo "Install Rust (https://rustup.rs), then: cargo install --locked --git https://github.com/$repo" >&2
        exit 1
        ;;
esac

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q "$1" -O "$2"; }
else
    echo "Downloading needs curl or wget; please install one of them." >&2
    exit 1
fi

archive=correctsound-$target.tar.gz
url=https://github.com/$repo/releases/latest/download/$archive
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

echo "Downloading CorrectSound for $target..."
fetch "$url" "$tmp/$archive"
fetch "$url.sha256" "$tmp/$archive.sha256"

# The checksum file names the archive as it was built; compare the hashes alone.
expected=$(cut -d ' ' -f 1 "$tmp/$archive.sha256")
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$archive" | cut -d ' ' -f 1)
else
    actual=$(shasum -a 256 "$tmp/$archive" | cut -d ' ' -f 1)
fi
if [ "$expected" != "$actual" ]; then
    echo "The download does not match its checksum; nothing was installed. Please try again." >&2
    exit 1
fi

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$dir"
cp "$tmp/correctsound-$target/correctsound" "$dir/correctsound.new"
chmod 755 "$dir/correctsound.new"
mv "$dir/correctsound.new" "$dir/correctsound"

echo "Installed $("$dir/correctsound" --version) in $dir."
case ":$PATH:" in
    *":$dir:"*) ;;
    *)
        echo
        echo "$dir is not on your PATH yet. Add this line to your shell's startup file"
        echo "(~/.bashrc, ~/.zshrc), then open a new terminal:"
        echo "    export PATH=\"$dir:\$PATH\""
        ;;
esac
echo
echo "Try it without any hardware:  correctsound --simulate"
echo "In this terminal instead:     correctsound --simulate --tty"
