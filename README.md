# pmon

A minimal task-manager-style monitor for the terminal (Linux, Windows, macOS): two live line
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

## Install

| Platform | Command |
| -------- | ------- |
| Windows (winget) | `winget install pmon` |
| Windows (Scoop)  | `scoop install https://raw.githubusercontent.com/OWNER/pmon/main/packaging/scoop/pmon.json` |
| Any, with Rust   | `cargo install pmon` |
| Any, prebuilt via [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) | `cargo binstall pmon` |
| Manual | download a zip / tarball from the [releases page](https://github.com/OWNER/pmon/releases) |

The package-manager routes need a published release first; see
[PACKAGING.md](PACKAGING.md).

From a checkout:

```bash
cargo install --path .
```

Building from source on Windows needs Rust from [rustup](https://rustup.rs)
plus the "Desktop development with C++" Build Tools it prompts for (for the
MSVC linker); everything else is pure Rust.

### Shell completion

Tab completion sets itself up the first time you run pmon interactively, so
`pmon --c<Tab>` completes to `--config` with no setup:

- **bash** (with bash-completion, the default on most distros), **fish**, and
  **zsh with oh-my-zsh** load completion scripts from a directory. pmon writes
  its script there silently and refreshes it when pmon is updated.
- **Plain zsh, PowerShell, elvish** (and bash without bash-completion) need a
  line in a startup file (`~/.zshrc`, `$PROFILE`, …). pmon asks once
  (`[Y/n]`) before adding it and remembers the answer.

It only happens for the TUI and `--config`, never for the daemon or scripts.
If you delete the installed script, pmon leaves it deleted. To opt out, set
`PMON_NO_COMPLETIONS=1`. To set it up later (e.g. after answering no), run
`pmon --install-completions [bash|zsh|fish|powershell|elvish]`, or print the
script yourself with `pmon --completions <shell>`. Open a new shell afterwards.

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

pmon --config             # pick style, characters, theme, top lines (live preview)
pmon --top                # one-off: overlay the 5 busiest processes (--top 3 for 3)
pmon --style area --charset halfblock --theme nord
```

Durations use `humantime` syntax: `30s`, `5m`, `1h`, `1h30m`, `2days`.

### Keys

| Key            | Action                                           |
| -------------- | ------------------------------------------------ |
| `q`, `Ctrl-C`  | quit                                             |
| `/`            | open the process picker                          |
| `Esc`          | back to system-wide (or close the picker)        |
| `?`            | help overlay showing keys and current settings   |
| `t`            | top-process lines on/off                         |
| `+` / `-`      | more / fewer top-process lines (1–8)             |
| `s`            | cycle graph style                                |
| `g`            | cycle character set                              |
| `c`            | cycle theme                                      |
| `w`            | save current style / characters / theme / top as defaults |
| picker: type   | fuzzy filter (name, pid, command line)           |
| picker: `↑ ↓`, `Ctrl-P/N`, `PgUp/PgDn` | move                     |
| picker: `Enter`| select · `Ctrl-U` clear · `Ctrl-W` delete word   |
| picker: `Delete` | kill the selected process (asks first: `Enter` kill, `f` force) |

**Killing processes.** In the picker, `Delete` asks for confirmation, then
`Enter` sends SIGTERM or `f` sends SIGKILL; on Windows both terminate the
process. pmon first checks that the pid still belongs to the same process
(pid + start time), so a reused pid is never hit. If the process exits it drops
out of the list; if it ignores SIGTERM it stays selected and the footer
suggests `Delete` then `f`. Errors such as "Operation not permitted" are shown
in the footer.

`pmon -p NAME|PID` selects directly when the argument is an existing pid, a
unique exact process name, or has exactly one fuzzy match. With `-H`, if several
recorded processes match but only one is still running, that one is chosen.
Otherwise the picker opens pre-filled with the argument.

## Display options

The easiest way to set these is `pmon --config`: a settings list with preview
charts (animated sample data, so every style has something to show) that
update as you change values. `↑↓` chooses a setting, `←→` changes it, `Enter`
saves, `r` resets, `Esc` cancels.

Inside pmon you can also change them with a key and save with `w`, or pass a
flag for one run. The flags are left out of `--help` to keep it short.

| Option | Values | Key |
| ------ | ------ | --- |
| `--style` | `line` (default), `area` (filled), `bars`, `dots` | `s` |
| `--charset` | `braille` (default), `octant`, `sextant`, `quadrant`, `halfblock`, `block`, `dot`, `ascii` | `g` |
| `--theme` | `default`, `nord`, `gruvbox`, `dracula`, `solarized`, `light`, `mono` | `c` |
| `--top [N]` | top-process lines, N = 1–8 (default 5), `0` = off | `t`, `+`, `-` |

- **Character sets:** `braille` and `octant` give the finest lines (2×4 dots
  per cell). `octant` and `sextant` need a recent font (e.g. Cascadia Code or
  Iosevka) and otherwise show as `�`. `halfblock` and `quadrant` look solid
  and work in most fonts. `block`, `dot` and `ascii` work everywhere, which
  makes them the safe choice for the old Windows console or minimal fonts.
- **Themes** set only foreground colours, so your terminal's background stays.
  `default` uses the terminal's own 16-colour palette; the named themes use
  24-bit colour. `light` is for light backgrounds. `mono` uses no colour and
  is the default when the `NO_COLOR` environment variable is set.
- **Top processes** (system view only): the CPU chart gets a line for each of
  the N processes with the highest average CPU over the visible window, and
  the RAM chart one for each of the N with the highest average memory. A
  colour key with current values sits in each chart's bottom border, and a
  process keeps the same colour in both charts. In the `area` style the processes
  are drawn as stacked filled bands instead of lines (the busiest at the
  bottom, the total's colour filling the rest), since lines drawn over a fill
  would punch holes in it. The stack is capped at the total, because RSS
  double-counts shared memory. CPU lines are scaled to the
  whole machine like the total (one busy core on an 8-core machine is 12.5%),
  so they always sit at or under the total line. In live mode, lines start when
  you turn them on; with `-H` they come from the daemon's history, which covers
  only your own processes.

### Config file

Defaults live in `~/.config/pmon/config.toml` (`$XDG_CONFIG_HOME` is
honoured; `%APPDATA%\pmon\config.toml` on Windows). Pressing `w` writes it;
you can also edit it by hand:

```toml
style = "area"        # line, area, bars, dots
charset = "halfblock" # braille, octant, sextant, quadrant, halfblock, block, dot, ascii
theme = "nord"        # default, nord, gruvbox, dracula, solarized, light, mono
top = 5               # top-process lines in the system view, 0 = off (max 8)
```

Command-line flags override the file for that run.

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

`pmon -d` starts a detached copy of itself (`pmon --daemon --foreground`) and
waits until it is up, so start-up errors are still printed. It samples every 2s:

- total CPU % and used/total RAM;
- CPU % and RSS of every process **owned by the current user**.

It writes to `$XDG_STATE_HOME/pmon/` (default `~/.local/state/pmon/`), or
`%LOCALAPPDATA%\pmon\` on Windows:

| File          | Contents                                                     |
| ------------- | ------------------------------------------------------------ |
| `pmon.lock`   | held with an exclusive lock for the daemon's whole lifetime  |
| `pmon.pid`    | pid of the running daemon                                    |
| `system.ring` | system samples (32-byte records)                             |
| `procs.ring`  | per-process samples (64-byte records)                        |
| `daemon.log`  | output of the background daemon (normally just a start line) |
| `stop`        | created by `pmon --stop`; the daemon exits when it sees it   |

Because "running" means "someone holds the lock", a second `pmon -d` refuses to
start, and files left behind by a crash never block a restart. `--stop` works
the same way on every OS (the daemon checks for the `stop` file every 250 ms);
Ctrl-C and SIGTERM also stop a foreground daemon cleanly.

Typical cost is ~4 MB RSS on Linux (~13 MB working set on Windows, which lists
many more per-user processes) and well under 1% of one core.

### Retention

`--retain` (default `1h`) sets the number of slots in the ring files. The
process ring is sized for twice the number of your processes at startup (at
least 128 per sample); if you later run many more, per-process history covers a
shorter span than the system history (`pmon --status` shows both). Restarting
the daemon with the same `--retain` keeps existing history; changing it
recreates the files.

For 1h with ~100 processes the process ring is 1800 samples × 256 slots × 64
bytes ≈ 29 MB at most. On Linux and macOS it is a sparse file, so only the
slots actually used take disk space; on Windows the full size is allocated up
front.

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

### Starting at logon on Windows

Register a Task Scheduler task that runs `pmon -d` when you log in:

```powershell
schtasks /Create /SC ONLOGON /TN pmon /TR "\"$((Get-Command pmon).Source)\" --daemon" /F
```

Remove it with `schtasks /Delete /TN pmon /F`. A console window may flash
briefly at logon while `pmon -d` launches the detached daemon.

### Windows notes

- Use Windows Terminal (or any terminal with a font that has Braille
  characters, e.g. Cascadia); the legacy console's default fonts draw the
  chart lines as boxes.
- Process names include `.exe`; `pmon -p notepad` matches `notepad.exe`.
- Under WSL, the Linux build monitors the WSL VM, not Windows. Use the Windows
  build to see Windows itself.

## File format

Both ring files share a 64-byte little-endian header:

```
 0  magic "PMONRING"   8  version u32 (2)   12  record size u32   16  capacity u64
24  begin count u64   32  end count u64   40  interval ms u32
```

followed by `capacity` fixed-size records; record *n* is stored in slot
*n mod capacity*. The writer bumps `begin` before writing a batch and `end`
after, so readers can discard any slot that was overwritten while they were
reading. Records:

- `system.ring`: `ts_ms u64, cpu f32, pad u32, mem_used u64, mem_total u64`
- `procs.ring`: `ts_ms u64, start_time u64, rss u64, pid u32, cpu f32, name [u8;32]`

Processes are identified by **pid + start time**, so a reused pid never merges
with an older process.
