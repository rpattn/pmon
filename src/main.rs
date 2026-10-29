mod agg;
mod app;
mod completions;
mod configure;
mod daemon;
mod picker;
mod sampler;
mod settings;
mod store;
mod theme;
mod util;

use clap::{CommandFactory, Parser};
use settings::{Charset, GraphStyle, Settings, ThemeName};
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
#[command(
    name = "pmon",
    version,
    after_help = "Display settings: pmon --config (or --style, --charset, --theme, --top)"
)]
struct Cli {
    /// Chart one process; opens a picker unless NAME|PID is unambiguous
    #[arg(short, long, value_name = "NAME|PID", num_args = 0..=1)]
    process: Option<Option<String>>,

    /// Show the daemon's history, then follow it live
    #[arg(short = 'H', long)]
    history: bool,

    /// How far back to show, e.g. 30s, 5m, 1h [default: 2m, 10m with -H]
    #[arg(long, value_name = "DURATION", value_parser = duration)]
    since: Option<Duration>,

    /// Bucket size [default: fit to chart width]
    #[arg(long, value_name = "DURATION", value_parser = duration)]
    agg: Option<Duration>,

    /// Choose graph style, characters, theme and top lines, with a live preview
    #[arg(long, conflicts_with_all = ["process", "history", "since", "agg", "daemon", "stop", "status"])]
    config: bool,

    // Display settings; usually set once via --config, so kept out of --help.
    #[arg(long, value_name = "STYLE", hide = true)]
    style: Option<GraphStyle>,
    #[arg(long, value_name = "SET", hide = true)]
    charset: Option<Charset>,
    #[arg(long, value_name = "THEME", hide = true)]
    theme: Option<ThemeName>,
    #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "5", hide = true)]
    top: Option<usize>,

    /// Start the history daemon (samples every 2s)
    #[arg(short, long, conflicts_with_all = ["process", "history", "since", "agg", "stop", "status"])]
    daemon: bool,

    /// With -d: how much history to keep [default: 1h]
    #[arg(long, value_name = "DURATION", value_parser = duration, requires = "daemon")]
    retain: Option<Duration>,

    /// With -d: don't detach (for systemd --user / Task Scheduler)
    #[arg(long, requires = "daemon", hide = true)]
    foreground: bool,

    /// Stop the daemon
    #[arg(long, conflicts_with_all = ["process", "history", "since", "agg", "status"])]
    stop: bool,

    /// Show whether the daemon runs and how much history is stored
    #[arg(long, conflicts_with_all = ["process", "history", "since", "agg"])]
    status: bool,

    /// Print a shell completion script (see README). Hidden, and hidden flags
    /// are left out of the script, so `--c<TAB>` completes to `--config`.
    #[arg(long, value_name = "SHELL", hide = true, exclusive = true)]
    completions: Option<clap_complete::Shell>,

    /// Set up Tab completion now (normally automatic on first run).
    #[arg(long, value_name = "SHELL", hide = true, exclusive = true, num_args = 0..=1)]
    install_completions: Option<Option<clap_complete::Shell>>,
}

/// The command without hidden flags; completion generators include hidden
/// flags, which would make `--c<Tab>` ambiguous.
fn visible_command() -> clap::Command {
    let full = Cli::command();
    clap::Command::new("pmon")
        .about(full.get_about().cloned().unwrap_or_default())
        .version(env!("CARGO_PKG_VERSION"))
        .args(full.get_arguments().filter(|a| !a.is_hide_set()).cloned())
}

/// Command-line display flags win over the config file.
fn overrides(mut s: Settings, cli: &Cli) -> Settings {
    s.style = cli.style.unwrap_or(s.style);
    s.charset = cli.charset.unwrap_or(s.charset);
    s.theme = cli.theme.unwrap_or(s.theme);
    s.top = cli.top.unwrap_or(s.top).min(settings::MAX_TOP);
    s
}

fn main() {
    // Exit quietly on a closed pipe (`pmon --status | head`) instead of panicking.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut visible_command(), "pmon", &mut std::io::stdout());
        return;
    }
    let res = if let Some(shell) = cli.install_completions {
        completions::install(shell, visible_command())
    } else if cli.stop {
        daemon::stop()
    } else if cli.status {
        daemon::status()
    } else if cli.daemon {
        daemon::start(cli.retain.unwrap_or(daemon::DEFAULT_RETAIN), cli.foreground)
    } else if cli.config {
        let installed = completions::auto(visible_command());
        // A broken config file shouldn't lock you out of the tool that fixes it.
        let (settings, note) = match Settings::load() {
            Ok(s) => (s, None),
            Err(e) => {
                let cause = e.root_cause().to_string();
                let cause = cause.split(" (expected").next().unwrap_or_default();
                (Settings::default(), Some(format!("config.toml: {cause}; saving replaces it")))
            }
        };
        let note = note.or(installed);
        configure::run(overrides(settings, &cli), note)
    } else {
        Settings::load().and_then(|settings| {
            let settings = overrides(settings, &cli);
            let flash = completions::auto(visible_command());
            app::run(app::Options { history: cli.history, process: cli.process, since: cli.since, agg: cli.agg, settings, flash })
        })
    };
    std::process::exit(match res {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pmon: {e:#}");
            1
        }
    });
}
