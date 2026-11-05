# Releasing TermOS

A release is a `v<version>` tag. The `release` workflow (D-077) builds
everything, checks it against the tag, and leaves a **draft** release on
GitHub for you to read through and publish.

## What a release carries

| File | What it is |
| --- | --- |
| `termos-<v>-linux-x86_64.tar.gz`, `…-linux-aarch64.tar.gz` | TermOS for a terminal on Linux (glibc 2.31 or newer) |
| `termos-<v>-linux-x86_64-musl.tar.gz`, `…-aarch64-musl.tar.gz` | The same, static: any Linux (no NVIDIA readings, D-079) |
| `termos-<v>-macos-arm64.tar.gz`, `…-macos-x86_64.tar.gz` | macOS 11 or newer |
| `termos-<v>-windows-x86_64.zip` | Windows 10 1809 or newer |
| `termos_<v>_amd64.deb`, `…_arm64.deb` | Debian and Ubuntu packages (glibc 2.31 or newer), with completions and the man page |
| `termos-<v>-1.x86_64.rpm`, `…aarch64.rpm` | Fedora packages, the same |
| `termos-<v>-x86_64.iso` | The live ISO: try TermOS, then `termos install` |
| `termos-<v>-<board>.img.zst` | Disk images: x86_64, aarch64-virt, rpi5, rpi4, rpi3, rpi-zero2w |
| `termos-<board>-<v>.raucb` | Update bundles, which `termos update` fetches |
| `termos-<v>-<board>-legal-info.tar.xz` | Buildroot's license report for each image |
| `SHA256SUMS` | Checksums of all of the above |

Every file also has a build attestation (Sigstore, through GitHub), so
anyone can check where it was built:

```sh
gh attestation verify termos-0.2.0-x86_64.iso --repo sovr610/smol-os
```

The programs are not code-signed for macOS or Windows yet
(D-077); docs/getting-started.md says how to open them anyway.

## The signing keys (once)

Update bundles are signed with your key, and images only install bundles
whose signature leads back to your CA (D-078). Make a CA and a signing
certificate for code signing, and keep `ca.key` somewhere safe and offline:
with it you can make a new signing certificate later.

```sh
openssl req -x509 -newkey rsa:4096 -nodes -days 7300 \
  -subj "/O=TermOS/CN=TermOS update CA" \
  -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign \
  -keyout ca.key -out ca.pem
openssl req -newkey rsa:4096 -nodes -subj "/O=TermOS/CN=TermOS updates" \
  -keyout signing.key -out signing.csr
cat > signing.ext <<'EOF'
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid, issuer
EOF
openssl x509 -req -in signing.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
  -days 3650 -extfile signing.ext -out signing.pem
```

Then, in the repository's settings (Secrets and variables → Actions), add
three secrets with the files' contents:

| Secret | File |
| --- | --- |
| `TERMOS_RAUC_CA` | `ca.pem` |
| `TERMOS_RAUC_CERT` | `signing.pem` |
| `TERMOS_RAUC_KEY` | `signing.key` |

Without them the release stops before building any image: a release must
not carry images that trust a throwaway key.

To build signed images by hand, point `TERMOS_RAUC_KEYRING`,
`TERMOS_RAUC_CERT` and `TERMOS_RAUC_KEY` at the three files before
`cargo xtask image build`. Without them, `cargo xtask image` makes
development keys in `target/image/keys/`; images built with those accept
only bundles made with them.

## Rehearsing

The workflow can also be run by hand (Actions → Release → Run workflow),
and runs by itself when a push changes it or a patched crate
(`third_party/`) on any branch. It builds and packages the same files,
named `…-<version>-rehearsal…`, and keeps them as the run's artifact for a
week instead of making a release; nothing is attested. The images (about an hour and a half) are
built only when the run asks for them, and without the signing secrets
they trust development keys.

## Package channels

The `.deb` and `.rpm` are made by the `release` workflow itself, and each
is installed on a clean Ubuntu, Debian or Fedora (x86_64 and arm64) before
the draft is made. The other channels follow once you **publish** the
release: the `packages` workflow fills in each manifest from the release's
`SHA256SUMS`, installs it on a clean runner, and publishes it if that
channel's secret is set (Secrets and variables → Actions). Without the
secret the job still tests, says so in a notice, and stops.

| Channel | Secret | What it needs |
| --- | --- | --- |
| Homebrew | `HOMEBREW_TAP_TOKEN` | A repository `sovr610/homebrew-termos` and a fine-grained token that may write its contents |
| Scoop | `SCOOP_BUCKET_TOKEN` | A repository `sovr610/scoop-termos` and a token that may write its contents |
| winget | `WINGET_TOKEN` | A classic token with `public_repo`, for `wingetcreate` to open a pull request to microsoft/winget-pkgs from your fork |
| AUR | `AUR_SSH_KEY` | An AUR account that owns `termos-bin`, and the private half of the SSH key registered with it |
| Nix | none | The flake in this repository; `nix flake lock` once and commit `flake.lock` |

The manifests' sources are in `packaging/`; run the workflow by hand
(Actions → Packages → Run workflow, with the tag) to publish again.

## The gallery's key

The gallery (docs/gallery.md) trusts only keys built into TermOS. Make
the key once, offline, with a TermOS build:

```sh
termos gallery keygen gallery.key
```

It prints a line for `KEYS` in `crates/termos-apps/src/gallery.rs`;
commit it, and releases from then on trust the gallery. Keep
`gallery.key` with `ca.key`: entries are signed with it (`termos gallery
sign index.json --key gallery.key`) after you review them.

## Making a release

1. Set the version in `Cargo.toml` (`[workspace.package] version`), run
   `cargo xtask ci`, and commit.
2. Tag the commit and push the tag:

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. The `release` workflow builds for about an hour and a half (the images
   take the longest). If the tag and `Cargo.toml` disagree, it stops at
   once.
4. Read the draft on GitHub's releases page, then publish it. TermOS
   machines see it the next time someone runs `termos update check`.
