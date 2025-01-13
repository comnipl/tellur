{
  description = "Development environment for tellur";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/release-24.11";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { nixpkgs, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.stdenv.mkDerivation {
          name = "tellur-dev";
          nativeBuildInputs = with pkgs; [
            pkg-config
            gnuplot
          ] ++ [
            toolchain
          ];
          buildInputs = with pkgs; [
            openssl
            libiconv
          ] ++ lib.optionals stdenvNoCC.isDarwin [
            darwin.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ];
        };
      }
    );
}
