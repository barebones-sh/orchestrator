// UI-facing summaries of a Profile's `scope` and `action` fields. Mirrors
// the information `orchestrator-cli`'s `format_profile_list` shows (see
// crates/orchestrator-cli/src/profile_commands.rs), just rendered as
// separate UI strings instead of a tab-separated line.

/**
 * @param {"Desktop" | {Window: {process_name: string, window_title_hint: string, backend_hint_id: string | null}}} scope
 */
export function scopeSummary(scope) {
  if (scope === "Desktop") return "Desktop";
  return `Window(${scope.Window.process_name})`;
}

/**
 * @param {any} action
 */
export function actionSummary(action) {
  if ("Repeat" in action) {
    const { interval_ms, jitter } = action.Repeat;
    const jitterText =
      jitter === "None" ? "no jitter" : `±${jitter.Uniform.range_ms}ms`;
    return `Repeat every ${interval_ms}ms (${jitterText})`;
  }
  const { steps, loop } = action.Macro;
  return `Macro (${steps.length} steps, ${loop ? "looping" : "once"})`;
}
