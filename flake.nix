{
  description = "Fnug - A nice lint runner";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake {inherit inputs;} {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      flake.overlays.default = final: _prev: {
        fnug = final.callPackage (
          {
            lib,
            rustPlatform,
            pkg-config,
          }: let
            cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          in
            rustPlatform.buildRustPackage {
              pname = "fnug";
              inherit (cargoToml.package) version;
              src = ./.;

              cargoLock.lockFile = ./Cargo.lock;

              nativeBuildInputs = [pkg-config];

              meta = {
                description = "A nice lint runner";
                inherit (cargoToml.package) homepage;
                license = lib.licenses.gpl3Only;
                mainProgram = "fnug";
              };
            }
        ) {};
      };

      perSystem = {
        pkgs,
        system,
        ...
      }: let
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in {
        _module.args.pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [
            (import inputs.rust-overlay)
            inputs.self.overlays.default
          ];
        };

        packages.default = pkgs.fnug;

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.cachix
            pkgs.alejandra
            pkgs.statix
            pkgs.deadnix
            pkgs.uv
            pkgs.maturin
            pkgs.ruff
            pkgs.vhs
            (pkgs.python3.withPackages (ps: [ps.pyyaml]))
          ];

          nativeBuildInputs = with pkgs; [
            rustToolchain
            pkg-config
          ];
        };
      };
    };
}
