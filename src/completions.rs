//! Tab completion that sets itself up.
//!
//! Shells that load completion scripts from a directory (bash with
//! bash-completion, fish, zsh with oh-my-zsh) get pmon's script written there
//! silently and refreshed when pmon's version changes. Shells that need a line
//! in a startup file (plain zsh, PowerShell, elvish, bash without
//! bash-completion) are asked once. Either way it happens only on interactive
//! runs, the answer is remembered in the state dir, and a script the user
//! deleted is not put back. `PMON_NO_COMPLETIONS=1` turns all of it off.

use crate::store;
use anyhow::{Context, Result, bail};
use clap::Command;
use clap_complete::Shell;
use std::collections::BTreeMap;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const STATE_FILE: &str = "completions";
/// Marks the line pmon adds to startup files.
const RC_TAG: &str = "# pmon tab completion";

fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default()
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute())
}

fn display(p: &Path) -> String {
    let h = home();
    match p.strip_prefix(&h) {
        Ok(rest) if !h.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => p.display().to_string(),
    }
}

/// The shell that ran pmon: the parent process if it is a shell, else
/// `$SHELL`. Also returns the executable name (pwsh vs powershell matters).
fn detect() -> Option<(Shell, String)> {
    let from_name = |name: &str| -> Option<Shell> {
        let name = name.trim_start_matches('-').to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        match name {
            "bash" => Some(Shell::Bash),
            "zsh" => Some(Shell::Zsh),
            "fish" => Some(Shell::Fish),
            "elvish" => Some(Shell::Elvish),
            "pwsh" | "powershell" => Some(Shell::PowerShell),
            _ => None,
        }
    };
    let parent = (|| {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
        let mut sys = System::new().ok()?;
        let me = sysinfo::get_current_pid().ok()?;
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[me]), false, ProcessRefreshKind::nothing());
        let ppid = sys.process(me)?.parent()?;
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[ppid]), false, ProcessRefreshKind::nothing());
        Some(sys.process(ppid)?.name().to_string_lossy().into_owned())
    })();
    if let Some(name) = parent
        && let Some(shell) = from_name(&name)
    {
        return Some((shell, name));
    }
    let shell = std::env::var("SHELL").ok()?;
    let name = Path::new(&shell).file_name()?.to_str()?.to_string();
    Some((from_name(&name)?, name))
}

fn bash_completion_installed() -> bool {
    [
        "/usr/share/bash-completion/bash_completion",
        "/etc/bash_completion",
        "/usr/local/share/bash-completion/bash_completion",
        "/opt/homebrew/etc/profile.d/bash_completion.sh",
        "/usr/local/etc/profile.d/bash_completion.sh",
    ]
    .iter()
    .any(|p| Path::new(p).exists())
}

/// The directory-loaded script location, for shells set up that way. Cheap:
/// no processes are started.
fn drop_in(shell: Shell) -> Option<PathBuf> {
    let h = home();
    match shell {
        Shell::Bash if bash_completion_installed() => {
            let base = env_dir("BASH_COMPLETION_USER_DIR")
                .or_else(|| env_dir("XDG_DATA_HOME").map(|d| d.join("bash-completion")))
                .unwrap_or_else(|| h.join(".local/share/bash-completion"));
            Some(base.join("completions/pmon"))
        }
        Shell::Fish => Some(env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| h.join(".config")).join("fish/completions/pmon.fish")),
        // oh-my-zsh puts this directory on $fpath for tools like pmon.
        Shell::Zsh => {
            let zsh = env_dir("ZSH").filter(|z| z.join("oh-my-zsh.sh").exists())?;
            Some(env_dir("ZSH_CACHE_DIR").unwrap_or_else(|| zsh.join("cache")).join("completions/_pmon"))
        }
        _ => None,
    }
}

/// The startup file and line for shells without a drop-in directory.
fn rc_line(shell: Shell, exe: &str) -> Result<(PathBuf, String)> {
    let h = home();
    Ok(match shell {
        Shell::Bash => (h.join(".bashrc"), format!("command -v pmon >/dev/null && source <(pmon --completions bash)  {RC_TAG}")),
        Shell::Zsh => (
            env_dir("ZDOTDIR").unwrap_or(h).join(".zshrc"),
            format!(
                "if (( $+commands[pmon] )); then (( $+functions[compdef] )) || {{ autoload -Uz compinit && compinit; }}; \
                 source <(pmon --completions zsh); fi  {RC_TAG}"
            ),
        ),
        Shell::Elvish => (
            env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| h.join(".config")).join("elvish/rc.elv"),
            format!("eval (pmon --completions elvish | slurp)  {RC_TAG}"),
        ),
        Shell::PowerShell => (
            powershell_profile(exe)?,
            format!(
                "if (Get-Command pmon -ErrorAction SilentlyContinue) {{ pmon --completions powershell | Out-String | Invoke-Expression }}  {RC_TAG}"
            ),
        ),
        _ => bail!("{shell} is not supported"),
    })
}

/// Asks PowerShell for `$PROFILE` (Documents may be redirected, e.g. to
/// OneDrive) and refuses if scripts can't run, since the profile wouldn't load.
fn powershell_profile(exe: &str) -> Result<PathBuf> {
    let exe = match exe.trim_end_matches(".exe") {
        e @ ("pwsh" | "powershell") => e,
        _ if which("pwsh") => "pwsh",
        _ => "powershell",
    };
    let out = std::process::Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command", "$PROFILE.CurrentUserCurrentHost; Get-ExecutionPolicy"])
        .output()
        .with_context(|| format!("running {exe}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let (Some(profile), Some(policy)) = (lines.next(), lines.next()) else {
        bail!("could not read the PowerShell profile path");
    };
    if matches!(policy, "Restricted" | "AllSigned") {
        bail!(
            "PowerShell's execution policy ({policy}) won't run profile scripts; \
             run `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned` first"
        );
    }
    Ok(PathBuf::from(profile))
}

fn which(cmd: &str) -> bool {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd"] } else { &[""] };
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| exts.iter().any(|e| d.join(format!("{cmd}{e}")).is_file())))
}

fn script(shell: Shell, cmd: &mut Command) -> Vec<u8> {
    let mut buf = format!("# pmon {VERSION} completions, generated by pmon and updated automatically.\n").into_bytes();
    clap_complete::generate(shell, cmd, "pmon", &mut buf);
    buf
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("pmon-tmp");
    fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

/// Appends `line` to `rc` unless pmon's line is already there.
fn add_rc_line(rc: &Path, line: &str) -> Result<bool> {
    let existing = fs::read_to_string(rc).unwrap_or_default();
    if existing.contains("pmon --completions") {
        return Ok(false);
    }
    if let Some(dir) = rc.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let sep = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(rc)
        .and_then(|mut f| writeln!(f, "{sep}{line}"))
        .with_context(|| format!("updating {}", rc.display()))?;
    Ok(true)
}

/// `shell -> "installed <version>" | "declined"`, one per line.
fn load_state() -> BTreeMap<String, String> {
    fs::read_to_string(store::state_dir().join(STATE_FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

fn save_state(state: &BTreeMap<String, String>) {
    let text: String = state.iter().map(|(k, v)| format!("{k} {v}\n")).collect();
    let _ = write_atomic(&store::state_dir().join(STATE_FILE), text.as_bytes());
}

/// Called on interactive start-up. Returns a message worth showing, if any.
pub fn auto(mut cmd: Command) -> Option<String> {
    if std::env::var_os("PMON_NO_COMPLETIONS").is_some_and(|v| !v.is_empty() && v != "0") {
        return None;
    }
    let (shell, exe) = detect()?;
    let key = shell.to_string();
    let mut state = load_state();
    let known = state.get(&key).cloned();
    if known.as_deref().is_some_and(|k| !k.starts_with("installed")) {
        return None; // declined, or couldn't be set up
    }
    if let Some(path) = drop_in(shell) {
        let current = format!("installed {VERSION}");
        match known.as_deref() {
            Some(_) if !path.exists() => return None, // the user removed it
            Some(k) if k == current => return None,
            _ => {}
        }
        write_atomic(&path, &script(shell, &mut cmd)).ok()?;
        state.insert(key, current);
        save_state(&state);
        let done = if known.is_some() { "updated" } else { "installed" };
        return Some(format!("Tab completion for {shell} {done} (new shells)"));
    }

    // Startup-file shells: ask once. The line loads the live script, so there
    // is nothing to refresh later.
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if known.is_some() || !interactive {
        return None;
    }
    let (rc, line) = match rc_line(shell, &exe) {
        Ok(v) => v,
        Err(e) => {
            println!("pmon: Tab completion not set up: {e:#}");
            state.insert(key, "unavailable".into());
            save_state(&state);
            return None;
        }
    };
    print!("pmon can turn on Tab completion for {shell} by adding one line to {}.\nDo it now? [Y/n] ", display(&rc));
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).ok()?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes") {
        state.insert(key, "declined".into());
        save_state(&state);
        println!("OK, won't ask again (`pmon --install-completions` does it later).");
        return None;
    }
    match add_rc_line(&rc, &line) {
        Ok(_) => {
            state.insert(key, format!("installed {VERSION}"));
            save_state(&state);
            println!("Done: Tab completion works in new {shell} sessions.");
        }
        Err(e) => println!("pmon: couldn't update {}: {e:#}", display(&rc)),
    }
    None
}

/// `pmon --install-completions [SHELL]`: install now, whatever was decided before.
pub fn install(shell: Option<Shell>, mut cmd: Command) -> Result<i32> {
    let detected = detect();
    let exe = detected.as_ref().map(|d| d.1.clone()).unwrap_or_default();
    let Some(shell) = shell.or(detected.map(|d| d.0)) else {
        bail!("couldn't tell which shell you use; name it, e.g. `pmon --install-completions zsh`");
    };
    let mut state = load_state();
    if let Some(path) = drop_in(shell) {
        write_atomic(&path, &script(shell, &mut cmd))?;
        println!("Installed {shell} completion: {}", display(&path));
    } else {
        let (rc, line) = rc_line(shell, &exe)?;
        if add_rc_line(&rc, &line)? {
            println!("Added Tab completion for {shell} to {}", display(&rc));
        } else {
            println!("{} already loads pmon's completion", display(&rc));
        }
    }
    state.insert(shell.to_string(), format!("installed {VERSION}"));
    save_state(&state);
    println!("Open a new {shell} session to use it.");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc_line_is_added_once_and_keeps_existing_content() {
        let dir = std::env::temp_dir().join(format!("pmon-rc-test-{}", std::process::id()));
        let rc = dir.join("sub/.zshrc");
        fs::create_dir_all(rc.parent().unwrap()).unwrap();
        fs::write(&rc, "alias ll='ls -l'").unwrap(); // no trailing newline
        assert!(add_rc_line(&rc, "source <(pmon --completions zsh)").unwrap());
        assert!(!add_rc_line(&rc, "source <(pmon --completions zsh)").unwrap());
        assert_eq!(fs::read_to_string(&rc).unwrap(), "alias ll='ls -l'\nsource <(pmon --completions zsh)\n");
        fs::remove_dir_all(dir).unwrap();
    }
}
