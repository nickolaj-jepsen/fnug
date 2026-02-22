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
      systems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];

      perSystem = {
        self',
        pkgs,
        system,
        ...
      }: let
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      in {
        _module.args.pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [(import inputs.rust-overlay)];
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "fnug";
          inherit (cargoToml.package) version;
          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
            # Vendored path dependency — tell Nix to use the local source
            outputHashes = {};
          };

          # Test uses /root which doesn't exist in the Nix sandbox
          doCheck = false;

          postPatch = ''
            cp -r vendor/vt100 $cargoDepsCopy/fnug-vt100-0.15.2
          '';

          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
          ];

          buildInputs = with pkgs; [
            openssl
          ];
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [self'.packages.default];

          packages = [
            self'.packages.default
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
