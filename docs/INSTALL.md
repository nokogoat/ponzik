# Installing and updating Ponzik

Ready-to-run builds are published on the
[GitHub Releases page](https://github.com/nokogoat/ponzik/releases):

| Release | What it is |
| --- | --- |
| **nightly** (pre-release) | Rebuilt on every change to `main`. Its files are replaced each time. |
| **v0.x.y** | Tagged versions, published when a version is tagged. |

Each release has the same files:

| File | For |
| --- | --- |
| `Ponzik-x86_64.AppImage` | Linux, x86_64 |
| `Ponzik-windows-x86_64.exe` | Windows 10 and 11, x86_64: the program alone |
| `Ponzik-windows-x86_64.zip` | The same program in a portable folder, with the license and changelog |
| `SHA256SUMS` | SHA-256 checksums of the files above |

## Linux

```sh
chmod +x Ponzik-x86_64.AppImage
./Ponzik-x86_64.AppImage
```

The AppImage bundles no library: Ponzik draws its own interface and talks to
X11 over its socket, so the only libraries it loads are ones every desktop
distribution has:

- `libasound.so.2` (ALSA; with PipeWire or PulseAudio, sound goes through them),
- the C library (`libc`, `libm`, `libgcc_s`), glibc 2.35 or newer.

It runs on X11, and on Wayland through XWayland. Running an AppImage normally
uses FUSE (`fusermount`, usually already installed). Without it, start it with
`./Ponzik-x86_64.AppImage --appimage-extract-and-run`.

## Windows

Download `Ponzik-windows-x86_64.exe` (or unzip the portable folder) and run it.
Windows may warn that the program is from an unknown publisher because it is
not signed: choose "More info", then "Run anyway". Keep the program in a folder
you can write to (Downloads, Desktop, a folder of your own, not
`C:\Program Files`), so that it can update itself.

## How updates work

When Ponzik starts, it asks GitHub in the background whether a newer build
exists on its channel. Nothing about you is sent: it is one request to the
public releases API (`api.github.com/repos/nokogoat/ponzik/releases`), made by
the system's own tools (`curl`, or `wget` on Linux; `curl.exe` or PowerShell on
Windows). Offline, nothing happens.

If a newer build exists, a message offers **Install & restart**. Ponzik then:

1. downloads the new AppImage or `.exe` to a temporary file;
2. checks its size and its SHA-256 against the release's `SHA256SUMS`, and stops
   if they differ;
3. puts it in place of the running one: on Linux, the AppImage file is replaced
   in one step; on Windows, the running `.exe` is renamed to `Ponzik.exe.old`
   (deleted at the next start) and the new one takes its name;
4. saves your project if it has changes, then restarts on the new version.

After an update, the **What's new** window shows the newest section of the
[changelog](CHANGELOG.md) once. Tick "Don't show again for this version" (it is
ticked by default) and it stays closed until the next version. It is always
available from the main menu, the command palette (Ctrl+K) and
**Settings › About**.

**Settings › About** also shows the exact build (version, build number, commit
and date) and has the update settings:

- **Check at startup**: on by default.
- **Install automatically**: off by default. When on, updates download and
  install in the background; the new version runs at the next start.
- **Channel**: *Nightly* (every change) or *Stable* (tagged versions only).

The same settings live in `settings.json` (`update_check`, `update_install`,
`update_channel`). From a terminal, `ponzik --version` prints the build and
`ponzik check-update [nightly|stable]` asks GitHub for the latest release.

### Builds from source

A build made with `cargo build` reports the channel `dev`: it does not check
for updates at startup (set `PONZIK_UPDATE_CHECK=1` to make it), and since it is
not an AppImage it cannot replace itself; an available update opens the
releases page instead.

### Private forks

The updater reads public releases without any account. A private fork needs a
GitHub token with read access to its contents, in the `PONZIK_GITHUB_TOKEN`
environment variable or as `update_token` in `settings.json`; the token is
passed to `curl` on its standard input, never on the command line.

## Building the packages yourself

```sh
scripts/build-appimage.sh          # dist/Ponzik-x86_64.AppImage
```

The script builds the release program, checks with `ldd` that it only needs the
libraries listed above, assembles the AppDir (program, `.desktop` file, icons)
and runs `appimagetool` (downloaded into `dist/tools` if it is not installed).

On Windows, `cargo build --release` embeds the icon and version information
when a resource compiler is available (`rc.exe` from the Windows SDK, or
`llvm-rc`). The workflow `.github/workflows/release.yml` does both on every
push and publishes the results.

The icons are generated from the Ponzik mark drawn in code:
`cargo run -p ponzik-ui --example app_icon -- assets/icons`.
