# pmon

A minimal task-manager-style monitor for the terminal (Linux, Windows, macOS):
live charts of **CPU**, **RAM**, **disk**, **network** and **GPU**, each with a
stats box (now / average / peak and the top consumers), for the whole system or
for one process. An optional background daemon records history so you can look
back.

```
┌ CPU 5.5% · system ─────────────────────────────┐┌ RAM 2.01 GiB of 15.5 GiB (13%) · system ───────┐
│ 100%│                                          ││15.5G│                                          │
│     │                                          ││     │                                          │
│     │                                          ││     │                                          │
│     │                                          ││     │                                          │
│     │                                          ││     │                                          │
│     │                                          ││     │                                          │
│  50%│                                          ││ 7.7G│                                          │
│     │                                          ││     │                                          │
│     │                                          ││     │                                          │
│     │                                          ││     │                                          │
│     │                                          ││     │                ⠐⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒│
│   0%│                ⢀⣀⣀⡠⠤⠤⠔⠒⠊⠉⠉⠑⠒⠒⠒⠒⠉⠉⠉⠉⠒⠒⠒⠒⠉⠉││    0│                ⠠⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤│
│     └──────────────────────────────────────────││     └──────────────────────────────────────────│
│  -10s                    -5s                now││  -10s                    -5s                now│
└ ■ yes 5.0%  ■ MainThread 0.1%  ────────────────┘└ ■ MainThread 456 MiB  ■ 2.1.304-332dfe 336 MiB ┘
┌ Disk R 0 B/s  W 12.0 KiB/s · system ───────────┐┌ Net ↓ 172 B/s  ↑ 172 B/s · system ─────────────┐
│  50K│                                          ││   5K│                                          │
│     │                                          ││     │                                          │
│     │                                          ││     │                                          │
│     │                                          ││     │                        ⢀⢆                │
│     │                     ⡀                    ││     │                        ⡸⠘⡄               │
│  25K│                    ⢰⢱                    ││ 2.5K│                       ⢀⠇ ⢣               │
│     │                   ⢀⠇ ⢇                   ││     │                       ⡜  ⠈⡆              │
│     │                   ⡜  ⠘⡄                  ││     │                      ⢠⠃   ⠸⡀             │
│     │                  ⢰⠁   ⢱                ⢀⠔││     │                      ⡎     ⢣             │
│     │                 ⢀⠇     ⢇              ⡠⠊ ││     │                     ⢰⠁     ⠘⡄            │
│    0│                ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀││    0│                ⢀⣀⣀⡠⠤⠎⠤⠔⠒⠒⠒⠢⠤⠱⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤│
│     └──────────────────────────────────────────││     └──────────────────────────────────────────│
│  -10s                    -5s                now││  -10s                    -5s                now│
└ ■ read  ■ write  ──────────────────────────────┘└ ■ received  ■ sent  ───────────────────────────┘
 LIVE  every 1s · bucket 1s max · 10s │ q quit / pick Esc reset ? help                              
```

## Install

| Platform | Command |
| -------- | ------- |
| Windows (winget) | `winget install pmon` |
| Windows (Scoop)  | `scoop install https://raw.githubusercontent.com/rpattn/pmon/master/packaging/scoop/pmon.json` |
| Any, with Rust   | `cargo install pmon` |
| Any, prebuilt via [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) | `cargo binstall pmon` |
| Manual | download a zip / tarball from the [releases page](https://github.com/rpattn/pmon/releases) |

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

pmon --config             # pick panels, layout, style, theme… with a live preview
pmon --inline             # draw below the prompt; the last frame stays after quitting
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
| `1`–`5`        | show/hide the CPU, RAM, disk, network, GPU panels |
| `l`            | cycle layout (your template, auto, rows, columns, grid) |
| `f`            | cycle the focus panel (full-width in an odd grid) |
| `a`            | group processes by app on/off                    |
| `i`            | stats boxes on/off                               |
| `t`            | top-process lines on/off                         |
| `+` / `-`      | more / fewer top-process lines (1–8)             |
| `s`            | cycle graph style                                |
| `g`            | cycle character set                              |
| `c`            | cycle theme                                      |
| `w`            | save the current display settings as defaults    |
| `d`            | start the history daemon (with `-H`, the status line says when it isn't running) |
| picker: type   | fuzzy filter (name, pid, command line)           |
| picker: `↑ ↓`, `Ctrl-P/N`, `PgUp/PgDn` | move                     |
| picker: `→` / `←` | expand / collapse an app to show its processes |
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
| `--panels` | any of `cpu`, `ram`, `disk`, `net`, `gpu` (default: all) | `1`–`5` |
| `--layout` | `auto` (default), `rows`, `columns`, `grid`, or a template like `"cpu / ram disk / net gpu"` | `l` |
| `--focus` | `cpu` (default), `ram`, `disk`, `net`, `gpu`, `none`: the panel given the full-width row in an odd grid | `f` |
| `--group` | `on` (default) / `off`: combine processes with the same name into one app | `a` |
| `--stats` | `on` (default) / `off` | `i` |
| `--inline [ROWS]` | draw below the prompt, default 24 rows; off by default | |
| `--style` | `line` (default), `area` (filled), `bars`, `dots` | `s` |
| `--charset` | `braille` (default), `octant`, `sextant`, `quadrant`, `halfblock`, `block`, `dot`, `ascii` | `g` |
| `--theme` | `default`, `nord`, `gruvbox`, `dracula`, `solarized`, `light`, `mono` | `c` |
| `--top [N]` | top-process lines, N = 1–8 (default 5), `0` = off | `t`, `+`, `-` |

- **Panels** appear in the order above. GPU hides itself when there's no
  readable GPU (e.g. under WSL, or without NVIDIA's driver library on Linux).
- **Layout:** `auto` stacks the panels when each gets at least 9 rows and
  otherwise uses a two-column grid. With an odd number of panels, the
  **focus** panel (CPU by default) gets the full-width top row; `focus = none`
  lets the last panel span the bottom row instead.
  For full control, write a **template**: rows separated by `/`, panels in a
  row side by side. `layout = "cpu / ram disk / net gpu"` gives CPU a
  full-width row on top. Hidden panels drop out and their row-mates widen (no
  GPU → `net` fills its row), and enabled panels the template leaves out are
  added below it. In `--config`, press `e` on the Layout row to type one with a
  live preview; `l` in the monitor cycles your template and the presets.
- **Proportions:** a panel is never taller than it looks wide (its height in
  rows is at most half its width in columns, since terminal cells are about
  twice as tall as wide). On a tall terminal the spare rows stay empty below
  the panels instead of stretching them.
- **Grouping by app** (on by default): processes with the same name, such as a
  browser's or an Electron app's many processes, count as one app (`chrome ×12`)
  in stats lists and top lines, with their CPU, memory, disk and GPU summed
  over *all* of them. The picker shows each app as one row (`▸ chrome ×12`):
  `→` expands it to its processes. Enter on an app charts the whole app, and
  Delete on an app offers to kill all its processes. `-p chrome` picks the app.
  Press `a` to switch back to one row per process.
- **Stats boxes** sit to the right of each chart when the panel is at least 64
  columns wide; narrower panels just show the chart.
- **Inline mode** (`--inline`, or `inline = 24` in the config) draws in a
  region below your prompt instead of taking over the screen. It uses only the
  rows the panels need at the current width (up to the number given). On `q` or
  Ctrl-C the last frame stays in your scrollback with a summary line
  (`pmon ran 3m · every 1s · …`), and the prompt continues below it.
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
  so they always sit at or under the total line. Pressing `t` to hide the lines keeps
  collecting in the background, so showing them again is instant. In live mode, lines start when
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
panels = "cpu,ram,disk,net,gpu"
layout = "auto"       # auto, rows, columns, grid
stats = true          # stats box beside each chart
inline = false        # or a number of rows: draw below the prompt
group = true          # combine processes with the same name into one app
focus = "cpu"         # full-width panel in an odd grid (cpu, ram, disk, net, gpu, none)
```

Command-line flags override the file for that run.

## What the charts show

- **CPU, system scope:** total utilisation of the machine, 0–100%.
- **CPU, process scope:** top-style, **100% = one fully busy core**, so a
  multi-threaded process can exceed 100%. The axis starts at 0–100% and grows
  in 100% steps (up to `cores × 100%`) when the process uses more.
- **RAM, system scope:** used memory (total − available), axis 0 → total RAM.
- **RSS, process scope:** resident set size, axis scaled to the peak in view.
- **Disk:** read and write throughput across physical disks (a device
  mounted several times counts once). With a process selected, that process's
  I/O. The axis scales to the peak in view.
- **Network:** received and sent across real interfaces. Loopback is skipped,
  and container bridges/veth pairs are listed in the stats box but not added
  to the total (their traffic also crosses a real interface). There's no
  per-process network data, so this panel stays system-wide.
- **GPU:** utilisation of the busiest GPU, plus VRAM as a share of the card
  when the GPU reports it (integrated GPUs that borrow system RAM usually
  don't). Per-process GPU use feeds the stats box and top lines. Read through
  NVIDIA's NVML on Linux and the GPU performance counters on Windows.
- **Stats box:** `now`, `avg` and `peak` over the visible range, then the top
  consumers by average (processes; interfaces for network). With `-H` the lists
  cover only your own processes, which is what the daemon records.
- **X axis:** time relative to now (`-10m … now`). The view slides as time
  passes. Live mode starts zoomed in on the data it has (10s) and widens as
  samples arrive, up to `--since` (default 2m), so the chart isn't empty at
  start.
- **Gaps:** where samples are missing (the daemon was stopped, the machine
  slept), the chart shows a gap instead of a line drawn across it. One missed
  sample is ignored, since sampling drifts slightly; two or more make a gap.

### Aggregation

Samples are grouped into time buckets so the range fits the chart width:
`bucket = range / columns`, rounded up to a sensible step (1s, 2s, 5s, 10s,
15s, 30s, 1m, 2m, 5m, …) and never finer than the sample interval. Each bucket
plots the **maximum** of its samples (not the mean), so short spikes stay
visible even when zoomed out. Buckets are aligned to the wall clock so they don't
shimmer as the window slides. Override the size with `--agg <duration>`.

The status line shows the sample interval, bucket size and range, e.g.
`every 2s · bucket 10s max · 10m`. When `--since` asks for more than the daemon
has stored, the range is capped and the status line says `only 37m of data`.

Live mode samples every second. Between samples the view is redrawn as time
moves the chart by about one dot (at most 10 times a second, so short windows
slide smoothly and a 2-minute window costs about one redraw a second). History mode uses the daemon's 2s samples and
re-reads the daemon's file each second, so it doesn't sample separately. If no
daemon is running, `-H` still shows whatever history is stored, then a gap,
then live data that pmon samples itself. Those samples are shown but not
saved, and the status line says `not recording: d starts daemon`. Once a
daemon runs (press `d`), pmon switches back to its data seamlessly.

## The daemon

`pmon -d` starts a detached copy of itself (`pmon --daemon --foreground`) and
waits until it is up, so start-up errors are still printed. It samples every 2s:

- total CPU, RAM, disk read/write, network receive/send, GPU usage and VRAM;
- CPU, RSS, disk I/O and GPU use of every process **owned by the current user**.

It writes to `$XDG_STATE_HOME/pmon/` (default `~/.local/state/pmon/`), or
`%LOCALAPPDATA%\pmon\` on Windows:

| File          | Contents                                                     |
| ------------- | ------------------------------------------------------------ |
| `pmon.lock`   | held with an exclusive lock for the daemon's whole lifetime  |
| `pmon.pid`    | pid of the running daemon                                    |
| `system.ring` | system samples (64-byte records)                             |
| `procs.ring`  | per-process samples (80-byte records)                        |
| `daemon.log`  | output of the background daemon (normally just a start line) |
| `stop`        | created by `pmon --stop`; the daemon exits when it sees it   |

Because "running" means "someone holds the lock", a second `pmon -d` refuses to
start, and files left behind by a crash never block a restart. `--stop` works
the same way on every OS (the daemon checks for the `stop` file every 250 ms);
Ctrl-C and SIGTERM also stop a foreground daemon cleanly.

Typical cost is ~4 MB RSS on Linux (~17 MB working set on Windows, which lists
many more per-user processes and reads GPU counters) and well under 1% of one
core.

### Retention

Restarting with plain `pmon -d` keeps the retention the stored history was
recorded with; only an explicit `--retain` changes it.

`--retain` (default `1h`) sets the number of slots in the ring files. The
process ring is sized for twice the number of your processes at startup (at
least 128 per sample); if you later run many more, per-process history covers a
shorter span than the system history (`pmon --status` shows both). Restarting
the daemon with the same `--retain` keeps existing history; changing it
recreates the files.

For 1h with ~100 processes the process ring is 1800 samples × 256 slots × 80
bytes ≈ 37 MB at most. On Linux and macOS it is a sparse file, so only the
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

### WSL notes

WSL shuts its Linux VM down shortly after the last terminal closes, which
stops the daemon (and WSL doesn't run systemd by default, so the unit above
won't start it). History then has a gap until the daemon runs again: press
`d` in `pmon -H`, or start it whenever you open a shell by adding this to
`~/.bashrc`:

```bash
pmon -d >/dev/null 2>&1  # start pmon's history daemon unless already running
```

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
 0  magic "PMONRING"   8  version u32 (3)   12  record size u32   16  capacity u64
24  begin count u64   32  end count u64   40  interval ms u32
```

followed by `capacity` fixed-size records; record *n* is stored in slot
*n mod capacity*. The writer bumps `begin` before writing a batch and `end`
after, so readers can discard any slot that was overwritten while they were
reading. Records:

- `system.ring` (64 bytes): `ts_ms u64, cpu f32, gpu f32, mem_used u64,
  mem_total u64, disk_read f32, disk_write f32, net_rx f32, net_tx f32,
  gpu_mem u64, gpu_mem_total u64` (rates in bytes/s)
- `procs.ring` (80 bytes): `ts_ms u64, start_time u64, rss u64, pid u32,
  cpu f32, disk_read f32, disk_write f32, gpu f32, gpu_mem_kib u32, name [u8;32]`

Processes are identified by **pid + start time**, so a reused pid never merges
with an older process.
