# .deb Packaging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Orchestrator actually installable via `.deb` on x86_64 and arm64, closing a hard requirement from the original project brief and, as a side effect, eliminating the two manual dev-only setup steps (`.desktop` file install, `$PATH` symlink) that this session's live verification found the hard way.

**Architecture:** Real project metadata (license, README, `Cargo.toml` package fields) that `cargo-deb` derives its Debian control file from — no second, hand-maintained metadata source to drift out of sync. A `[package.metadata.deb]` block on `orchestrator-cli` declaring `ydotool` as a runtime dependency and bundling the `.desktop` file as a packaged asset. A tag-triggered release workflow building both architectures on native GitHub-hosted runners (reusing the existing CI workflow's proven "native runners, no cross-compilation" approach) and uploading both `.deb`s to a GitHub Release via the `gh` CLI (already present on GitHub-hosted runners, authenticated via the automatic `GITHUB_TOKEN` — no third-party release-upload action needed).

**Tech Stack:** `cargo-deb` 3.7.0 (already researched/pinned during the project's first increment), GitHub Actions, `gh` CLI.

**Spec:** None — implements the original project brief's stated packaging requirement (".deb packaging for x86_64 and arm64 via cargo-deb or manual control file") plus the specific design confirmed with the human partner directly in conversation (MIT OR Apache-2.0 license, cargo-deb, tag-triggered release workflow, native arm64 runners matching the existing CI workflow). No separate design doc was written since there was no remaining product ambiguity — same precedent as the prior CI plan.

## Global Constraints

- License: `MIT OR Apache-2.0` (the standard Rust-ecosystem dual-license convention) — add both `LICENSE-MIT` and `LICENSE-APACHE` at the repo root, standard boilerplate text for each.
- Packaging tool: `cargo-deb`, version `3.7.0` (already pinned during the project's first increment's toolchain research — do not pick a different version without checking whether that research is stale).
- Only `orchestrator-cli` (the `orchestrator` binary) gets packaged. `orchestrator-gui` remains an unpackaged placeholder — do not add packaging metadata to it.
- The `.deb`'s `depends` field must include `ydotool` (the one real runtime dependency this project's live verification discovered this session — see `docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md` and `packaging/linux/README.md`).
- The `.deb` must install `packaging/linux/io.github.barebonessh.Orchestrator.desktop` to `/usr/share/applications/` as a packaged asset (not left as a manual dev-only step for real installs — this is new, real value this packaging work adds beyond what existed before).
- Multi-arch via `cargo deb --target <triple>` (cargo-deb's own documented mechanism), built on **native** runners for each arch (`ubuntu-latest` for x86_64, `ubuntu-24.04-arm` for arm64) — matching the existing `.github/workflows/ci.yml`'s already-proven, already-approved "native runners, no cross-compilation" choice. Do not introduce `cross`, QEMU, or any cross-compilation toolchain.
- The release workflow triggers only on pushing a tag matching `v*` — not on every push to `main` (that's what `ci.yml` is for).
- Upload built `.deb`s to a GitHub Release using the `gh` CLI directly (`gh release create` / `gh release upload`), authenticated via the workflow's automatic `${{ secrets.GITHUB_TOKEN }}` — do not add a third-party release-upload GitHub Action as a new dependency when the built-in `gh` CLI already does this.
- Do not push any tag, create any release, or otherwise trigger the new workflow as part of executing this plan — implementers commit locally only. Actually cutting a release (pushing a `v*` tag) is a separate, explicit decision the controller confirms with the human partner after this plan's review is complete — same "ask before the outward-facing action" pattern as the CI workflow's push.
- Do not touch `orchestrator-core`/`orchestrator-hotkey`/`orchestrator-input`/`orchestrator-window`'s `Cargo.toml` package metadata — this plan only touches `orchestrator-cli` and the repo root.

---

## Task 1: Project metadata (LICENSE, README, Cargo.toml package fields)

**Files:**
- Create: `LICENSE-MIT`
- Create: `LICENSE-APACHE`
- Create: `README.md` (repo root)
- Modify: `crates/orchestrator-cli/Cargo.toml`

**Interfaces:**
- Produces: `license = "MIT OR Apache-2.0"`, `description`, `repository`, `authors` fields on `orchestrator-cli`'s `[package]` section — consumed by Task 2's `cargo-deb` invocation, which derives the Debian control file's own metadata from these fields.

This task has no code to test in the usual sense — its "test" is that the resulting files are present, correctly formatted, and that `cargo metadata`/`cargo build` still succeed with the new `Cargo.toml` fields (a malformed `Cargo.toml` addition would break the build immediately, which is the actual verification signal here).

- [ ] **Step 1: Add the two license files**

Create `LICENSE-MIT` at the repo root with the standard MIT license text (substitute the copyright line with `Copyright (c) 2026 the Orchestrator contributors` or check `git log` for the actual author name/email to use — this repo's git config, per `git log`, shows commits authored as "Huge" — use whatever name/format the existing commit history actually uses, don't invent a different one).

Create `LICENSE-APACHE` at the repo root with the standard Apache License 2.0 full text (this is a long, fixed, well-known document — reproduce it verbatim and completely, do not abbreviate or summarize it; if you have live internet access via WebFetch, fetch the canonical text from `https://www.apache.org/licenses/LICENSE-2.0.txt` rather than reconstructing it from memory, since an inaccurate reproduction of a legal document is worse than none).

- [ ] **Step 2: Write the root README**

Create `README.md` at the repo root covering, at minimum:
- One-paragraph description of what Orchestrator is (a global-hotkey-triggered input automation tool for KDE Plasma/Wayland, with macOS support planned).
- Build from source: `git clone <repo-url> && cd orchestrator && cargo build --release` — the binary lands at `target/release/orchestrator`. State the pinned toolchain version (read `rust-toolchain.toml` yourself to get the exact current value — do not hardcode a version without checking the file first).
- Linux one-time setup, in this exact order, each with the *why*, drawing directly from `packaging/linux/README.md` (read that file yourself — do not restate it from memory, since it may have details specific to what was actually verified live this session):
  1. `input` group membership + `ydotoold` (needed for input injection).
  2. The `.desktop` file requirement for the KDE portal to allow hotkey registration (note: this step becomes unnecessary once installed via a real `.deb` package — Task 2 of this same plan bundles the `.desktop` file into the package — but is still needed for a from-source build).
- Basic usage: `orchestrator profile add ...` (one concrete example, e.g. a simple `--scope desktop --action repeat` profile) and `orchestrator run`.
- A line noting the project is licensed under `MIT OR Apache-2.0`, pointing at the two `LICENSE-*` files.

Keep this README a genuinely useful entry point, not a restatement of every design doc in `docs/superpowers/specs/` — link out to `packaging/linux/README.md` for the full Linux setup detail rather than duplicating it at length.

- [ ] **Step 3: Add package metadata to `orchestrator-cli/Cargo.toml`**

Read the current full file yourself first. Add to its `[package]` section (do not touch `[[bin]]`, `[dependencies]`, or the target-cfg blocks):

```toml
license = "MIT OR Apache-2.0"
description = "Global-hotkey-triggered input automation for KDE Plasma/Wayland"
repository = "https://github.com/barebones-sh/orchestrator"
authors = ["<use whatever the existing git log commit author name is>"]
```

Confirm the exact `authors` value by checking `git log --format='%an <%ae>' | sort -u` yourself rather than guessing — use the actual name(s)/email(s) this repo's commits are authored as.

- [ ] **Step 4: Verify nothing broke**

Run: `cargo build --workspace` and `cargo metadata --format-version 1 -q | python3 -c "import json,sys; d=json.load(sys.stdin); pkg=[p for p in d['packages'] if p['name']=='orchestrator-cli'][0]; print(pkg['license'], pkg['description'], pkg['repository'])"`
Expected: build succeeds unchanged; the metadata query prints the three values you just added, confirming Cargo parsed them correctly.

- [ ] **Step 5: Commit**

```bash
git add LICENSE-MIT LICENSE-APACHE README.md crates/orchestrator-cli/Cargo.toml
git commit -m "docs: add project license, README, and package metadata"
```

---

## Task 2: cargo-deb configuration + local build verification

**Files:**
- Modify: `crates/orchestrator-cli/Cargo.toml`

**Interfaces:**
- Consumes: Task 1's `[package]` metadata fields (license/description/repository), the existing `packaging/linux/io.github.barebonessh.Orchestrator.desktop` file (already committed in an earlier increment).
- Produces: a `[package.metadata.deb]` block that Task 3's release workflow invokes via `cargo deb --target <triple>`.

- [ ] **Step 1: Install cargo-deb locally**

Run: `cargo install cargo-deb --version 3.7.0 --locked`
Expected: installs successfully. If version `3.7.0` is no longer available on crates.io (versions occasionally get yanked) or a materially newer version is clearly the actively-maintained current release, install the latest available version instead and note in your report that you deviated from the plan's pinned version, with your reasoning — don't silently install a different version without saying so.

- [ ] **Step 2: Add the `[package.metadata.deb]` block**

Read the current full `crates/orchestrator-cli/Cargo.toml` yourself first (Task 1 already added `[package]` metadata fields — build on top of that, don't recreate the file). Add:

```toml
[package.metadata.deb]
maintainer-scripts = "packaging/linux/"
depends = "ydotool"
assets = [
    ["target/release/orchestrator", "usr/bin/", "755"],
    ["packaging/linux/io.github.barebonessh.Orchestrator.desktop", "usr/share/applications/", "644"],
]
```

Verify this exact structure against `cargo-deb`'s actual current documentation (its README, or `cargo deb --help` once installed) rather than assuming the field names/shapes above are still current — `cargo-deb`'s asset-list format and metadata-key names have changed across versions before. If `maintainer-scripts` isn't a real field for the version you installed (this project has no pre/post-install scripts to run, so it may not be needed at all — only include it if you find you actually need it for something concrete, don't add unused config), drop it. The two required outcomes — bundling the built binary at `/usr/bin/orchestrator` and the `.desktop` file at `/usr/share/applications/`, with `ydotool` as a runtime dependency — are load-bearing per this task's brief; the exact TOML shape achieving them should match whatever your installed `cargo-deb` version actually expects.

- [ ] **Step 3: Build a real `.deb` locally and inspect it**

Run:
```bash
cargo build --release -p orchestrator-cli
cargo deb -p orchestrator-cli --no-build
```
(`--no-build` reuses the release binary you just built rather than having `cargo-deb` rebuild it redundantly — check this flag's exact current name against your installed version's `--help` output, adjust if it differs.)

Expected: produces a `.deb` file (likely under `target/debian/`). Inspect it:
```bash
dpkg-deb --info target/debian/*.deb
dpkg-deb --contents target/debian/*.deb
```
Expected: the control file shows the license/description/`ydotool` dependency from Task 1/this task's metadata; the contents listing shows `usr/bin/orchestrator` and `usr/share/applications/io.github.barebonessh.Orchestrator.desktop` present at the right paths with the right permissions (executable bit on the binary).

If `dpkg-deb` isn't available in this environment, note that in your report and use whatever equivalent inspection is available (e.g. `ar t target/debian/*.deb` plus extracting `control.tar.*`/`data.tar.*` manually) — the goal is confirming the package's actual contents, not the specific tool used to look.

- [ ] **Step 4: Commit**

```bash
git add crates/orchestrator-cli/Cargo.toml
git commit -m "feat: add cargo-deb packaging configuration"
```

Do not commit the built `.deb` file itself or the `target/` directory (already covered by the existing `.gitignore` — confirm this with `git status` before committing, it should show nothing under `target/`).

---

## Task 3: Tag-triggered release workflow

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: Task 2's working `cargo deb -p orchestrator-cli --target <triple>` invocation (verified locally in Task 2 for the host architecture only — this task exercises it for both architectures for real, in CI, which is the first time the arm64 leg of packaging is actually verified at all).

- [ ] **Step 1: Write the release workflow**

Read `.github/workflows/ci.yml` yourself first for the established toolchain-setup pattern (the `dtolnay/rust-toolchain` step shape, `Swatinem/rust-cache` usage) and reuse it exactly rather than reinventing it.

Create `.github/workflows/release.yml`:

```yaml
name: Release

on:
  push:
    tags:
      - "v*"

permissions:
  contents: write

jobs:
  create-release:
    name: create-release
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Create GitHub Release
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: gh release create "${{ github.ref_name }}" --generate-notes

  package:
    name: package (${{ matrix.name }})
    needs: create-release
    strategy:
      fail-fast: false
      matrix:
        include:
          - name: x86_64
            os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
          - name: arm64
            os: ubuntu-24.04-arm
            target: aarch64-unknown-linux-gnu
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@master
        with:
          toolchain: "1.97.1"
          targets: ${{ matrix.target }}

      - uses: Swatinem/rust-cache@v2

      - name: Install cargo-deb
        run: cargo install cargo-deb --version 3.7.0 --locked

      - name: Build release binary
        run: cargo build --release --target ${{ matrix.target }} -p orchestrator-cli

      - name: Build .deb package
        run: cargo deb -p orchestrator-cli --target ${{ matrix.target }} --no-build

      - name: Upload to release
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: gh release upload "${{ github.ref_name }}" target/${{ matrix.target }}/debian/*.deb
```

Adjust the `cargo deb`/build command flags (`--no-build`, the exact output path under `target/<triple>/debian/`) to match whatever Task 2 actually confirmed works for the installed `cargo-deb` version, rather than assuming this plan's guess is exactly right — Task 2 already did this verification for the host architecture; carry its findings into this workflow rather than re-deriving them.

Verify the toolchain-pin approach (literal `"1.97.1"` string) still matches `rust-toolchain.toml`'s current value — read the file, don't assume it hasn't changed since the CI plan was written.

- [ ] **Step 2: Validate the YAML is well-formed**

Run:
```bash
python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/release.yml')); print('YAML OK')"
```
Expected: `YAML OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: add tag-triggered release workflow building and publishing .deb packages"
```

Do **not** push a tag or trigger this workflow — that is a separate, explicit decision the controller confirms with the human partner after this plan's review is complete.

---

## Plan-Level Verification

- Task 1: `cargo build --workspace` succeeds; the `cargo metadata` query in Task 1 Step 4 confirms the new fields parsed correctly.
- Task 2: a real `.deb` was built locally and its contents were inspected and confirmed correct (binary + `.desktop` file present, `ydotool` dependency declared).
- Task 3: the release workflow YAML is well-formed and its steps were derived from Task 2's actually-verified `cargo-deb` invocation, not guessed independently.
- No tag was pushed, no release was created, no workflow run was triggered as part of executing this plan.
- Design-intent check: license is `MIT OR Apache-2.0` everywhere it's declared (both license files present, `Cargo.toml`'s `license` field, README's mention); only `orchestrator-cli` was touched, never `orchestrator-gui` or the trait crates; native runners only in the release workflow, matching `ci.yml`'s precedent; `gh` CLI used for release creation/upload, no new third-party action dependency added for that purpose.
