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

      allWorkspaceMembers =
        (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.members;

      pluginDefinitions = {
        herdr-picker = {
          sourceRoots = [
            "herdr-picker"
            "sdk/rust"
          ];
          binaries = [ "herdr-picker" ];
          binOnly = true;
          platforms = [
            "darwin"
            "linux"
          ];
        };

        herdr-picker-herdr = {
          sourceRoots = [
            "herdr-picker-herdr"
            "sdk/rust"
          ];
          binaries = [
            "herdr-picker-herdr-agents"
            "herdr-picker-herdr-workspaces"
            "herdr-picker-herdr-focus-agent"
            "herdr-picker-herdr-focus-workspace"
          ];
          binOnly = true;
          exampleFiles = [
            "agents.toml"
            "workspaces.toml"
          ];
          platforms = [
            "darwin"
            "linux"
          ];
        };

        equalize-splits = {
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

        fork-to-pane = {
          sourceRoots = [
            "fork-to-pane"
            "sdk/rust"
          ];
          binaries = [ "herdr-fork-to-pane" ];
          platforms = [
            "darwin"
            "linux"
          ];
        };

        history = {
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

        herdr-micro = {
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
          runtimeFiles = [
            "integrations/pi/herdr-effort.js"
            "integrations/thinking-effort.sh"
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
              crate =
                builtins.fromTOML
                  (builtins.readFile (./. + "/${name}/Cargo.toml"));
              pluginSource = sourceFor name definition;
              selectedWorkspaceMembers = definition.sourceRoots;
              selectedWorkspaceMembersText = workspaceMembersText selectedWorkspaceMembers;
              linkPath = "${placeholder "out"}/${name}";
              cargoReleaseDir = "target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release";
              installBinaries = lib.concatMapStringsSep "\n" (
                binary:
                if definition.binOnly or false then
                  ''install -Dm755 "${cargoReleaseDir}/${binary}" "$out/bin/${binary}"''
                else if definition.binLayout or false then
                  ''install -Dm750 "${cargoReleaseDir}/${binary}" "$out/${name}/bin/${binary}"''
                else
                  ''install -Dm755 "${cargoReleaseDir}/${binary}" "$out/target/release/${binary}"''
              ) definition.binaries;
              installRuntimeFiles = lib.concatMapStringsSep "\n" (
                file: ''install -Dm444 "${name}/${file}" "$out/${name}/${file}"''
              ) (definition.runtimeFiles or [ ]);
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
                substituteInPlace Cargo.toml \
                  --replace-fail ${lib.escapeShellArg originalWorkspaceMembers} \
                  ${lib.escapeShellArg selectedWorkspaceMembersText}
              '';

              installPhase = ''
                runHook preInstall

                ${lib.optionalString (!(definition.binOnly or false)) ''
                  install -Dm444 "${name}/herdr-plugin.toml" "$out/${name}/herdr-plugin.toml"
                ''}
                ${installBinaries}
                ${installRuntimeFiles}
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
