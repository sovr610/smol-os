# TermOS as a Nix flake (D-126): `nix run github:sovr610/smol-os`, or
# `nix profile install github:sovr610/smol-os`. It builds from source with
# the Rust version rust-toolchain.toml pins, and installs the shell
# completions and the man page.
{
  description = "TermOS: a terminal-first desktop";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system:
        f (import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; }));
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
    in
    {
      packages = forAll (pkgs:
        let
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
        in
        rec {
          termos = rustPlatform.buildRustPackage {
            pname = "termos";
            inherit version;
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "termos-cli" ];
            # The tests drive shells, PTYs and sockets, which the build
            # sandbox does not have; CI runs them on every change.
            doCheck = false;
            nativeBuildInputs = [ pkgs.installShellFiles pkgs.pkg-config ];
            postInstall = ''
              installShellCompletion --cmd termos \
                --bash <($out/bin/termos completions bash) \
                --zsh <($out/bin/termos completions zsh) \
                --fish <($out/bin/termos completions fish)
              $out/bin/termos man > termos.1
              installManPage termos.1
            '';
            meta = {
              description = "Terminal-first desktop: windows, AI agents and local models in any terminal";
              homepage = "https://github.com/sovr610/smol-os";
              license = pkgs.lib.licenses.gpl3Plus;
              mainProgram = "termos";
            };
          };
          default = termos;
        });
    };
}
