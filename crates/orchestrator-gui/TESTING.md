# Manual Testing

There is no automated end-to-end test for this app yet — run this by hand
after any change to the profile management UI.

Run from `/home/hb/src/orchestrator/crates/orchestrator-gui`:

```bash
# 0. Confirm clean starting state
rm -f ~/.config/orchestrator/config.json   # only if it exists and you don't need it
npm run tauri dev
```

Wait for the app window ("orchestrator-gui") to appear. It should show
"Orchestrator" / "Profiles" / "No profiles configured yet." / an "Add
profile" button.

**1. Add a Desktop-scope Repeat profile**
- Click **Add profile**.
- Name: `test-desktop`
- Scope: leave on **Desktop** (default).
- Action: leave on **Repeat** (default). Input kind: **Key combo**. Check
  the **Ctrl** modifier box. Key: `F9`. Interval (ms): `250`. Jitter: type
  `50`.
- Debounce: type `500`.
- Click **Add profile** (submit).
- *Expected:* form closes, list shows one row: `test-desktop | Desktop |
  Repeat every 250ms (±50ms) | debounce=500ms`, with Edit/Remove buttons.
- *Expected on disk* (`cat ~/.config/orchestrator/config.json`): one
  profile named `test-desktop`, `"scope": "Desktop"`, `"action": {"Repeat":
  {"input": {"KeyPress": {"modifiers": ["Ctrl"], "key": "F9"}},
  "interval_ms": 250, "jitter": {"Uniform": {"range_ms": 50}}}}`,
  `"debounce_ms": 500`.

**2. Edit it**
- Click **Edit** on the `test-desktop` row.
- *Expected:* form pre-fills: Name field shows `test-desktop` and is
  disabled/greyed out (not editable), Ctrl is checked, Key is `F9`,
  Interval is `250`, Jitter is `50`, Debounce is `500`.
- Change Interval to `100`, clear the Jitter field entirely (leave blank),
  click **Save changes**.
- *Expected:* list row updates to `Repeat every 100ms (no jitter)`.
- *Expected on disk:* `interval_ms: 100`, `"jitter": "None"` for that
  profile — and this step is the one that exercises the bug this task
  fixed (clearing the Jitter field used to throw a TypeError on submit;
  it should now submit cleanly).

**3. Add a Window-scope profile via the picker**
- Open some other window first (e.g. a terminal) so the picker has
  something to list.
- Click **Add profile**.
- Name: `test-window`
- Scope: click **Window**. Wait for "Loading windows..." to resolve into a
  list of radio options (`<process> — <title>`). If the list is empty,
  click **Refresh windows**.
- Select any window from the list. *Expected:* a "Selected: `<process>` —
  `<title>`" hint line appears below the list.
- Action: leave on Repeat, Input kind **Scroll**, dx `0`, dy `-1`,
  Interval `100`.
- Click **Add profile**.
- *Expected:* list now shows two rows; `test-window`'s scope summary reads
  `Window(<process_name>)`.
- *Expected on disk:* a second profile with `"scope": {"Window":
  {"process_name": "...", "window_title_hint": "...", "backend_hint_id":
  "..."}}` matching the window you picked, and `"action": {"Repeat":
  {"input": {"Scroll": {"dx": 0, "dy": -1}}, ...}}`.

**4. Edit the Window-scope profile — covers the bug fixed after this
report was first written**
- Click **Edit** on the `test-window` row.
- *Expected immediately on open, with no clicks yet:* the Scope section is
  already on **Window**, and the window picker is already populated with
  the live window list (not "No windows currently open.") — you should
  **not** need to click "Refresh windows" to see it. A "Selected: `<process>`
  — `<title>`" hint should already show the previously-picked window. (This
  is the exact path a bug lived in earlier: the picker used to stay empty
  here until a manual refresh, even though the selection hint was correct.)
- Change the selection to a different window in the list, if more than one
  is available (skip this specific click if only one window is open — the
  "already populated" check above is the important part). Change Interval
  to `150`.
- Click **Save changes**.
- *Expected:* list row updates, scope summary still reads
  `Window(<process_name>)` (updated if you picked a different window).
- *Expected on disk:* that profile's `interval_ms` is now `150`, and (if
  you changed the selection) `scope.Window.process_name` /
  `window_title_hint` / `backend_hint_id` reflect the newly-picked window.

**5. Remove both**
- Click **Remove** on `test-desktop`. *Expected:* row disappears
  immediately; on disk, only `test-window` remains.
- Click **Remove** on `test-window`. *Expected:* list shows "No profiles
  configured yet." again; on disk, `"profiles": []`.

**6. Clean up**
- Stop `npm run tauri dev` (Ctrl+C).
- Confirm `~/.config/orchestrator/config.json` has `"profiles": []` (or
  delete the file/directory entirely if it didn't exist before you
  started) — leave the machine as it was found.

Optional additional coverage if you want to go further than the brief's
minimum: try a **Macro** action (add a couple of steps with **Add step**,
mix Key combo and Scroll kinds, toggle **Loop**, remove a step with
**Remove step**) and confirm the JSON shape (`"action": {"Macro": {"steps":
[...], "loop": true}}`) on disk; try leaving the Debounce field blank on
add and confirm it defaults to `400` on disk.
