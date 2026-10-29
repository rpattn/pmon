mod agg;
mod app;
mod daemon;
mod picker;
mod sampler;
mod store;
mod util;

use clap::Parser;
use std::time::Duration;

fn duration(s: &str) -> Result<Duration, String> {
    let d = humantime::parse_duration(s).map_err(|e| e.to_string())?;
    if d.is_zero() {
        return Err("duration must be greater than zero".into());
    }
    Ok(d)
}

/// Minimal task-manager-style CPU and RAM monitor for the terminal.
#[derive(Parser, Debug)]
#[command(name = "pmon", version)]
struct Cli {
    /// Scope the charts to one process. Without a value, opens a fuzzy picker;
    /// with a name or pid, skips the picker when the match is unambiguous.
    #[arg(short, long, value_name = "NAME|PID", num_args = 0..=1)]
    process: Option<Option<String>>,

    /// Show history recorded by the daemon, then keep following it live.
    #[arg(short = 'H', long)]
    history: bool,

    /// How far back to show (e.g. 30s, 5m, 1h). With -H it is capped at what
    /// the daemon has stored. [default: 10m with -H, 2m live]
    #[arg(long, value_name = "DURATION", value_parser = duration)]
    since: Option<Duration>,

    /// Aggregation bucket size. [default: automatic, fits the chart width]
    #[arg(long, value_name = "DURATION", value_parser = duration)]
    agg: Option<Duration>,

    /// Run the history daemon in the background (samples every 2s).
    #[arg(short, long, conflicts_with_all = ["process", "history", "since", "agg", "stop", "status"])]
    daemon: bool,

    /// With -d: how much history to keep. [default: 1h]
    #[arg(long, value_name = "DURATION", value_parser = duration, requires = "daemon")]
    retain: Option<Duration>,

    /// With -d: stay in the foreground (for `systemd --user`).
    #[arg(long, requires = "daemon")]
    foreground: bool,

    /// Stop the running daemon.
    #[arg(long, conflicts_with_all = ["process", "history", "since", "agg", "status"])]
    stop: bool,

    /// Report whether the daemon is running and how much history is stored.
    #[arg(long, conflicts_with_all = ["process", "history", "since", "agg"])]
    status: bool,
}

fn main() {
    // Exit quietly on a closed pipe (`pmon --status | head`) instead of panicking.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let cli = Cli::parse();
    let res = if cli.stop {
        daemon::stop()
    } else if cli.status {
        daemon::status()
    } else if cli.daemon {
        daemon::start(cli.retain.unwrap_or(daemon::DEFAULT_RETAIN), cli.foreground)
    } else {
        app::run(app::Options { history: cli.history, process: cli.process, since: cli.since, agg: cli.agg })
    };
    std::process::exit(match res {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pmon: {e:#}");
            1
        }
    });
}
