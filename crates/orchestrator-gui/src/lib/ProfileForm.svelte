<script>
  import { invoke } from "@tauri-apps/api/core";
  import { untrack } from "svelte";

  /**
   * @typedef {{ kind: "KeyPress" | "Scroll", modifiers: string[], key: string, dx: number, dy: number }} InputFields
   * @typedef {{ id: number, kind: "KeyPress" | "Scroll" | "Unsupported", modifiers: string[], key: string, dx: number, dy: number, delayMs: number, raw?: any }} MacroStepFields
   */

  /** @type {{ mode: "add" | "edit", initial: any | null, onSubmit: (payload: any) => Promise<void>, onCancel: () => void }} */
  let { mode, initial: initialProp, onSubmit, onCancel } = $props();

  // The parent remounts this component (via {#key}) whenever the edit
  // target changes, so `initial` only ever needs to seed local state once
  // at creation time -- read it through `untrack` to make that one-shot
  // read explicit instead of svelte-check warning that a reactive prop is
  // "only captured once" (correct here, since local edits are never meant
  // to sync back to the prop).
  const initial = untrack(() => initialProp);

  const MODIFIERS = ["Ctrl", "Shift", "Alt", "Meta"];

  // -- name -------------------------------------------------------------
  let name = $state(initial?.name ?? "");

  // -- scope --------------------------------------------------------------
  let scopeKind = $state(
    initial && initial.scope !== "Desktop" ? "Window" : "Desktop",
  );
  /** @type {{ process_name: string, window_title_hint: string, backend_hint_id: string | null } | null} */
  let windowChoice = $state(
    initial && initial.scope !== "Desktop"
      ? {
          process_name: initial.scope.Window.process_name,
          window_title_hint: initial.scope.Window.window_title_hint,
          backend_hint_id: initial.scope.Window.backend_hint_id,
        }
      : null,
  );
  /** @type {any[]} */
  let windows = $state([]);
  let windowsError = $state("");
  let windowsLoading = $state(false);

  async function loadWindows() {
    windowsLoading = true;
    windowsError = "";
    try {
      windows = await invoke("list_windows");
    } catch (e) {
      windowsError = String(e);
    } finally {
      windowsLoading = false;
    }
  }

  /** @param {"Desktop" | "Window"} kind */
  function chooseScopeKind(kind) {
    scopeKind = kind;
    if (kind === "Window" && windows.length === 0 && !windowsLoading) {
      loadWindows();
    }
  }

  /** @param {any} w */
  function selectWindow(w) {
    windowChoice = {
      process_name: w.process_name ?? "",
      window_title_hint: w.title,
      backend_hint_id: w.handle,
    };
  }

  // -- action: Repeat -------------------------------------------------------

  /** @param {any} event */
  function inputEventToFields(event) {
    if (event && "KeyPress" in event) {
      return {
        kind: "KeyPress",
        modifiers: event.KeyPress.modifiers,
        key: event.KeyPress.key,
        dx: 0,
        dy: 0,
      };
    }
    if (event && "Scroll" in event) {
      return {
        kind: "Scroll",
        modifiers: [],
        key: "",
        dx: event.Scroll.dx,
        dy: event.Scroll.dy,
      };
    }
    // The GUI form only edits the two InputEvent kinds the CLI's own
    // `key:`/`scroll:` repeat notation supports (see
    // crates/orchestrator-cli/src/notation.rs's parse_repeat_input) -- no
    // current write path (CLI or GUI) can ever produce anything else here,
    // but fall back safely rather than crash if one ever did.
    return { kind: "KeyPress", modifiers: [], key: "", dx: 0, dy: 0 };
  }

  const initialRepeat =
    initial && "Repeat" in initial.action ? initial.action.Repeat : null;
  const initialRepeatFields = inputEventToFields(initialRepeat?.input);

  let actionKind = $state(
    initial && initial.action && "Macro" in initial.action
      ? "Macro"
      : "Repeat",
  );

  let repeatInputKind = $state(initialRepeatFields.kind);
  let repeatModifiers = $state(initialRepeatFields.modifiers);
  let repeatKey = $state(initialRepeatFields.key);
  let repeatDx = $state(initialRepeatFields.dx);
  let repeatDy = $state(initialRepeatFields.dy);
  let repeatIntervalMs = $state(initialRepeat?.interval_ms ?? 100);
  // A `type="number"` input's `bind:value` in Svelte 5 always yields a
  // `number` (non-empty) or `null` (empty) -- never a string, regardless of
  // what this is seeded with -- so this is typed/handled as `number | null`
  // throughout, not run through `.trim()`/`Number()` like a text field.
  /** @type {number | null} */
  let repeatJitterMs = $state(
    initialRepeat && initialRepeat.jitter !== "None"
      ? initialRepeat.jitter.Uniform.range_ms
      : null,
  );

  /** @param {string} mod */
  function toggleRepeatModifier(mod) {
    repeatModifiers = repeatModifiers.includes(mod)
      ? repeatModifiers.filter((/** @type {string} */ m) => m !== mod)
      : [...repeatModifiers, mod];
  }

  // -- action: Macro --------------------------------------------------------

  let macroStepIdCounter = 0;

  /** @param {any} step */
  function macroStepToFields(step) {
    const id = macroStepIdCounter++;
    if ("KeyPress" in step) {
      return {
        id,
        kind: "KeyPress",
        modifiers: step.KeyPress.combo.modifiers,
        key: step.KeyPress.combo.key,
        dx: 0,
        dy: 0,
        delayMs: step.KeyPress.delay_ms,
      };
    }
    if ("Scroll" in step) {
      return {
        id,
        kind: "Scroll",
        modifiers: [],
        key: "",
        dx: step.Scroll.dx,
        dy: step.Scroll.dy,
        delayMs: step.Scroll.delay_ms,
      };
    }
    // MouseClick/Drag macro steps aren't editable by this form (the brief
    // scopes macro-step editing to the same key/scroll kinds as Repeat) --
    // keep the original step data untouched rather than corrupt it.
    return {
      id,
      kind: "Unsupported",
      modifiers: [],
      key: "",
      dx: 0,
      dy: 0,
      delayMs: 0,
      raw: step,
    };
  }

  const initialMacro =
    initial && initial.action && "Macro" in initial.action
      ? initial.action.Macro
      : null;

  /** @type {MacroStepFields[]} */
  let macroSteps = $state(
    initialMacro ? initialMacro.steps.map(macroStepToFields) : [],
  );
  let macroLoop = $state(initialMacro?.loop ?? false);

  function addMacroStep() {
    macroSteps = [
      ...macroSteps,
      {
        id: macroStepIdCounter++,
        kind: "KeyPress",
        modifiers: [],
        key: "",
        dx: 0,
        dy: 0,
        delayMs: 50,
      },
    ];
  }

  /** @param {number} id */
  function removeMacroStep(id) {
    macroSteps = macroSteps.filter((s) => s.id !== id);
  }

  /**
   * @param {number} id
   * @param {string} mod
   */
  function toggleMacroStepModifier(id, mod) {
    macroSteps = macroSteps.map((s) =>
      s.id === id
        ? {
            ...s,
            modifiers: s.modifiers.includes(mod)
              ? s.modifiers.filter((m) => m !== mod)
              : [...s.modifiers, mod],
          }
        : s,
    );
  }

  // -- debounce -------------------------------------------------------------
  // Same `number | null` note as `repeatJitterMs` above applies here.
  /** @type {number | null} */
  let debounceMs = $state(initial ? initial.debounce_ms : null);

  // -- submit -----------------------------------------------------------

  let errorMessage = $state("");
  let submitting = $state(false);

  function buildScope() {
    if (scopeKind === "Desktop") return "Desktop";
    return {
      Window: windowChoice ?? {
        process_name: "",
        window_title_hint: "",
        backend_hint_id: null,
      },
    };
  }

  function buildAction() {
    if (actionKind === "Repeat") {
      const input =
        repeatInputKind === "KeyPress"
          ? { KeyPress: { modifiers: repeatModifiers, key: repeatKey } }
          : { Scroll: { dx: repeatDx, dy: repeatDy } };
      const jitter =
        repeatJitterMs === null
          ? "None"
          : { Uniform: { range_ms: repeatJitterMs } };
      return {
        Repeat: { input, interval_ms: repeatIntervalMs, jitter },
      };
    }
    const steps = macroSteps.map((s) => {
      if (s.kind === "Unsupported") return s.raw;
      if (s.kind === "KeyPress") {
        return {
          KeyPress: {
            combo: { modifiers: s.modifiers, key: s.key },
            delay_ms: s.delayMs,
          },
        };
      }
      return { Scroll: { delay_ms: s.delayMs, dx: s.dx, dy: s.dy } };
    });
    return { Macro: { steps, loop: macroLoop } };
  }

  /** @param {SubmitEvent} event */
  async function handleSubmit(event) {
    event.preventDefault();
    errorMessage = "";
    submitting = true;
    try {
      await onSubmit({
        name,
        scope: buildScope(),
        action: buildAction(),
        debounceMs,
      });
    } catch (e) {
      errorMessage = String(e);
    } finally {
      submitting = false;
    }
  }
</script>

<form onsubmit={handleSubmit}>
  <h2>{mode === "add" ? "Add profile" : `Edit profile: ${name}`}</h2>

  {#if errorMessage}
    <p class="error">{errorMessage}</p>
  {/if}

  <label>
    Name
    <input
      type="text"
      bind:value={name}
      disabled={mode === "edit"}
      required
    />
  </label>

  <fieldset>
    <legend>Scope</legend>
    <label>
      <input
        type="radio"
        name="scope-kind"
        checked={scopeKind === "Desktop"}
        onchange={() => chooseScopeKind("Desktop")}
      />
      Desktop
    </label>
    <label>
      <input
        type="radio"
        name="scope-kind"
        checked={scopeKind === "Window"}
        onchange={() => chooseScopeKind("Window")}
      />
      Window
    </label>

    {#if scopeKind === "Window"}
      <div class="window-picker">
        {#if windowsLoading}
          <p>Loading windows...</p>
        {:else if windowsError}
          <p class="error">{windowsError}</p>
        {:else if windows.length === 0}
          <p>No windows currently open.</p>
        {:else}
          <ul class="window-list">
            {#each windows as w (w.handle)}
              <li>
                <label>
                  <input
                    type="radio"
                    name="window-choice"
                    checked={windowChoice?.backend_hint_id === w.handle}
                    onchange={() => selectWindow(w)}
                  />
                  {w.process_name ?? "(unknown process)"} — {w.title}
                </label>
              </li>
            {/each}
          </ul>
        {/if}
        <button type="button" onclick={loadWindows}>Refresh windows</button>
        {#if windowChoice}
          <p class="hint">
            Selected: {windowChoice.process_name} — {windowChoice.window_title_hint}
          </p>
        {/if}
      </div>
    {/if}
  </fieldset>

  <fieldset>
    <legend>Action</legend>
    <label>
      <input
        type="radio"
        name="action-kind"
        checked={actionKind === "Repeat"}
        onchange={() => (actionKind = "Repeat")}
      />
      Repeat
    </label>
    <label>
      <input
        type="radio"
        name="action-kind"
        checked={actionKind === "Macro"}
        onchange={() => (actionKind = "Macro")}
      />
      Macro
    </label>

    {#if actionKind === "Repeat"}
      <div class="action-detail">
        <label>
          Input kind
          <select bind:value={repeatInputKind}>
            <option value="KeyPress">Key combo</option>
            <option value="Scroll">Scroll</option>
          </select>
        </label>

        {#if repeatInputKind === "KeyPress"}
          <div class="modifiers">
            {#each MODIFIERS as mod (mod)}
              <label>
                <input
                  type="checkbox"
                  checked={repeatModifiers.includes(mod)}
                  onchange={() => toggleRepeatModifier(mod)}
                />
                {mod}
              </label>
            {/each}
          </div>
          <label>
            Key
            <input type="text" bind:value={repeatKey} required />
          </label>
        {:else}
          <label>
            Scroll dx
            <input type="number" bind:value={repeatDx} required />
          </label>
          <label>
            Scroll dy
            <input type="number" bind:value={repeatDy} required />
          </label>
        {/if}

        <label>
          Interval (ms)
          <input
            type="number"
            min="1"
            bind:value={repeatIntervalMs}
            required
          />
        </label>
        <label>
          Jitter (ms, optional)
          <input type="number" min="0" bind:value={repeatJitterMs} />
        </label>
      </div>
    {:else}
      <div class="action-detail">
        {#each macroSteps as step (step.id)}
          <div class="macro-step">
            {#if step.kind === "Unsupported"}
              <p class="hint">
                Step type not editable in this UI (created outside the GUI);
                delay_ms unchanged.
              </p>
            {:else}
              <label>
                Input kind
                <select bind:value={step.kind}>
                  <option value="KeyPress">Key combo</option>
                  <option value="Scroll">Scroll</option>
                </select>
              </label>
              {#if step.kind === "KeyPress"}
                <div class="modifiers">
                  {#each MODIFIERS as mod (mod)}
                    <label>
                      <input
                        type="checkbox"
                        checked={step.modifiers.includes(mod)}
                        onchange={() =>
                          toggleMacroStepModifier(step.id, mod)}
                      />
                      {mod}
                    </label>
                  {/each}
                </div>
                <label>
                  Key
                  <input type="text" bind:value={step.key} required />
                </label>
              {:else}
                <label>
                  Scroll dx
                  <input type="number" bind:value={step.dx} required />
                </label>
                <label>
                  Scroll dy
                  <input type="number" bind:value={step.dy} required />
                </label>
              {/if}
            {/if}
            <label>
              Delay (ms)
              <input
                type="number"
                min="0"
                bind:value={step.delayMs}
                required
              />
            </label>
            <button type="button" onclick={() => removeMacroStep(step.id)}
              >Remove step</button
            >
          </div>
        {/each}
        <button type="button" onclick={addMacroStep}>Add step</button>
        <label>
          <input type="checkbox" bind:checked={macroLoop} />
          Loop
        </label>
      </div>
    {/if}
  </fieldset>

  <label>
    Debounce (ms, optional, defaults to 400)
    <input type="number" min="0" bind:value={debounceMs} />
  </label>

  <div class="form-actions">
    <button type="submit" disabled={submitting}>
      {mode === "add" ? "Add profile" : "Save changes"}
    </button>
    <button type="button" onclick={onCancel} disabled={submitting}
      >Cancel</button
    >
  </div>
</form>

<style>
  form {
    display: flex;
    flex-direction: column;
    gap: 1em;
    max-width: 32em;
  }

  fieldset {
    border: 1px solid rgba(127, 127, 127, 0.35);
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    gap: 0.5em;
  }

  label {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.25em;
  }

  fieldset > label {
    flex-direction: row;
    align-items: center;
  }

  .modifiers {
    display: flex;
    gap: 0.75em;
  }

  .modifiers label {
    flex-direction: row;
    align-items: center;
    gap: 0.25em;
  }

  .action-detail,
  .window-picker {
    display: flex;
    flex-direction: column;
    gap: 0.5em;
    padding-left: 0.5em;
  }

  .macro-step {
    border: 1px dashed rgba(127, 127, 127, 0.4);
    border-radius: 6px;
    padding: 0.5em;
    display: flex;
    flex-direction: column;
    gap: 0.4em;
  }

  .window-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .form-actions {
    display: flex;
    gap: 0.5em;
  }

  .error {
    color: #c0392b;
  }

  .hint {
    opacity: 0.7;
    font-size: 0.9em;
  }
</style>
