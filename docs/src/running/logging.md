# Logging

Torsten uses the [tracing](https://docs.rs/tracing) ecosystem for structured logging. It supports multiple output targets, log rotation for file output, and fine-grained level control.

## Output Targets

Torsten can log to one or more output targets simultaneously using the `--log-output` flag. You can specify this flag multiple times to enable multiple targets:

```bash
# Stdout only (default)
torsten-node run --log-output stdout ...

# File only
torsten-node run --log-output file ...

# Both stdout and file
torsten-node run --log-output stdout --log-output file ...

# Systemd journal (requires journald feature)
torsten-node run --log-output journald ...
```

### Stdout

The default output target. Logs are written to standard output with ANSI color codes when the output is a terminal. Colors can be disabled with `--log-no-color`.

### File

Logs are written to rotating log files in the directory specified by `--log-dir` (default: `logs/`). The rotation strategy is configured with `--log-file-rotation`:

| Strategy | Description |
|----------|-------------|
| `daily` | Rotate log files daily (default) |
| `hourly` | Rotate log files every hour |
| `never` | Write to a single `torsten.log` file with no rotation |

```bash
torsten-node run \
  --log-output file \
  --log-dir /var/log/torsten \
  --log-file-rotation daily \
  ...
```

File output uses non-blocking I/O with buffered writes. The buffer is flushed automatically on shutdown.

### Journald

Native systemd journal integration. This requires building Torsten with the `journald` feature:

```bash
cargo build --release --features journald
```

Then run with:

```bash
torsten-node run --log-output journald ...
```

View logs with `journalctl`:

```bash
journalctl -u torsten-node -f
journalctl -u torsten-node --since "1 hour ago"
```

## Log Levels

The log level can be set via the `--log-level` CLI flag or the `RUST_LOG` environment variable. If both are set, `RUST_LOG` takes priority.

```bash
# Via CLI flag
torsten-node run --log-level debug ...

# Via environment variable (takes priority)
RUST_LOG=debug torsten-node run ...
```

Available levels (from most to least verbose):

| Level | Description |
|-------|-------------|
| `trace` | Very detailed internal diagnostics |
| `debug` | Internal operations: genesis loading, storage ops, network handshakes, epoch transitions |
| `info` | Operator-relevant events: sync progress, peer connections, block production (default) |
| `warn` | Potential issues: stale snapshots, replay failures |
| `error` | Errors that may affect node operation |

### Per-Crate Filtering

Use `RUST_LOG` for fine-grained control over which components produce output:

```bash
# Debug only for specific crates
RUST_LOG=torsten_network=debug,torsten_consensus=debug torsten-node run ...

# Trace storage operations, debug everything else
RUST_LOG=torsten_storage=trace,debug torsten-node run ...

# Silence noisy crates
RUST_LOG=info,torsten_network=warn torsten-node run ...
```

## Log Format

Torsten uses a fixed-width column format for readable, aligned output:

```
HH:MM:SS.mmm  LEVEL  target                          message
```

Example output:

```
12:34:56.789  INFO torsten_node::node              Sync         slot=142857392 block=11283746 epoch=512 utxo=15234892 sync=95.42% speed=312 blk/s
12:34:56.790  INFO torsten_node::node              Peer         connected to 1.2.3.4:3001 (42ms)
12:34:57.123 DEBUG torsten_ledger::state::epoch     Stake distribution rebuilt for epoch 512
```

The target column is fixed at 30 characters. Longer module paths are truncated from the left (keeping the most specific part). Colors are automatically enabled when stdout is a terminal.

## CLI Reference

All logging flags are shared between the `run` and `mithril-import` subcommands:

| Flag | Default | Description |
|------|---------|-------------|
| `--log-output` | `stdout` | Log output target: `stdout`, `file`, or `journald`. Can be specified multiple times. |
| `--log-level` | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error`. Overridden by `RUST_LOG`. |
| `--log-dir` | `logs` | Directory for log files (used with `--log-output file`) |
| `--log-file-rotation` | `daily` | Log file rotation: `daily`, `hourly`, or `never` |
| `--log-no-color` | `false` | Disable ANSI colors in stdout output |

## Production Recommendations

For production deployments:

```bash
torsten-node run \
  --log-output file \
  --log-output journald \
  --log-dir /var/log/torsten \
  --log-file-rotation daily \
  --log-no-color \
  ...
```

This configuration:
- Writes structured logs to systemd journal for `journalctl` integration
- Writes rotated log files for archival and debugging
- Disables ANSI colors (not needed for file/journal output, but harmless)

For containerized deployments (Docker, Kubernetes), stdout is typically sufficient since the container runtime captures output:

```bash
torsten-node run --log-output stdout ...
```
