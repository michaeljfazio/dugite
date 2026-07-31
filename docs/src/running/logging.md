# Logging

Dugite uses the [tracing](https://docs.rs/tracing) ecosystem for structured logging. It supports multiple output targets, structured and human-readable formats, log rotation for file output, and fine-grained level control.

## Output Formats

Dugite supports two log formats, selectable via the `--log-format` flag:

### Text (default)

Human-readable compact output with timestamps, level, target module, and structured fields:

```bash
dugite-node run --log-format text ...
```

```
2026-03-12T12:34:56.789Z  INFO dugite_node::node: Syncing progress="95.42%" epoch=512 block=11283746 tip=11300000 remaining=16254 speed="312 blk/s" utxos=15234892
2026-03-12T12:34:56.790Z  INFO dugite_node::node: Peer connected peer=1.2.3.4:3001 rtt_ms=42
```

### JSON

Structured JSON output, one object per line. Ideal for log aggregation systems (ELK, Loki, Datadog):

```bash
dugite-node run --log-format json ...
```

```json
{"timestamp":"2026-03-12T12:34:56.789Z","level":"INFO","target":"dugite_node::node","fields":{"message":"Syncing","progress":"95.42%","epoch":512,"block":11283746}}
```

## Output Targets

Dugite can log to one or more output targets simultaneously using the `--log-output` flag. You can specify this flag multiple times to enable multiple targets:

```bash
# Stdout only (default)
dugite-node run --log-output stdout ...

# File only
dugite-node run --log-output file ...

# Both stdout and file
dugite-node run --log-output stdout --log-output file ...

# Systemd journal (requires journald feature)
dugite-node run --log-output journald ...
```

### Stdout

The default output target. Logs are written to standard output with ANSI color codes when the output is a terminal. Colors can be disabled with `--log-no-color`.

### File

Logs are written to rotating log files in the directory specified by `--log-dir` (default: `logs/`). The rotation strategy is configured with `--log-file-rotation`:

| Strategy | Description |
|----------|-------------|
| `daily` | Rotate log files daily (default) |
| `hourly` | Rotate log files every hour |
| `never` | Write to a single `dugite.log` file with no rotation |

```bash
dugite-node run \
  --log-output file \
  --log-dir /var/log/dugite \
  --log-file-rotation daily \
  ...
```

File output uses non-blocking I/O with buffered writes. The buffer is flushed automatically on shutdown — which is one more reason to stop the node with SIGTERM rather than `kill -9`.

Log files are named `dugite.log` (with a date/hour suffix under `daily` /
`hourly` rotation). Note that `--log-retention-days` is accepted but the cleanup
sweep is not currently wired into the running node, so old files accumulate
until you rotate them out yourself (logrotate, a cron job, or a tmpfiles rule).

### Journald

Native systemd journal integration. This requires building Dugite with the `journald` feature, which lives on the `dugite-node` crate:

```bash
cargo build --release -p dugite-node --features journald
```

Requesting `--log-output journald` from a binary built without the feature is a hard startup error, not a silent downgrade to stdout.

Then run with:

```bash
dugite-node run --log-output journald ...
```

View logs with `journalctl`:

```bash
journalctl -u dugite-node -f
journalctl -u dugite-node --since "1 hour ago"
```

## Log Levels

Verbosity is resolved with this precedence, highest first:

1. `RUST_LOG` environment variable
2. `--log-level` CLI flag
3. `LogDirective` config field
4. `MinSeverity` config field

```bash
# Via CLI flag
dugite-node run --log-level debug ...

# Via environment variable (takes priority)
RUST_LOG=debug dugite-node run ...
```

The two config fields are applied by a live filter reload immediately after the
config file is parsed, and only when neither `RUST_LOG` nor `--log-level` is
set — so a CLI or environment override is never clobbered by the file. See
[Configuration → Log Level Control](./configuration.md#log-level-control).

`MinSeverity` uses cardano-node's syslog vocabulary, which is wider than
`tracing`'s. It is translated rather than passed through: `Notice` → `info`, and
`Critical` / `Alert` / `Emergency` → `error`. An unrecognised value falls back to
`info`. Use `LogDirective` when you need per-target control — it is handed to
`EnvFilter` unchanged.

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
RUST_LOG=dugite_network=debug,dugite_consensus=debug dugite-node run ...

# Trace storage operations, debug everything else
RUST_LOG=dugite_storage=trace,debug dugite-node run ...

# Silence noisy crates
RUST_LOG=info,dugite_network=warn dugite-node run ...
```

## CLI Reference

The logging flags are shared by every subcommand that does work: `run`,
`mithril-import`, `dump-snapshot`, `verify-ledger-snapshot`, and
`snapshot-convert`. (`db info` does not initialise the subscriber and takes no
logging flags.)

| Flag | Default | Description |
|------|---------|-------------|
| `--log-output` | `stdout` | Log output target: `stdout`, `file`, or `journald` (aliases: `journal`, `systemd`). Can be specified multiple times. Values are case-insensitive |
| `--log-format` | `text` | Log format: `text` (alias `plain`) or `json` (structured) |
| `--log-level` | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error`. Overridden by `RUST_LOG` |
| `--log-dir` | `logs` | Directory for log files (used with `--log-output file`) |
| `--log-file-rotation` | `daily` | Log file rotation: `daily`, `hourly`, or `never` (alias `none`) |
| `--log-no-color` | `false` | Disable ANSI colors in stdout output. Colors are auto-disabled anyway when stdout is not a terminal |
| `--log-retention-days` | `7` | Accepted but **currently inert** — no cleanup task is wired into the running node |
| `--stdout-overflow` | `drop` | Channel-full policy for the non-blocking stdout writer. `drop` (alias `lossy`) keeps the hot path unblocked and counts dropped lines; `block` (alias `lossless`) parks the producer until the writer drains |

An unrecognised value for any of these is a startup error naming the valid set,
not a silent fallback.

### Non-blocking output and `--stdout-overflow`

Every `tracing` call hands its line to a background writer thread over a bounded
channel rather than performing a synchronous `write(2)` on the emitting tokio
worker. The default `drop` policy means that under a genuine log flood, lines
are discarded rather than stalling block application — which is the right
trade-off in production.

Use `--stdout-overflow block` for development, CI, or forensic capture where
every line must survive. It re-introduces blocking on the hot path, so it
defeats the point of the non-blocking writer under real overload.

## Runtime Log Verbosity Reload (SIGHUP)

Dugite supports changing per-subsystem log verbosity at runtime without restarting the node. This is useful for debugging a specific issue (for example, enabling trace logging for the network layer) without disrupting ongoing block production or sync.

**Workflow:**

1. Edit the node configuration file and add (or update) the `LogDirective` field:

   ```json
   {
     "LogDirective": "info,dugite_network=trace,dugite_consensus=debug"
   }
   ```

   The value accepts any `RUST_LOG`-compatible directive, including `*=debug`, `trace`, or per-module overrides like `dugite_ledger=warn`.

2. Send `SIGHUP` to the running node:

   ```bash
   kill -HUP $(pgrep -x dugite-node)
   ```

   The node re-reads the config file. If `LogDirective` is present and valid, the filter is reloaded immediately across every output target and the change is logged.

   The directive is parsed before any filter handle is touched, so an invalid string leaves the previous filter fully intact rather than half-applied.

3. To restore the original level, remove `LogDirective` from the config and send SIGHUP again, or set it back to `"info"`.

Two details worth knowing, because they differ between startup and SIGHUP:

- **At startup**, `LogDirective` / `MinSeverity` are applied only when neither
  `RUST_LOG` nor `--log-level` is set, so an explicit operator override is never
  clobbered by the file.
- **On SIGHUP**, they are applied unconditionally, and the reload uses the
  directive directly rather than re-consulting `RUST_LOG`. A SIGHUP therefore
  *does* override a level you set on the command line or in the environment.

The log filter is only touched when at least one hot-reloadable field actually
changed. A SIGHUP against an unmodified config file is a no-op, logged as
`config_reload: no fields changed`.

SIGHUP also reloads the topology and the hot-reloadable peer-governor targets.
See [Live Reload](./configuration.md#live-reload-sighup) for the full field
partition.

## Production Recommendations

For production deployments with log aggregation:

```bash
dugite-node run \
  --log-output file \
  --log-output journald \
  --log-format json \
  --log-dir /var/log/dugite \
  --log-file-rotation daily \
  ...
```

This configuration:
- Writes structured JSON logs to systemd journal for `journalctl` integration
- Writes rotated JSON log files for archival and ingestion by log aggregators
- JSON format ensures all structured fields are machine-parseable

For human operators monitoring the console:

```bash
dugite-node run --log-output stdout --log-format text ...
```

For containerized deployments (Docker, Kubernetes), stdout with JSON is ideal since the container runtime captures output and log drivers can parse the structured format:

```bash
dugite-node run --log-output stdout --log-format json ...
```
