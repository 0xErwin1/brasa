{
  description = "Brasa - a statically-typed scripting language with a bytecode VM";

  inputs = {
    nixpkgs.url = "github:cachix/devenv-nixpkgs/rolling";
    devenv.url = "github:cachix/devenv";
  };

  outputs = { self, nixpkgs, devenv, ... } @ inputs:
    let
      forEachSystem = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];

      mkBrasa = pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "brasa";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;

          # .cargo/config.toml forces `linker = "clang"` with a mold link-arg
          # for fast local rebuilds. Neither is needed for a Nix release
          # build: mold only speeds up iterative dev builds, and the flake
          # sandbox has no guarantee of a working mold/clang toolchain
          # matching the pinned nixpkgs. Drop the override so the build uses
          # the default linker from stdenv instead of wiring in clang/mold.
          postPatch = ''
            rm -f .cargo/config.toml
          '';

          cargoBuildFlags = [ "-p" "brasa" ];
          # Scope checkPhase to the same package: buildRustPackage's default
          # check runs `cargo test` across the whole workspace, which would
          # also exercise the other workspace crates' test suites even
          # though this package output only builds and ships `brasa`.
          cargoTestFlags = [ "-p" "brasa" ];

          meta = {
            description = "Brasa - a statically-typed scripting language with a bytecode VM";
            license = pkgs.lib.licenses.gpl3Only;
            mainProgram = "brasa";
          };
        };

      # The Brasa analogue of `pkgs.writeShellApplication`: same shape
      # (name, runtimeInputs, inline text or a source file), but the
      # build runs `brasa --check`, so a script that does not type-check
      # can never reach an activated generation.
      mkWriteBrasaApplication = pkgs: brasa:
        { name
        , text ? null
        , src ? null
        , runtimeInputs ? [ ]
        }:
        assert pkgs.lib.assertMsg ((text == null) != (src == null))
          "writeBrasaApplication ${name}: exactly one of `text` or `src` must be set";
        let
          script =
            if text != null
            then pkgs.writeText "${name}.bras" text
            else src;

          pathPrefix = pkgs.lib.optionalString (runtimeInputs != [ ])
            "--prefix PATH : ${pkgs.lib.makeBinPath runtimeInputs}";
        in
        pkgs.runCommand name
          {
            nativeBuildInputs = [ brasa pkgs.makeBinaryWrapper ];
            meta.mainProgram = name;
          } ''
          brasa --check ${script}

          mkdir -p $out/bin
          makeWrapper ${brasa}/bin/brasa $out/bin/${name} \
            --add-flags ${script} \
            ${pathPrefix}
        '';
    in
    {
      overlays.default = final: prev:
        let brasa = mkBrasa final;
        in {
          inherit brasa;
          writeBrasaApplication = mkWriteBrasaApplication final brasa;
        };

      packages = forEachSystem (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          brasa = mkBrasa pkgs;
          default = self.packages.${system}.brasa;
        });

      apps = forEachSystem (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.brasa}/bin/brasa";
        };
      });

      devShells = forEachSystem (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = devenv.lib.mkShell {
            inherit inputs pkgs;
            modules = [
              {
                languages.rust.enable = true;

                packages = [
                  pkgs.mold
                  pkgs.clang
                  pkgs.cargo-insta
                ];
              }
            ];
          };
        });
    };
}
