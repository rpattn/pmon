# pmon

A minimal task-manager-style monitor for the Linux terminal: two live line
charts, **CPU %** on top and **RAM** below, for the whole system or for one
process. An optional background daemon records history so you can look back.

```
┌ CPU 0.1% · system ───────────────────────────────────────────────────────────┐
│ 100%│                                                                        │
│     │                                                                        │
│     │                                                                        │
│     │                                                                        │
│  50%│                                                                        │
│     │                                                                        │
│     │                                                       ⣀⠤⠤⠤⠤⠤⠤⠤⠤⣀       │
│   0%│                                            ⣀⣀⣀⣀⣀⣀⣀⣀⠤⠒⠉          ⠉⠒⢄⣀⣀⣀⣀│
│     └────────────────────────────────────────────────────────────────────────│
│  -30s                                  -15s                               now│
└──────────────────────────────────────────────────────────────────────────────┘
┌ RAM 1.18 GiB of 15.5 GiB (8%) · system ──────────────────────────────────────┐
│15.5G│                                                                        │
│     │                                                                        │
│     │                                                                        │
│ 7.7G│                                                                        │
│     │                                                                        │
│     │                                                                        │
│    0│                                            ⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒│
│     └────────────────────────────────────────────────────────────────────────│
│  -30s                                  -15s                               now│
└──────────────────────────────────────────────────────────────────────────────┘
 LIVE  every 1s · bucket 1s max · 30s │ q quit / pick Esc reset                 
```

## Build & install

```bash
cargo install --path .
```

Requires Linux (reads `/proc` via `sysinfo`).

## Usage

```bash
pmon                      # live system-wide CPU and RAM, starting from now
pmon -p                   # fuzzy-pick a process, then chart its CPU and RSS
pmon -p firefox           # skip the picker if the name/pid is unambiguous
pmon -p 4242              # by pid
pmon --since 10m          # live, with a 10 minute window (default 2m)

pmon -d                   # start the history daemon (keeps 1h by default)
pmon -d --retain 6h       # keep 6 hours instead
pmon --status             # is it running, how much history is stored
pmon --stop               # stop it

pmon -H                   # last 10 minutes from the daemon, then follow live
pmon -H --since 1h        # last hour (capped at what is stored)
pmon -H -p                # pick a process from the recorded history
pmon -H -p code --since 30m --agg 30s
```

Durations use `humantime` syntax: `30s`, `5m`, `1h`, `1h30m`, `2days`.

### Keys

| Key            | Action                                           |
| -------------- | ------------------------------------------------ |
| `q`, `Ctrl-C`  | quit                                             |
| `/`            | open the process picker                          |
| `Esc`          | back to system-wide (or close the picker)        |
| picker: type   | fuzzy filter (name, pid, command line)           |
| picker: `↑ ↓`, `Ctrl-P/N`, `PgUp/PgDn` | move                     |
| picker: `Enter`| select · `Ctrl-U` clear · `Ctrl-W` delete word   |

`pmon -p NAME|PID` selects directly when the argument is an existing pid, a
unique exact process name, or has exactly one fuzzy match. With `-H`, if several
recorded processes match but only one is still running, that one is chosen.
Otherwise the picker opens pre-filled with the argument.

## What the charts show

- **CPU, system scope:** total utilisation of the machine, 0–100%.
- **CPU, process scope:** top-style, **100% = one fully busy core**, so a
  multi-threaded process can exceed 100%. The axis starts at 0–100% and grows
  in 100% steps (up to `cores × 100%`) when the process uses more.
- **RAM, system scope:** used memory (total − available), axis 0 → total RAM.
- **RSS, process scope:** resident set size, axis scaled to the peak in view.
- **X axis:** time relative to now (`-10m … now`). The view slides as time passes.

### Aggregation

Samples are grouped into time buckets so the range fits the chart width:
`bucket = range / columns`, rounded up to a sensible step (1s, 2s, 5s, 10s,
15s, 30s, 1m, 2m, 5m, …) and never finer than the sample interval. Each bucket
plots the **maximum** of its samples (not the mean), so short spikes stay
visible even when zoomed out. Buckets are aligned to the wall clock so they don't
shimmer as the window slides. Override the size with `--agg <duration>`.

The status line shows the sample interval, bucket size and range, e.g.
`every 2s · bucket 10s max · 10m`. When `--since` asks for more than the daemon
has stored, the range is capped and the status line says `only 37m stored`.

Live mode samples every second. History mode uses the daemon's 2s samples and
re-reads the daemon's file each second, so it doesn't sample separately.

## The daemon

`pmon -d` double-forks into the background and samples every 2s:

- total CPU % and used/total RAM;
- CPU % and RSS of every process **owned by the current user**.

It writes to `$XDG_STATE_HOME/pmon/` (default `~/.local/state/pmon/`):

| File          | Contents                                                |
| ------------- | ------------------------------------------------------- |
| `pmon.pid`    | pid of the running daemon; `flock`ed while it runs      |
| `system.ring` | system samples (32-byte records)                        |
| `procs.ring`  | per-process samples (48-byte records)                   |
| `daemon.log`  | stderr of the background daemon (normally empty)        |

The pidfile is locked for the daemon's lifetime, so a second `pmon -d` refuses
to start, and a stale pidfile left by a crash doesn't block a restart.

Typical cost is ~2 MB RSS and well under 1% of one core.

### Retention

`--retain` (default `1h`) sets the number of slots in the ring files. The
process ring is sized for twice the number of your processes at startup (at
least 128 per sample); if you later run many more, per-process history covers a
shorter span than the system history (`pmon --status` shows both). Restarting
the daemon with the same `--retain` keeps existing history; changing it
recreates the files.

For 1h with ~100 processes the process ring is 1800 samples × 256 slots × 48
bytes ≈ 22 MB at most; it is a sparse file, so only the slots actually used
take disk space.

### Running under systemd

Instead of `pmon -d`, run it as a user service with
[`contrib/pmon.service`](contrib/pmon.service), which uses
`pmon --daemon --foreground`:

```bash
mkdir -p ~/.config/systemd/user
cp contrib/pmon.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now pmon.service
```

`pmon --status` and `pmon -H` work the same either way. Stop it with
`systemctl --user stop pmon` (or `pmon --stop`; the unit only restarts on failure).

## File format

Both ring files share a 64-byte little-endian header:

```
 0  magic "PMONRING"   8  version u32   12  record size u32   16  capacity u64
24  begin count u64   32  end count u64   40  interval ms u32
```

followed by `capacity` fixed-size records; record *n* is stored in slot
*n mod capacity*. The writer bumps `begin` before writing a batch and `end`
after, so readers can discard any slot that was overwritten while they were
reading. Records:

- `system.ring`: `ts_ms u64, cpu f32, pad u32, mem_used u64, mem_total u64`
- `procs.ring`: `ts_ms u64, start_time u64, rss u64, pid u32, cpu f32, name [u8;16]`

Processes are identified by **pid + start time**, so a reused pid never merges
with an older process.
