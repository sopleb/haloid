{
  description = "Haloid — a self-hosted media host in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        # Native libraries the Rust crates link against.
        nativeLibs = with pkgs; [
          vips          # haloid-media: decode/resize/thumbnail/watermark
          glib          # vips dependency, needed on the pkg-config path
          ffmpeg        # haloid-media: ffprobe + frame extraction for video/audio
          openssl
          zlib
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            pkg-config
            # Rust from nixpkgs so the shell is self-contained: the host profile
            # has rustc but no rustfmt or clippy, and CI gates on both.
            rustc
            cargo
            rustfmt
            clippy
            mariadb.client   # `mariadb` CLI for the local dev database
            nodejs_24
            pnpm
            chromium         # design-taste-frontend screenshot scripts
            cargo-nextest
            sqlx-cli
          ];

          buildInputs = nativeLibs;

          # The taste skill's scripts/*.mjs look for an installed browser.
          CHROME_PATH = "${pkgs.chromium}/bin/chromium";
          PUPPETEER_EXECUTABLE_PATH = "${pkgs.chromium}/bin/chromium";
          PUPPETEER_SKIP_DOWNLOAD = "1";

          shellHook = ''
            echo "haloid dev shell"
            echo "  vips    $(vips --version 2>/dev/null || echo MISSING)"
            echo "  ffprobe $(ffprobe -version 2>/dev/null | head -1 || echo MISSING)"
            echo "  cargo   $(cargo --version 2>/dev/null || echo 'MISSING — install via rustup or nix profile')"
            echo "  node    $(node --version)"
          '';
        };
      });
}
