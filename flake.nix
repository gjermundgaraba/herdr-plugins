{
  description = "Herdr plugins built as independently reusable Nix packages";

  inputs = {
    # 26.05 still supports both Intel and Apple Silicon macOS.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      forAllSystems = nixpkgs.lib.genAttrs systems;
      workspaceManifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);

      perSystem = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          inherit pkgs rustToolchain;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
        }
      );

      # Only what cannot be derived lives here: the dependency topology
      # (sourceRoots) and how the output is laid out. Platforms come from each
      # plugin's herdr-plugin.toml and binaries from the crate itself.
      pluginDefinitions = {
        herdr-picker = {
          sourceRoots = [
            "herdr-picker"
            "sdk/picker"
            "sdk/ratatui"
            "sdk/rust"
          ];
          binOnly = true;
        };

        herdr-picker-agents = {
          sourceRoots = [
            "herdr-picker-agents"
            "sdk/picker"
            "sdk/rust"
          ];
          binOnly = true;
          exampleFiles = [ "agents.toml" ];
        };

        herdr-picker-workspaces = {
          sourceRoots = [
            "herdr-picker-workspaces"
            "sdk/picker"
            "sdk/rust"
          ];
          binOnly = true;
          exampleFiles = [ "workspaces.toml" ];
        };

        equalize-splits = {
          sourceRoots = [
            "equalize-splits"
            "sdk/rust"
          ];
        };

        fork-to-pane = {
          sourceRoots = [
            "fork-to-pane"
            "sdk/rust"
          ];
        };

        history = {
          sourceRoots = [
            "history"
            "sdk/rust"
          ];
        };

        space-meta = {
          sourceRoots = [
            "space-meta"
            "sdk/rust"
          ];
        };

        # herdr-micro is deliberately absent: its service binary must be
        # codesigned with a local Apple Development identity, which a pure Nix
        # build cannot do. Build it through the plugin manifest instead.
      };

      # Plugin manifests declare "macos"/"linux"; Nix says "darwin"/"linux".
      # Crates without a manifest are plain CLI tools that build everywhere.
      platformsFor =
        name:
        let
          manifest = ./. + "/${name}/herdr-plugin.toml";
        in
        if builtins.pathExists manifest then
          map (platform: if platform == "macos" then "darwin" else platform) (
            (builtins.fromTOML (builtins.readFile manifest)).platforms
          )
        else
          [
            "darwin"
            "linux"
          ];

      # Cargo builds the package binary plus one binary per src/bin file.
      binariesFor =
        name: crate:
        let
          binDir = ./. + "/${name}/src/bin";
          extras =
            if builtins.pathExists binDir then
              map (nixpkgs.lib.removeSuffix ".rs") (
                builtins.filter (nixpkgs.lib.hasSuffix ".rs") (builtins.attrNames (builtins.readDir binDir))
              )
            else
              [ ];
        in
        [ crate.package.name ] ++ extras;

    in
    {
      packages = forAllSystems (
        system:
        let
          inherit (perSystem.${system}) pkgs rustPlatform;
          inherit (pkgs) lib;

          platformName = if pkgs.stdenv.hostPlatform.isDarwin then "darwin" else "linux";
          supportedDefinitions = lib.filterAttrs (
            name: _definition: lib.elem platformName (platformsFor name)
          ) pluginDefinitions;

          sourceFor =
            name: definition:
            lib.cleanSourceWith {
              name = "herdr-${name}-source";
              src = ./.;
              filter =
                path: _type:
                let
                  relative = lib.removePrefix "${toString ./.}/" (toString path);
                  inSourceRoot =
                    root:
                    relative == root
                    || lib.hasPrefix "${root}/" relative
                    || (relative != "" && lib.hasPrefix "${relative}/" root);
                in
                relative == "Cargo.toml"
                || relative == "Cargo.lock"
                || lib.any inSourceRoot definition.sourceRoots;
            };

          buildPlugin =
            name: definition:
            let
              crate =
                builtins.fromTOML
                  (builtins.readFile (./. + "/${name}/Cargo.toml"));
              pluginSource = sourceFor name definition;
              selectedManifest = workspaceManifest // {
                workspace = workspaceManifest.workspace // {
                  members = definition.sourceRoots;
                };
              };
              selectedCargoToml =
                (pkgs.formats.toml { }).generate "Cargo-${name}.toml" selectedManifest;
              linkPath = "${placeholder "out"}/${name}";
              cargoReleaseDir = "target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release";
              installBinaries = lib.concatMapStringsSep "\n" (
                binary:
                if definition.binOnly or false then
                  ''install -Dm755 "${cargoReleaseDir}/${binary}" "$out/bin/${binary}"''
                else
                  ''install -Dm755 "${cargoReleaseDir}/${binary}" "$out/target/release/${binary}"''
              ) (binariesFor name crate);
              installExampleFiles = lib.concatMapStringsSep "\n" (
                file:
                ''install -Dm444 "${name}/examples/${file}" "$out/share/herdr-picker/examples/${file}"''
              ) (definition.exampleFiles or [ ]);
            in
            rustPlatform.buildRustPackage {
              pname = if definition.binOnly or false then name else "herdr-plugin-${name}";
              inherit (crate.package) version;
              src = pluginSource;

              cargoLock.lockFile = ./Cargo.lock;
              cargoBuildFlags = [
                "--package"
                crate.package.name
              ];
              doCheck = true;

              postPatch = ''
                install -m644 ${selectedCargoToml} Cargo.toml
              '';

              installPhase = ''
                runHook preInstall

                ${lib.optionalString (!(definition.binOnly or false)) ''
                  install -Dm444 "${name}/herdr-plugin.toml" "$out/${name}/herdr-plugin.toml"
                ''}
                ${installBinaries}
                ${installExampleFiles}

                runHook postInstall
              '';

              passthru = {
                localPathDependencyClosure = definition.sourceRoots;
              } // lib.optionalAttrs (!(definition.binOnly or false)) {
                pluginRoot = linkPath;
              };

              meta = {
                description =
                  if definition.binOnly or false
                  then crate.package.description
                  else "Prebuilt ${name} plugin for Herdr";
                homepage = "https://github.com/gjermundgaraba/herdr-plugins/tree/main/${name}";
                license = lib.licenses.asl20;
                platforms =
                  (lib.optionals (lib.elem "linux" (platformsFor name)) lib.platforms.linux)
                  ++ (lib.optionals (lib.elem "darwin" (platformsFor name)) lib.platforms.darwin);
              };
            };
        in
        lib.mapAttrs buildPlugin supportedDefinitions
      );

      checks = self.packages;

      devShells = forAllSystems (
        system:
        let
          inherit (perSystem.${system}) pkgs rustToolchain;
        in
        {
          default = pkgs.mkShell {
            packages = [ rustToolchain ];
          };
        }
      );
    };
}
