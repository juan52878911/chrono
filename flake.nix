{
  description = "chrono — conocimiento del historial de Git, acotado para una IA";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in {
      packages = forAll (pkgs: {
        default = pkgs.buildGoModule {
          pname = "chrono";
          version = "0.1.0";
          src = ./.;
          vendorHash = "sha256-tQ39T7gLFDK5OH6MxXsx0usdHRPdAdOViSP9P1Df5B4=";
          ldflags = [ "-s" "-w" "-X main.version=v0.1.0" ];
          subPackages = [ "cmd/chrono" ];
          # chrono usa `git` (obligatorio) y `gh` (opcional, para PRs) en runtime.
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postInstall = ''
            wrapProgram $out/bin/chrono --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.git pkgs.gh ]}
          '';
          meta.description = "Conocimiento del historial de Git en un binario, para una IA";
        };
      });

      # Para `nix run` y `nix profile install`.
      apps = forAll (pkgs: {
        default = { type = "app"; program = "${self.packages.${pkgs.system}.default}/bin/chrono"; };
      });
    };
}
