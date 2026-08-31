{
  cargoNixPluginSrc,
  pkgs,
  source,
}:

let
  lib = pkgs.lib;
  cargoNix = import "${cargoNixPluginSrc}/lib" {
    inherit pkgs;
    src = source;
    buildRustCrateForPkgs =
      cratePkgs: base: args:
      base (
        args
        // cratePkgs.lib.optionalAttrs (args.crateName == "rustnzb") {
          RUSTNZB_SKIP_FRONTEND_BUILD = "1";
          nativeCheckInputs = (args.nativeCheckInputs or [ ]) ++ [
            cratePkgs.p7zip
            cratePkgs.unrar
            cratePkgs.which
          ];
        }
        // cratePkgs.lib.optionalAttrs (args.crateName == "nzb-postproc") {
          nativeCheckInputs = (args.nativeCheckInputs or [ ]) ++ [
            cratePkgs.p7zip
            cratePkgs.unrar
            cratePkgs.which
          ];
        }
        // cratePkgs.lib.optionalAttrs (args.crateName == "openssl-sys") {
          nativeBuildInputs = (args.nativeBuildInputs or [ ]) ++ [ cratePkgs.pkg-config ];
          buildInputs = (args.buildInputs or [ ]) ++ [ cratePkgs.openssl ];
        }
      );
    clippyArgs = [
      "-D"
      "warnings"
    ];
  };
  application = cargoNix.workspaceMembers.rustnzb;
  compiled = application.build;
  workspaceMembers = lib.attrValues cargoNix.workspaceMembers;
  formatSources = map (member: member.build.src) workspaceMembers;
  cargoFormat =
    pkgs.runCommand "rustnzb-workspace-format"
      {
        nativeBuildInputs = [ pkgs.rustfmt ];
      }
      ''
        {
          for sourcePath in ${lib.escapeShellArgs (map toString formatSources)}; do
            find "$sourcePath" -type f -name '*.rs' -print0
          done
        } | sort -zu | xargs -0 -r rustfmt --edition 2024 --check
        touch "$out"
      '';
  cargoCheck = cargoNix.allWorkspaceMembers;
  cargoClippy = cargoNix.clippy.allWorkspaceMembers;
  cargoTest = cargoNix.allWorkspaceMemberTests;
  runtimePath = lib.makeBinPath [
    pkgs.unrar
    pkgs.p7zip
    pkgs.which
  ];
in
pkgs.runCommand "rustnzb-${application.crateInfo.version}"
  {
    nativeBuildInputs = [ pkgs.makeWrapper ];
    passthru = {
      inherit
        cargoCheck
        cargoClippy
        cargoFormat
        cargoTest
        compiled
        ;
    };
    meta = {
      description = "Rust Usenet newsreader and NZB transfer engine";
      homepage = "https://github.com/TheDancingDeveloper-org/rustnzb";
      license = lib.licenses.mit;
      mainProgram = "rustnzb";
      platforms = lib.platforms.linux;
    };
  }
  ''
    mkdir -p "$out/bin"
    makeWrapper ${compiled}/bin/rustnzb "$out/bin/rustnzb" \
      --prefix PATH : ${runtimePath}
  ''
