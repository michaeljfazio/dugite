# Installation

Dugite can be installed from pre-built binaries, a container image, Nix, or built from source.

## Pre-built Binaries

Download the latest release from [GitHub Releases](https://github.com/michaeljfazio/dugite/releases):

| Platform | Architecture | Download |
|----------|-------------|----------|
| Linux | x86_64 | `dugite-x86_64-linux.tar.gz` |
| Linux | aarch64 | `dugite-aarch64-linux.tar.gz` |
| macOS | Apple Silicon | `dugite-aarch64-macos.tar.gz` |

> **Note:** macOS x86_64 (Intel) binaries are not published — GitHub's macOS runners are aarch64-only. Intel Mac users should [build from source](#building-from-source).

Each tarball contains **`dugite-node`, `dugite-cli`, the `config/` tree, `README.md`, and `LICENSE`**. The two TUIs (`dugite-monitor`, `dugite-config`) are *not* in the release tarballs — get them from the [container image](#container-image) or by [building from source](#building-from-source).

```bash
# Example: download and extract for Linux x86_64
curl -LO https://github.com/michaeljfazio/dugite/releases/latest/download/dugite-x86_64-linux.tar.gz
tar xzf dugite-x86_64-linux.tar.gz
sudo mv dugite-node dugite-cli /usr/local/bin/
```

Verify checksums:

```bash
curl -LO https://github.com/michaeljfazio/dugite/releases/latest/download/SHA256SUMS.txt
sha256sum -c SHA256SUMS.txt
```

## Container Image

Multi-arch (`linux/amd64`, `linux/arm64`) images are published to GitHub Container Registry on every tagged release:

```bash
docker pull ghcr.io/michaeljfazio/dugite:latest
# …or pin a release (tag = the release version without the leading "v")
docker pull ghcr.io/michaeljfazio/dugite:<version>
```

The image is distroless, runs as non-root (uid 65532), ships all four binaries (`dugite-node`, `dugite-cli`, `dugite-config`, `dugite-monitor`), and bundles the `config/` tree at `/opt/dugite/config/`. `ENTRYPOINT` is `dugite-node`; the default command runs a preview relay.

```bash
docker run --rm \
  -v dugite-db:/opt/dugite/db \
  -v dugite-ipc:/opt/dugite/ipc \
  -p 3001:3001 \
  ghcr.io/michaeljfazio/dugite:latest \
  run --config /opt/dugite/config/preview/config.json \
      --topology /opt/dugite/config/preview/topology.json \
      --database-path /opt/dugite/db \
      --socket-path /opt/dugite/ipc/node.sock \
      --host-addr 0.0.0.0 --port 3001
```

A Helm chart is published alongside the image as an OCI artifact:

```bash
helm install dugite-relay \
  oci://ghcr.io/michaeljfazio/charts/dugite-node \
  --set network.name=preview
```

See [Kubernetes Deployment](./running/kubernetes.md) for the full chart reference.

## Nix

The repository is a flake (`flake.nix` plus the modules under `nix/`), supporting `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, and `aarch64-darwin`.

```bash
# Build a single binary
nix build github:michaeljfazio/dugite#dugite-node
nix build github:michaeljfazio/dugite#dugite-cli

# Build every binary in one derivation
nix build github:michaeljfazio/dugite#dugite-all

# Development shell (stable Rust via fenix, plus just, jq, fd, ripgrep)
nix develop
```

`packages.default` is `dugite-node`. A NixOS service module lives at `nix/nixosModules/service-dugite.nix`.

> **Caveats:** the `dugite-tui` flake output is stale — no such crate exists in the workspace (the TUIs are `dugite-monitor` and `dugite-config`). The dev shell also does not yet provide `protoc`, which `dugite-rpc`'s build script requires; install it separately (see [System Dependencies](#system-dependencies)) until the flake is updated.

## Building from Source

### Prerequisites

#### Rust Toolchain

Install the latest stable Rust toolchain via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Verify the installation:

```bash
rustc --version
cargo --version
```

Dugite requires the **latest stable Rust** toolchain (edition 2021). Use `rustup update stable` to stay current. The repository does not pin a toolchain — CI and the Nix flake both track `stable`, so there is no `rust-toolchain.toml` to honour.

#### System Dependencies

**`protoc` is required.** The `dugite-rpc` crate generates its gRPC stubs at build time from the vendored UTxO RPC `.proto` files, and its build script invokes `protoc`. Without it, `cargo build` fails on `dugite-rpc` (and therefore on `dugite-node`, which depends on it).

```bash
# Debian / Ubuntu — libprotobuf-dev supplies the google/protobuf/*.proto well-known types
sudo apt-get install -y protobuf-compiler libprotobuf-dev

# macOS
brew install protobuf
```

Beyond `protoc`, there is nothing else to install. The storage layer is pure Rust: block storage uses append-only chunk files, and the UTxO set uses `dugite-lsm`, a pure Rust LSM tree — no RocksDB, no LMDB, no C toolchain requirement.

Optionally install [`just`](https://github.com/casey/just) to use the top-level task runner (`just build`, `just check`, …). See [Development](./development.md).

### Build

Clone the repository:

```bash
git clone https://github.com/michaeljfazio/dugite.git
cd dugite
```

Build in release mode:

```bash
cargo build --release
# …or, with just installed:
just build
```

On Linux with kernel 5.1+, you can enable io_uring for improved disk I/O in the UTxO LSM tree. The feature is defined on `dugite-storage` and re-exported by `dugite-node`:

```bash
cargo build --release --features io-uring
```

This produces four operator binaries in `target/release/`:

| Binary | Description |
|--------|-------------|
| `dugite-node` | The Cardano node |
| `dugite-cli` | The cardano-cli compatible command-line interface |
| `dugite-monitor` | Terminal monitoring dashboard (ratatui-based, real-time metrics via Prometheus polling) |
| `dugite-config` | Interactive TUI configuration editor with tree navigation, inline editing, and diff view |

A workspace build also emits internal development helpers (`apply_bench`, `probe_block`, `replay_phase2`, `capture-ratification-fixture`, `xtask`). Those are not part of the operator surface and are not shipped in releases.

#### Install Binaries

To install the binaries into your `$CARGO_HOME/bin` (typically `~/.cargo/bin/`):

```bash
cargo install --path crates/dugite-node
cargo install --path crates/dugite-cli --bin dugite-cli
cargo install --path crates/dugite-monitor
cargo install --path crates/dugite-config
```

(`--bin dugite-cli` keeps the internal `capture-ratification-fixture` helper out of your `bin/` directory.)

## Running Tests

Verify everything is working (requires [cargo-nextest](https://nexte.st/)):

```bash
cargo nextest run --workspace
cargo test --doc
```

The project enforces a zero-warning policy. Run the full CI gate locally with a single recipe:

```bash
just check     # fmt-check → clippy → build → test → test-doc
```

Or invoke the same steps directly:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build --release --all-targets
cargo nextest run --workspace
cargo test --doc
```

## Development Build

For faster compilation during development, use the debug profile (the default):

```bash
cargo build
```

Debug builds are significantly faster to compile but produce slower binaries. Always use `--release` for running a node against a live network.
