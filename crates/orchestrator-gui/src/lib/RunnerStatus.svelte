<script>
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";

  /** @typedef {{ state: "idle" | "running" | "error", error: string | null }} RunnerStatusDto */

  /** @type {RunnerStatusDto} */
  let status = $state({ state: "idle", error: null });
  let errorMessage = $state("");
  let busy = $state(false);

  /** @param {unknown} event */
  function applyStatus(event) {
    status = /** @type {RunnerStatusDto} */ (event);
  }

  onMount(() => {
    invoke("runner_status").then(applyStatus);

    const unlistenPromise = listen("runner-status-changed", (event) =>
      applyStatus(event.payload),
    );
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  });

  async function handleStart() {
    errorMessage = "";
    busy = true;
    try {
      await invoke("start_runner");
    } catch (e) {
      errorMessage = String(e);
    } finally {
      busy = false;
    }
  }

  async function handleStop() {
    errorMessage = "";
    busy = true;
    try {
      await invoke("stop_runner");
    } catch (e) {
      errorMessage = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="runner-status">
  <div class="status-row">
    <span class="label">Status:</span>
    <span class="state state-{status.state}">
      {status.state === "idle"
        ? "Idle"
        : status.state === "running"
          ? "Running"
          : "Error"}
    </span>
    {#if status.state === "running"}
      <button onclick={handleStop} disabled={busy}>Stop</button>
    {:else}
      <button onclick={handleStart} disabled={busy}>Start</button>
    {/if}
  </div>

  {#if status.state === "error" && status.error}
    <p class="error">{status.error}</p>
  {/if}

  {#if errorMessage}
    <p class="error">{errorMessage}</p>
  {/if}

  <p class="hint">
    The first time you click Start, KDE may ask you to press a key
    combination for each profile that doesn't have one assigned yet — this
    happens once per profile. Change an assigned shortcut anytime in KDE's
    System Settings → Shortcuts → Orchestrator.
  </p>
</section>

<style>
  .runner-status {
    display: flex;
    flex-direction: column;
    gap: 0.5em;
    padding: 0.75em 1em;
    margin-bottom: 1.5em;
    border: 1px solid rgba(127, 127, 127, 0.35);
    border-radius: 8px;
  }

  .status-row {
    display: flex;
    align-items: center;
    gap: 0.75em;
  }

  .label {
    font-weight: 600;
  }

  .state {
    font-weight: 600;
  }

  .state-idle {
    opacity: 0.7;
  }

  .state-running {
    color: #2e8b57;
  }

  .state-error {
    color: #c0392b;
  }

  .error {
    color: #c0392b;
    margin: 0;
  }

  .hint {
    opacity: 0.7;
    font-size: 0.9em;
    margin: 0;
  }
</style>
