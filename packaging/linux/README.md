# Linux packaging notes

This file documents the manual, from-source install path. A packaged `.deb`
(see the repo root README's "Install via `.deb`" section) handles both of
these steps for you — this file is only relevant if you're building from
source.

## Dev install (required for `orchestrator run` / `profile add --scope window`'s
## hotkey registration to work at all)

The KDE `GlobalShortcuts` portal requires the calling app's id to match an
installed `.desktop` file (see
`docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md`
§1b). For a from-source build, install it manually once:

```
mkdir -p ~/.local/share/applications
cp packaging/linux/io.github.barebonessh.Orchestrator.desktop ~/.local/share/applications/
kbuildsycoca6   # or: update-desktop-database ~/.local/share/applications
```

**Also required, found the hard way during live verification:** the portal's
app-info lookup does not just check that the `.desktop` file exists — it
also requires the file's `Exec=` command to resolve to a real executable on
`$PATH`. `Exec=orchestrator` will not be found (and the whole `.desktop`
file will be silently treated as nonexistent, with the same "App info not
found" error a missing file produces) unless an `orchestrator` binary is
actually on `$PATH`. For a local dev build, symlink the built binary in:

```
ln -sf "$(pwd)/target/debug/orchestrator" ~/.local/bin/orchestrator   # assumes ~/.local/bin is on $PATH
```

A real `.deb` install would put the binary in `/usr/bin/` directly, so this
step is dev-only — but without it, `run` and `profile add --scope window`
fail with a confusing "App info not found" error that looks identical to a
missing/uninstalled `.desktop` file, even when the file is correctly
installed. (Two independent, real gaps were found here: the hyphen in an
earlier app-id draft, since fixed, and this `$PATH` requirement — either
one alone is enough to produce the exact same error message.)

See the repo root README for `.deb` install instructions.

## Autostart (dev builds)

`orchestrator service enable` writes a user-level systemd unit at
`~/.config/systemd/user/orchestrator.service` when no `.deb`-installed
vendor unit is present at `/usr/lib/systemd/user/orchestrator.service` --
pointing `ExecStart` at whatever binary you actually ran `enable` from
(`target/debug/orchestrator` or `target/release/orchestrator`). If you
rebuild to a different profile (debug vs. release) after enabling, re-run
`orchestrator service enable` to refresh the unit (it only writes one if
none exists yet -- delete the old one first, or `service disable` then
`rm ~/.config/systemd/user/orchestrator.service` before re-enabling with
the new build).
