{
  description = "rustnzb source package and exact Rust checks";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forEachSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          rustnzb = pkgs.rustPlatform.buildRustPackage {
            pname = "rustnzb";
            version = "1.4.6";
            src = self;

            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "rustnzb"
            ];
            doCheck = false;

            nativeBuildInputs = [
              pkgs.makeWrapper
              pkgs.pkg-config
            ];
            buildInputs = [ pkgs.openssl ];
            RUSTNZB_BUILD_REF = self.shortRev or "dirty";
            RUSTNZB_SKIP_FRONTEND_BUILD = "1";

            postInstall = ''
              wrapProgram "$out/bin/rustnzb" \
                --prefix PATH : ${
                  pkgs.lib.makeBinPath [
                    pkgs.p7zip
                    pkgs.which
                  ]
                }
            '';

            meta = {
              description = "Rust Usenet newsreader and NZB transfer engine";
              homepage = "https://github.com/TheDancingDeveloper-org/rustnzb";
              license = pkgs.lib.licenses.mit;
              mainProgram = "rustnzb";
              platforms = systems;
            };
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
          pkgs = import nixpkgs { inherit system; };
          rustnzb = self.packages.${system}.rustnzb;
          rustCommand =
            name: command:
            rustnzb.overrideAttrs (old: {
              pname = "rustnzb-${name}";
              cargoBuildType = "debug";
              doCheck = false;
              nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
                pkgs.clippy
                pkgs.rustfmt
              ];
              buildPhase = ''
                runHook preBuild
                ${command}
                runHook postBuild
              '';
              installPhase = ''
                runHook preInstall
                touch "$out"
                runHook postInstall
              '';
              postInstall = "";
            });
        in
        {
          package = rustnzb;
          fmt = rustCommand "fmt" ''
            cargo fmt --all --check
          '';
          check = rustCommand "check" ''
            cargo check --offline --locked --workspace --all-targets
          '';
          clippy = rustCommand "clippy" ''
            cargo clippy --offline --locked --workspace --all-targets -- -D warnings
          '';
          test = rustCommand "test" ''
            cargo test --offline --locked --workspace
          '';
        }
      );
    };
}
