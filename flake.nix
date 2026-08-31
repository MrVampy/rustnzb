{
  description = "rustnzb source package and exact Rust checks";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    cargo-nix-plugin = {
      url = "git+ssh://git@git.mesh:2222/MrVampy/cargo-nix-plugin.git?ref=main";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      cargo-nix-plugin,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
      mkPkgs =
        system:
        import nixpkgs {
          inherit system;
          config.allowUnfreePredicate = package: nixpkgs.lib.getName package == "unrar";
        };
    in
    {
      packages = forEachSystem (
        system:
        let
          pkgs = mkPkgs system;
          rustnzb = import ./nix/package.nix {
            inherit pkgs;
            cargoNixPluginSrc = cargo-nix-plugin.outPath or cargo-nix-plugin;
            source = ./.;
          };

        in
        {
          default = rustnzb;
          inherit rustnzb;
        }
      );

      checks = forEachSystem (
        system:
        let
          pkgs = mkPkgs system;
          rustnzb = self.packages.${system}.rustnzb;
        in
        {
          package = rustnzb;
          compiled = rustnzb.compiled;
          product-boundary =
            assert rustnzb.drvPath != rustnzb.compiled.drvPath;
            pkgs.writeText "rustnzb-product-boundary" "runtime and compiled derivations are distinct\n";
          runtime-extractors =
            pkgs.runCommand "rustnzb-runtime-extractors"
              {
                nativeBuildInputs = [ pkgs.gnugrep ];
              }
              ''
                grep -F -- '${pkgs.unrar}/bin' '${rustnzb}/bin/rustnzb'
                grep -F -- '${pkgs.p7zip}/bin' '${rustnzb}/bin/rustnzb'
                touch "$out"
              '';
          fmt = rustnzb.cargoFormat;
          check = rustnzb.cargoCheck;
          clippy = rustnzb.cargoClippy;
          test = rustnzb.cargoTest;
        }
      );
    };
}
