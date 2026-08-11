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

      allWorkspaceMembers = [
        "command-palette"
        "equalize-splits"
        "herdr-micro"
        "herdr-micro/codex-micro"
        "history"
        "popup-terminal"
        "sdk/ratatui"
        "sdk/rust"
      ];

      pluginDefinitions = {
        command-palette = {
          crateName = "herdr-command-palette";
          version = "0.2.0";
          sourceRoots = [
            "command-palette"
            "sdk/ratatui"
            "sdk/rust"
          ];
          binaries = [ "herdr-command-palette" ];
          platforms = [
            "darwin"
            "linux"
          ];
        };

        equalize-splits = {
          crateName = "herdr-equalize-splits";
          version = "0.3.0";
          sourceRoots = [
            "equalize-splits"
            "sdk/rust"
          ];
          binaries = [ "herdr-equalize-splits" ];
          platforms = [
            "darwin"
            "linux"
          ];
        };

        history = {
          crateName = "herdr-history";
          version = "0.1.0";
          sourceRoots = [
            "history"
            "sdk/rust"
          ];
          binaries = [ "herdr-history" ];
          platforms = [
            "darwin"
            "linux"
          ];
        };

        popup-terminal = {
          crateName = "herdr-popup-terminal";
          version = "0.2.0";
          sourceRoots = [
            "popup-terminal"
            "sdk/rust"
          ];
          binaries = [ "herdr-popup-terminal" ];
          platforms = [
            "darwin"
            "linux"
          ];
        };

        herdr-micro = {
          crateName = "herdr-micro";
          version = "0.1.0";
          # This includes codex-micro, the plugin's local path dependency.
          sourceRoots = [
            "herdr-micro"
            "herdr-micro/codex-micro"
            "sdk/rust"
          ];
          binaries = [
            "herdr-micro"
            "herdr-micro-hid"
          ];
          platforms = [ "darwin" ];
          binLayout = true;
        };
      };

      workspaceMembersText = members: ''
        members = [
        ${nixpkgs.lib.concatMapStringsSep "\n" (member: "    \"${member}\",") members}
        ]
      '';

      originalWorkspaceMembers = workspaceMembersText allWorkspaceMembers;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          inherit (pkgs) lib;

          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };

          platformName = if pkgs.stdenv.hostPlatform.isDarwin then "darwin" else "linux";
          supportedDefinitions = lib.filterAttrs (
            _name: definition: lib.elem platformName definition.platforms
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
              pluginSource = sourceFor name definition;
              selectedWorkspaceMembers = definition.sourceRoots;
              selectedWorkspaceMembersText = workspaceMembersText selectedWorkspaceMembers;
              linkPath = "${placeholder "out"}/${name}";
              cargoReleaseDir = "target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release";
              installBinaries = lib.concatMapStringsSep "\n" (
                binary:
                if definition.binLayout or false then
                  ''install -Dm750 "${cargoReleaseDir}/${binary}" "$out/${name}/bin/${binary}"''
                else
                  ''install -Dm755 "${cargoReleaseDir}/${binary}" "$out/target/release/${binary}"''
              ) definition.binaries;
            in
            rustPlatform.buildRustPackage {
              pname = "herdr-plugin-${name}";
              inherit (definition) version;
              src = pluginSource;

              cargoLock.lockFile = ./Cargo.lock;
              cargoBuildFlags = [
                "--package"
                definition.crateName
              ];
              doCheck = true;

              postPatch = ''
                substituteInPlace Cargo.toml \
                  --replace-fail ${lib.escapeShellArg originalWorkspaceMembers} \
                  ${lib.escapeShellArg selectedWorkspaceMembersText}
              '';

              installPhase = ''
                runHook preInstall

                mkdir -p "$out/${name}"
                install -Dm444 "${name}/herdr-plugin.toml" "$out/${name}/herdr-plugin.toml"
                ${installBinaries}

                runHook postInstall
              '';

              passthru = {
                pluginRoot = linkPath;
                localPathDependencyClosure = definition.sourceRoots;
              };

              meta = {
                description = "Prebuilt ${name} plugin for Herdr";
                homepage = "https://github.com/gjermundgaraba/herdr-plugins/tree/main/${name}";
                license = lib.licenses.asl20;
                platforms =
                  (lib.optionals (lib.elem "linux" definition.platforms) lib.platforms.linux)
                  ++ (lib.optionals (lib.elem "darwin" definition.platforms) lib.platforms.darwin);
              };
            };
        in
        lib.mapAttrs buildPlugin supportedDefinitions
      );

      checks = forAllSystems (
        system:
        nixpkgs.lib.mapAttrs' (
          name: package: nixpkgs.lib.nameValuePair "build-${name}" package
        ) self.packages.${system}
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          default = pkgs.mkShell {
            packages = [ rustToolchain ];
          };
        }
      );
    };
}
