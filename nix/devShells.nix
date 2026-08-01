{
  perSystem = {
    config,
    pkgs,
    inputs',
    ...
  }: let
    # Use stable toolchain - same as packages.nix
    toolchain = with inputs'.fenix.packages;
      combine [
        stable.rustc
        stable.cargo
        stable.clippy
        stable.rustfmt
        stable.rust-analyzer
      ];
  in {
    devShells.default = with pkgs;
      mkShell {
        packages =
          [
            # Rust toolchain (stable from fenix)
            toolchain
            cmake
            pkg-config
            openssl
            zlib

            # REQUIRED to build the workspace at all: crates/dugite-rpc's
            # build.rs runs tonic_prost_build::compile_protos, and dugite-node
            # depends on dugite-rpc. Without this, `nix develop` hands you a
            # shell the project cannot build in (#946).
            protobuf

            # Task runner
            just

            # Utilities
            jq
            fd
            ripgrep

            # Tree formatter
            config.treefmt.build.wrapper
          ]
          ++ lib.optionals stdenv.hostPlatform.isDarwin [
            darwin.apple_sdk.frameworks.Security
            darwin.apple_sdk.frameworks.SystemConfiguration
          ];

        # prost-build locates protoc via $PROTOC when it is not on a
        # conventional path.
        PROTOC = "${pkgs.protobuf}/bin/protoc";

        shellHook = ''
          echo "🦀 Dugite - Cardano Node in Rust"
          echo ""
          echo "Rust: $(rustc --version)"
          echo "Cargo: $(cargo --version)"
          echo ""
          echo "Commands:"
          echo "  cargo build --all-targets          # Build everything"
          echo "  cargo test --all                   # Run all tests"
          echo "  cargo clippy --all-targets -- -D warnings  # Lint"
          echo "  cargo fmt --all -- --check         # Check formatting"
          echo ""
          echo "  cargo build --release              # Build release binary"
          echo "  cargo run -p dugite-node -- --help"
          echo "  cargo run -p dugite-cli -- --help"
          echo "  cargo run -p dugite-monitor        # terminal dashboard"
          echo "  cargo run -p dugite-config         # config editor TUI"
        '';
      };
  };
}
