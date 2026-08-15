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
      # The project-shaped analogue of `writeBrasaApplication`: `src` is a
      # directory whose `brasa.toml` names the entry, and the output is a
      # BUNDLED executable rather than a wrapper around a script. The
      # difference is load-bearing: import aliases resolve against the
      # manifest at compile time, and a wrapper run from any cwd would
      # never find `brasa.toml` again — `brasa bundle` embeds every
      # resolved module, so nothing is needed at run time. Bundling also
      # runs the full compiler, so the same gate holds: a project that
      # does not compile can never reach an activated generation.
      mkWriteBrasaProject = pkgs: brasa:
        { src
        , name ? null
        , runtimeInputs ? [ ]
        }:
        let
          manifest = builtins.fromTOML (builtins.readFile (src + "/brasa.toml"));
        in
        assert pkgs.lib.assertMsg (manifest ? build && manifest.build ? entry)
          "writeBrasaProject: ${toString src}/brasa.toml defines no `build.entry`";
        let
          entry = manifest.build.entry;

          # The same default chain the CLI uses for `bundle`:
          # project.name, then the entry's stem.
          finalName =
            if name != null
            then name
            else manifest.project.name or (pkgs.lib.removeSuffix ".bras" (baseNameOf entry));

          wrapped = runtimeInputs != [ ];
          bundled = if wrapped then "$out/libexec/${finalName}" else "$out/bin/${finalName}";
        in
        pkgs.runCommand finalName
          {
            nativeBuildInputs = [ brasa ]
              ++ pkgs.lib.optional wrapped pkgs.makeBinaryWrapper;
            meta.mainProgram = finalName;
          } ''
          mkdir -p $out/bin ${pkgs.lib.optionalString wrapped "$out/libexec"}

          cd ${src}
          brasa bundle -o ${bundled}

          ${pkgs.lib.optionalString wrapped ''
            makeWrapper ${bundled} $out/bin/${finalName} \
              --prefix PATH : ${pkgs.lib.makeBinPath runtimeInputs}
          ''}
        '';
    in
    {
      overlays.default = final: prev:
        let brasa = mkBrasa final;
        in {
          inherit brasa;
          writeBrasaApplication = mkWriteBrasaApplication final brasa;
          writeBrasaProject = mkWriteBrasaProject final brasa;
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
