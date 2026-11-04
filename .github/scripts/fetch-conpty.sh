#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Puts Microsoft's ConPTY (conpty.dll and OpenConsole.exe, MIT) and its
# licence into each directory given (D-101, D-107). Programs' pictures in
# panes pass through this ConPTY; the system's drops them. The package is
# pinned by version and SHA-512 (as NuGet's catalog publishes it).
set -euo pipefail
VERSION=1.24.261001001
SHA512=Jj58t5UkyZtIk/6vqcl3RPttMQSjhHJDrOowbCl27on5jQvdoHdav+lKWwFSY7+tVrsVZfyUQsT1CpTgK+1U8g==
URL="https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$VERSION/microsoft.windows.console.conpty.$VERSION.nupkg"

[ $# -gt 0 ] || { echo "usage: $0 DIR..." >&2; exit 2; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL --retry 3 -o "$work/conpty.nupkg" "$URL"
python3 -I - "$work/conpty.nupkg" "$SHA512" "$work" <<'PY'
import base64, hashlib, sys, zipfile
pkg, want, out = sys.argv[1:4]
data = open(pkg, "rb").read()
got = base64.b64encode(hashlib.sha512(data).digest()).decode()
if got != want:
    sys.exit(f"ConPTY package checksum mismatch: {got}")
z = zipfile.ZipFile(pkg)
for name, dest in [
    ("runtimes/win-x64/native/conpty.dll", "conpty.dll"),
    ("build/native/runtimes/x64/OpenConsole.exe", "OpenConsole.exe"),
]:
    with open(f"{out}/{dest}", "wb") as f:
        f.write(z.read(name))
PY
for dir in "$@"; do
	mkdir -p "$dir"
	cp "$work/conpty.dll" "$work/OpenConsole.exe" "$dir/"
	cat > "$dir/THIRD-PARTY-CONPTY.txt" <<NOTICE
conpty.dll and OpenConsole.exe are Microsoft's ConPTY, from the NuGet package
Microsoft.Windows.Console.ConPTY $VERSION (SHA-512 $SHA512),
https://github.com/microsoft/terminal. TermOS uses them for the terminals in
its windows, so pictures that programs print there reach TermOS.

MIT License

Copyright (c) Microsoft Corporation. All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
NOTICE
done
echo "ConPTY $VERSION in: $*"
