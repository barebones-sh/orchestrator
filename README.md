# Orchestrator

Orchestrator is a global-hotkey-triggered input automation tool for KDE
Plasma/Wayland (macOS support planned). It lets you define profiles that bind
a hotkey to a repeated key/scroll input or a multi-step macro, scoped either
to the whole desktop or to a specific window, and runs them via the
Wayland-native portals (`GlobalShortcuts` for hotkey capture, `ydotool` for
input injection) rather than relying on legacy X11-only automation APIs.

## Build from source

```
git clone https://github.com/barebones-sh/orchestrator.git
cd orchestrator
cargo build --release
```

The binary lands at `target/release/orchestrator`. The project pins Rust
`1.97.1` via `rust-toolchain.toml`; if you use `rustup`, the correct
toolchain is installed and selected automatically when you build from the
repo root.

## Install via `.deb`

Download the `.deb` matching your architecture (`amd64` or `arm64`) from a
[GitHub Release](https://github.com/barebones-sh/orchestrator/releases),
then:

```
sudo apt install ./orchestrator-cli_<version>_<arch>.deb
```

This installs the `orchestrator` binary to `/usr/bin/` and its `.desktop`
file to `/usr/share/applications/`, and pulls in `ydotool` as a dependency.
You still need to complete step 1 of "Linux one-time setup" below (the
`input` group / `ydotoold` step) — that part isn't done by the package.

## Linux one-time setup

Step 1 below is required either way (from-source build or `.deb` install) —
`ydotool` being installed doesn't put the current user in the `input` group
or start its daemon. Step 2 is only needed for a from-source build; a `.deb`
install already places the `.desktop` file where the portal expects it.

1. **Add yourself to the `input` group and enable `ydotoold`.** Input
   injection happens through `ydotool`, which needs access to `/dev/uinput`
   via its `ydotoold` daemon:

   ```
   sudo usermod -aG input "$USER"   # takes effect on next login
   systemctl --user enable --now ydotool.service
   ```

   Group membership must be active *before* `ydotoold` starts, since it
   opens `/dev/uinput` itself at startup — re-login (or use `sg input -c
   '...'` to pick up the new group in the current session) before enabling
   the service if you added yourself to the group just now.

2. **Install the app's `.desktop` file.** KDE's `GlobalShortcuts` portal
   only allows hotkey registration for an app whose id matches an installed
   `.desktop` file. From a source build, install it manually:

   ```
   mkdir -p ~/.local/share/applications
   cp packaging/linux/io.github.barebonessh.Orchestrator.desktop ~/.local/share/applications/
   kbuildsycoca6   # or: update-desktop-database ~/.local/share/applications
   ```

   See `packaging/linux/README.md` for the full detail on this step,
   including a `$PATH`/`Exec=` gotcha that produces a confusing "App info
   not found" error if missed.

## Basic usage

Add a profile that repeats a key combo while the hotkey is held, scoped to
the whole desktop:

```
orchestrator profile add --name jiggler --scope desktop --action repeat \
    --input key:Ctrl+Alt+J --interval-ms 5000
```

Then run all configured profiles until Ctrl-C:

```
orchestrator run
```

Use `orchestrator profile list`, `profile edit`, and `profile remove` to
manage profiles afterward.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache License, Version
2.0](LICENSE-APACHE) at your option.
