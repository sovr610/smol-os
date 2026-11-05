#!/bin/sh
# Fills packaging/*/*.in with a release's version and checksums (D-126).
# Usage: render-packaging.sh VERSION SHA256SUMS OUT_DIR
# Writes OUT_DIR/homebrew/termos.rb, OUT_DIR/scoop/termos.json,
# OUT_DIR/winget/*.yaml and OUT_DIR/aur/PKGBUILD. Fails if a checksum the
# packages need is not in SHA256SUMS.
set -eu
V=$1
SUMS=$2
OUT=$3
ROOT=$(cd "$(dirname "$0")/../.." && pwd)

sum() {
	s=$(awk -v f="$1" '$2 == f || $2 == "*" f { print $1 }' "$SUMS")
	if [ -z "$s" ]; then
		echo "render-packaging: $1 is not in $SUMS" >&2
		exit 1
	fi
	echo "$s"
}

MACOS_ARM64=$(sum "termos-$V-macos-arm64.tar.gz")
MACOS_X86_64=$(sum "termos-$V-macos-x86_64.tar.gz")
LINUX_X86_64=$(sum "termos-$V-linux-x86_64.tar.gz")
LINUX_AARCH64=$(sum "termos-$V-linux-aarch64.tar.gz")
WINDOWS_X86_64=$(sum "termos-$V-windows-x86_64.zip")
WINDOWS_UPPER=$(echo "$WINDOWS_X86_64" | tr 'a-f' 'A-F')

fill() {
	mkdir -p "$(dirname "$2")"
	sed -e "s/@VERSION@/$V/g" \
		-e "s/@SHA_MACOS_ARM64@/$MACOS_ARM64/g" \
		-e "s/@SHA_MACOS_X86_64@/$MACOS_X86_64/g" \
		-e "s/@SHA_LINUX_X86_64@/$LINUX_X86_64/g" \
		-e "s/@SHA_LINUX_AARCH64@/$LINUX_AARCH64/g" \
		-e "s/@SHA_WINDOWS_X86_64_UPPER@/$WINDOWS_UPPER/g" \
		-e "s/@SHA_WINDOWS_X86_64@/$WINDOWS_X86_64/g" \
		"$1" >"$2"
	if grep -q '@[A-Z_0-9]*@' "$2"; then
		echo "render-packaging: $2 still has a placeholder" >&2
		exit 1
	fi
}

P="$ROOT/packaging"
fill "$P/homebrew/termos.rb.in" "$OUT/homebrew/termos.rb"
fill "$P/scoop/termos.json.in" "$OUT/scoop/termos.json"
for f in "$P"/winget/*.yaml.in; do
	b=$(basename "$f" .in)
	fill "$f" "$OUT/winget/$b"
done
fill "$P/aur/PKGBUILD.in" "$OUT/aur/PKGBUILD"
echo "render-packaging: TermOS $V's packages are in $OUT"
