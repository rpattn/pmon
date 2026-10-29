# Releasing and packaging pmon

Everything hangs off GitHub releases: the release workflow builds
binaries for six targets, and every installer (winget, Scoop, cargo-binstall)
just downloads those assets.

## One-time setup

1. Push the repo to GitHub and replace `OWNER` in `Cargo.toml`, `README.md`
   and `packaging/scoop/pmon.json` with your GitHub user or org.
2. Optionally set your name in `LICENSE` (it says "pmon contributors").
3. Reserve the crate name (it was free when this was written):
   `cargo login` then `cargo publish`.

## Each release

```bash
# bump version in Cargo.toml, then:
cargo publish
git tag v0.2.0 && git push origin v0.2.0
```

The tag triggers `.github/workflows/release.yml`, which builds:

| Target | Asset |
| ------ | ----- |
| `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` | `pmon-vX.Y.Z-<target>.zip` |
| `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (static) | `.tar.gz` |
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | `.tar.gz` |

plus `SHA256SUMS`, and publishes them as a GitHub release.
`cargo binstall pmon` finds them via `[package.metadata.binstall]`.

## winget

The first submission is manual; after that CI does it.

1. After the first release, on Windows:
   ```powershell
   winget install wingetcreate
   wingetcreate new https://github.com/rpattn/pmon/releases/download/v0.1.0/pmon-v0.1.0-x86_64-pc-windows-msvc.zip
   ```
   Choose package identifier `YourName.pmon`, installer type **portable** inside
   the zip (`pmon-v0.1.0-x86_64-pc-windows-msvc\pmon.exe`), command alias
   `pmon`, and let it open the PR against `microsoft/winget-pkgs`.
2. Microsoft's validation pipeline installs and scans it; a human moderator
   merges it, usually within a few days.
3. To automate later releases, add a repo variable `WINGET_IDENTIFIER`
   (`YourName.pmon`) and a secret `WINGET_TOKEN` (classic PAT, `public_repo`
   scope). The `winget` job in the release workflow then opens the update PR.

Users then run `winget install pmon` (or `winget install YourName.pmon`).

## Scoop

No review needed. After each release, fill in the version and hashes in
`packaging/scoop/pmon.json` (the hashes are in `SHA256SUMS`), commit, and users
can run:

```powershell
scoop install https://raw.githubusercontent.com/rpattn/pmon/master/packaging/scoop/pmon.json
```

The manifest has `checkver`/`autoupdate`, so if you later move it into a
dedicated bucket repo (`scoop-pmon`), Scoop's tooling (or the
[Excavator](https://github.com/ScoopInstaller/Excavator) action) keeps it
current automatically, and users run `scoop bucket add pmon <url>` then
`scoop install pmon`.

## SmartScreen and signing

"Windows protected your PC" only appears for files a browser downloaded
(they carry a Mark-of-the-Web). winget, Scoop and cargo don't add that mark, so
installs through them run without the prompt. Signing only matters if many
people download the zip by hand. The cheap options are Azure Trusted Signing
(a small monthly fee) or SignPath's free plan for open-source projects.

## conda-forge / pixi

Possible via a recipe PR to `conda-forge/staged-recipes`; then
`pixi global install pmon` works. It's slower to set up and reaches fewer
Windows users than winget/Scoop, so it's not worth doing first.
