# Wayland injection spike

Throwaway exploratory binary — own workspace, not a member of the product's
Cargo workspace (see `docs/superpowers/specs/2026-08-09-orchestrator-design.md`
§6). Read
`docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md` for the
actual findings; this file only covers how to re-run the phases.

## One-time setup

1. **`input` group** (for `ydotool`/uinput):
   ```
   sudo usermod -aG input "$USER"
   ```
   Takes effect on next login. To test in the same session without logging
   out, run commands needing it via `sg input -c '...'`.

2. **`ydotoold`** (Linux user service, disabled by default on Kubuntu):
   ```
   systemctl --user enable --now ydotool.service
   ```
   or, to run it manually for a quick test (bypasses the systemd user
   manager's possibly-stale group cache):
   ```
   sg input -c ydotoold &
   ```

3. **A `.desktop` file matching the app id**, required by
   `org.freedesktop.host.portal.Registry.Register` for both
   `GlobalShortcuts` and `RemoteDesktop` portals to accept a non-sandboxed
   app:
   ```
   mkdir -p ~/.local/share/applications
   cat > ~/.local/share/applications/org.orchestrator.Spike.desktop <<'EOF'
   [Desktop Entry]
   Type=Application
   Name=Orchestrator Spike
   Exec=true
   NoDisplay=true
   EOF
   ```

## Running each phase

```
cargo run --bin spike -- kglobalaccel-raw          # dead end, see findings doc
cargo run --bin spike -- portal-shortcuts           # watch for a KDE dialog first run
cargo run --bin spike -- kwin-windows               # lists windows, prompts to activate one
cargo run --bin spike -- activate-inject-restore <index>   # combo test
cargo run --bin spike -- inject-portal-libei        # watch for a KDE permission dialog
```

All phases print progress to stdout; several require a human to watch the
screen (dialogs, focus, flicker) and/or press a physical key — none of this
is automatable headlessly.
