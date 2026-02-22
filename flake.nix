{
  description = "Fnug - A nice lint runner";

  nixConfig = {
    extra-substituters = ["https://fnug.cachix.org"];
    extra-trusted-public-keys = ["fnug.cachix.org-1:SDUeF2nZSbSPOAMNJdYZdoVB+tHdB8UHHcqhEmizeNk="];
  };

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
      systems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];

      flake.overlays.default = final: prev: {
        fnug = final.callPackage ({
          lib,
          rustPlatform,
          pkg-config,
          cmake,
          openssl,
        }: let
          cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        in
          rustPlatform.buildRustPackage {
            pname = "fnug";
            inherit (cargoToml.package) version;
            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
              outputHashes = {};
            };

            doCheck = false;

            postPatch = ''
              cp -r vendor/vt100 $cargoDepsCopy/fnug-vt100-0.15.2
            '';

            nativeBuildInputs = [pkg-config cmake];
            buildInputs = [openssl];

            meta = {
              description = "A nice lint runner";
              inherit (cargoToml.package) homepage;
              license = lib.licenses.mit;
              mainProgram = "fnug";
            };
          }) {};
      };

      perSystem = {
        self',
        config,
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
          inputsFrom = [self'.packages.default];

          packages = [
            self'.packages.default
            pkgs.cachix
          ];

          nativeBuildInputs = with pkgs; [
            rustToolchain
            pkg-config
            cmake
          ];
        };
      };
    };
}
