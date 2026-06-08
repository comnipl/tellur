{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix.url = "github:nix-community/fenix";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      fenix,
      crane,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        rustToolchain = fenix.packages.${system}.fromToolchainFile {
          file = ./rust-toolchain.toml;
          sha256 = "sha256-gh/xTkxKHL4eiRXzWv8KP7vfjSk61Iq48x47BEDFgfk=";
        };
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # The Vite web bundle that tellur-live embeds via include_bytes!. Built
        # here so neither the Rust build nor the published crate needs npm; the
        # result is injected into the source tree before the Rust build so
        # tellur-live's build.rs finds it and short-circuits.
        web = pkgs.buildNpmPackage {
          pname = "tellur-live-web";
          version = "0.1.0";
          src = ./tellur-live/web;
          npmDepsHash = "sha256-UaW+OOrL7kRe125ASCkwJR1ecqiwVmo/ZU1O2z7/TrE=";
          installPhase = ''
            runHook preInstall
            cp -r dist $out
            runHook postInstall
          '';
        };

        commonArgs = {
          pname = "tellur";
          version = "0.1.0";
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          cargoExtraArgs = "--locked --package tellur --features cli";

          # `.cargo/config.toml` links with `-fuse-ld=mold`, so mold must be on
          # PATH for the build (matching the dev shell).
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.mold
          ];
          buildInputs = [ pkgs.fontconfig ];

          # Drop the prebuilt web bundle into the crate so build.rs embeds it
          # rather than invoking npm (which has no network in the sandbox).
          preConfigure = ''
            mkdir -p tellur-live/web/dist
            cp -r ${web}/. tellur-live/web/dist/
          '';
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        tellur = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            doCheck = false;
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.makeWrapper ];
            # `tellur live` shells out to `ffmpeg` for the preview stream, and the
            # GPU renderer dlopens the Vulkan loader at runtime. (Building a
            # project's cdylib still needs a Rust dev environment on PATH.)
            postInstall = ''
              wrapProgram $out/bin/tellur \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.ffmpeg ]} ${
                  pkgs.lib.optionalString pkgs.stdenv.isLinux
                    "--prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath [ pkgs.vulkan-loader ]}"
                }
            '';
          }
        );
      in
      {
        packages = {
          default = tellur;
          tellur = tellur;
          web = web;
        };

        apps.default = flake-utils.lib.mkApp { drv = tellur; };
        apps.tellur = flake-utils.lib.mkApp { drv = tellur; };

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
            pkgs.vulkan-loader
            pkgs.vulkan-tools
          ];

          LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.isLinux (
            pkgs.lib.makeLibraryPath [ pkgs.vulkan-loader ]
          );
        };
      }
    );
}
