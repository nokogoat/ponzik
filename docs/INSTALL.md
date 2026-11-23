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

The first time it starts, Ponzik offers to **add itself to the applications
menu**, or to **just run** as it is (tick "Don't ask again" to keep it that
way). Adding it to the menu writes, for your user only, in
`~/.local/share` (or `$XDG_DATA_HOME`):

| File | What it is |
| --- | --- |
| `applications/ponzik.desktop` | The menu entry; it starts the AppImage where it is |
| `icons/hicolor/256x256/apps/ponzik.png`, `icons/hicolor/scalable/apps/ponzik.svg` | The icon |
| `mime/packages/ponzik.xml` | The file types `application/x-ponzik` (`*.ponzik`) and `application/x-flp` (FL Studio's `*.flp`) |

then refreshes the desktop's caches with `update-desktop-database`,
`update-mime-database` and `gtk-update-icon-cache` when they are installed.
If the AppImage sits in Downloads, Ponzik offers to move it to `~/Applications`
first (it then restarts from there). Nothing is written outside your home.

The menu entry follows the AppImage: after an update its version is updated,
and if you move the AppImage, the entry points to the new place the next time
you start it from there. **Settings › About** shows whether the menu starts
this AppImage, with **Add to menu** and **Remove from menu**. Removing it takes
away the entry, the icons and the file types; the AppImage and your projects
stay.

The AppImage bundles no library: Ponzik draws its own interface and talks to
X11 over its socket, so the only libraries it loads are ones every desktop
distribution has:

- `libasound.so.2` (ALSA; with PipeWire or PulseAudio, sound goes through them),
- the C library (`libc`, `libm`, `libgcc_s`), glibc 2.35 or newer.

It runs on X11, and on Wayland through XWayland. Running an AppImage normally
uses FUSE (`fusermount`, usually already installed). Without it, start it with
`./Ponzik-x86_64.AppImage --appimage-extract-and-run`.

## Windows

Download `Ponzik-windows-x86_64.exe` and run it. Windows may warn that the
program is from an unknown publisher because it is not signed: choose "More
info", then "Run anyway".

The first screen offers two choices:

- **Install Ponzik** (recommended). Ponzik installs itself, for your user only
  and without administrator rights; there is no separate installer program:
  - the program goes to `%LOCALAPPDATA%\Programs\Ponzik\Ponzik.exe`;
  - a shortcut is added to the Start menu, and to the desktop if the box is ticked;
  - `.ponzik` projects open with Ponzik, and Ponzik is added to the "Open
    with" list of FL Studio's `.flp` files (it becomes their default program
    only if no other program, FL Studio for instance, claimed them);
  - Ponzik appears in **Settings › Apps › Installed apps** (publisher, version,
    size), from where it can be uninstalled.

  Ponzik then starts again from its new place. The downloaded file is no longer
  needed and can be deleted.
- **Just run it**: nothing is installed, Ponzik runs from the downloaded file.
  Tick "Don't ask again" to stop the question. It can be installed later from
  **Settings › About**.

The portable zip (`Ponzik-windows-x86_64.zip`) has a file named `portable`
next to `Ponzik.exe`: that copy never offers to install itself. Keep the
folder somewhere you can write to (not `C:\Program Files`) so that it can update
itself. Delete the `portable` file to be offered the install again.

Everything the install writes is under your user:

| Where | What |
| --- | --- |
| `HKCU\Software\Classes\.ponzik`, `Ponzik.Project`, `Ponzik.FlpProject` | File types and their icon |
| `HKCU\Software\Classes\.flp\OpenWithProgids` | Ponzik in the "Open with" list of `.flp` files |
| `HKCU\Software\Classes\Applications\Ponzik.exe` | Ponzik's name in "Open with" |
| `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Ponzik` | The "Installed apps" entry (`UninstallString` is `Ponzik.exe --uninstall`) |

Shortcuts are `.lnk` files made with the shell's own `IShellLinkW` object.

### Uninstalling on Windows

Choose **Uninstall** in Settings › Apps › Installed apps, or in Ponzik's
**Settings › About**. Ponzik asks first, then removes its shortcuts, its file
types, its "Installed apps" entry and the program itself (the program file and
its folder are deleted a couple of seconds after Ponzik closes, by a hidden
`cmd` that waits for it). Your projects (in `Music\Ponzik`) always stay. Your
settings (`%APPDATA%\Ponzik`: themes, shortcuts, translations) stay too,
unless you tick "Also remove my settings".

## From the command line

```sh
ponzik install [--desktop]           # Windows: install for this user (--desktop: a desktop shortcut)
                                     # Linux: add the AppImage to the applications menu
ponzik uninstall [--yes] [--purge]   # remove it again (--purge: the settings too)
ponzik <file.ponzik | file.flp>      # open a project
```

`ponzik uninstall` asks for confirmation unless `--yes` is given.
`Ponzik.exe --uninstall --quiet` uninstalls on Windows without any window.

For a try from source, `PONZIK_INSTALL=welcome` shows the first screen in a
build that would not show it otherwise.

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
   (deleted at the next start) and the new one takes its name. The installed
   copy updates the same way, in its install folder; at its next start it
   writes its new version in the "Installed apps" entry (`DisplayVersion`);
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
