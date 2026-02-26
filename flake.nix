{
  description = "Cardano Lightning relay — Lightning ↔ Cardano bridge";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-utils.url = "github:numtide/flake-utils";

    # Sibling crate required by Cargo.toml path dependency
    cardano-lightning-client = {
      url = "path:../cardano-lightning-client";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, cardano-lightning-client }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rust = pkgs.rust-bin.stable."1.85.0".default;

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rust;
          rustc = rust;
        };

        # Assemble combined source tree so path dep ../cardano-lightning-client resolves
        combinedSrc = pkgs.stdenv.mkDerivation {
          name = "cardano-lightning-src";
          phases = [ "installPhase" ];
          installPhase = ''
            mkdir -p $out/cardano-lightning-relay $out/cardano-lightning-client
            cp -a ${self}/. $out/cardano-lightning-relay
            cp -a ${cardano-lightning-client}/. $out/cardano-lightning-client
          '';
        };
      in
      {
        packages.default = rustPlatform.buildRustPackage {
          pname = "cardano-lightning-relay";
          version = "0.2.0";

          src = combinedSrc;
          buildAndTestSubdir = "cardano-lightning-relay";

          cargoLock = {
            lockFile = self + "/Cargo.lock";
          };

          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [ openssl ];
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ rust pkg-config ];
          buildInputs = with pkgs; [ openssl ];
        };
      }
    );
}
