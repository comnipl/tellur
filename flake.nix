{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix.url = "github:nix-community/fenix";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      fenix,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        rustToolchain = fenix.packages.${system}.fromToolchainFile {
          file = ./rust-toolchain.toml;
          sha256 = "sha256-gh/xTkxKHL4eiRXzWv8KP7vfjSk61Iq48x47BEDFgfk=";
        };
      in
      {
        devShells.default = pkgs.mkShell {
          name = "tellur";

          packages = [
            rustToolchain
            pkgs.cargo-watch
            pkgs.cargo-nextest
            pkgs.just
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.pkg-config
            pkgs.mold
            pkgs.fontconfig
          ];
        };
      }
    );
}
