# Linux packaging notes

## Dev install (required for `orchestrator run` / `profile add --scope window`'s
## hotkey registration to work at all)

The KDE `GlobalShortcuts` portal requires the calling app's id to match an
installed `.desktop` file (see
`docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md`
§1b). Until this is packaged into a real `.deb`, install it manually once:

```
mkdir -p ~/.local/share/applications
cp packaging/linux/io.github.barebones-sh.Orchestrator.desktop ~/.local/share/applications/
kbuildsycoca6   # or: update-desktop-database ~/.local/share/applications
```

Real `.deb` packaging (installing this file system-wide as part of a
package) is a separate, future increment.
