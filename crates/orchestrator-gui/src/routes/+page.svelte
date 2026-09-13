<script>
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import ProfileList from "$lib/ProfileList.svelte";
  import ProfileForm from "$lib/ProfileForm.svelte";

  /** @type {any[]} */
  let profiles = $state([]);
  let errorMessage = $state("");
  /** @type {"list" | "form"} */
  let view = $state("list");
  /** @type {any | null} */
  let editingProfile = $state(null);

  async function refreshProfiles() {
    try {
      profiles = await invoke("list_profiles");
      errorMessage = "";
    } catch (e) {
      errorMessage = String(e);
    }
  }

  onMount(refreshProfiles);

  function startAdd() {
    editingProfile = null;
    view = "form";
  }

  /** @param {any} profile */
  function startEdit(profile) {
    editingProfile = profile;
    view = "form";
  }

  function cancelForm() {
    view = "list";
  }

  /** @param {string} name */
  async function handleRemove(name) {
    errorMessage = "";
    try {
      await invoke("remove_profile", { name });
      await refreshProfiles();
    } catch (e) {
      errorMessage = String(e);
    }
  }

  /** @param {{ name: string, scope: any, action: any, debounceMs: number | null }} payload */
  async function handleFormSubmit(payload) {
    if (editingProfile === null) {
      await invoke("add_profile", payload);
    } else {
      await invoke("edit_profile", payload);
    }
    await refreshProfiles();
    view = "list";
  }
</script>

<main class="container">
  <h1>Orchestrator</h1>

  {#if errorMessage}
    <p class="error">{errorMessage}</p>
  {/if}

  {#if view === "list"}
    <ProfileList
      {profiles}
      onAdd={startAdd}
      onEdit={startEdit}
      onRemove={handleRemove}
    />
  {:else}
    {#key editingProfile?.name ?? "__add__"}
      <ProfileForm
        mode={editingProfile === null ? "add" : "edit"}
        initial={editingProfile}
        onSubmit={handleFormSubmit}
        onCancel={cancelForm}
      />
    {/key}
  {/if}
</main>

<style>
  :root {
    font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
    font-size: 16px;
    line-height: 24px;
    font-weight: 400;

    color: #0f0f0f;
    background-color: #f6f6f6;

    font-synthesis: none;
    text-rendering: optimizeLegibility;
    -webkit-font-smoothing: antialiased;
    -moz-osx-font-smoothing: grayscale;
    -webkit-text-size-adjust: 100%;
  }

  .container {
    margin: 0 auto;
    max-width: 48em;
    padding: 2em 1.5em;
  }

  h1 {
    text-align: center;
  }

  .error {
    color: #c0392b;
    background-color: rgba(192, 57, 43, 0.1);
    border: 1px solid rgba(192, 57, 43, 0.3);
    border-radius: 6px;
    padding: 0.5em 0.75em;
  }

  /* `:global(...)` here: these elements live inside ProfileList/ProfileForm
     (separate components with their own scoped styles), not directly in
     this component's own template, so an un-global selector would never
     match anything and svelte-check flags it as an unused CSS selector. */
  :global(input),
  :global(button),
  :global(select) {
    border-radius: 8px;
    border: 1px solid transparent;
    padding: 0.5em 0.9em;
    font-size: 1em;
    font-weight: 500;
    font-family: inherit;
    color: #0f0f0f;
    background-color: #ffffff;
    transition: border-color 0.25s;
    box-shadow: 0 2px 2px rgba(0, 0, 0, 0.2);
  }

  :global(button) {
    cursor: pointer;
  }

  :global(button:hover) {
    border-color: #396cd8;
  }
  :global(button:active) {
    border-color: #396cd8;
    background-color: #e8e8e8;
  }

  :global(input),
  :global(button),
  :global(select) {
    outline: none;
  }

  @media (prefers-color-scheme: dark) {
    :root {
      color: #f6f6f6;
      background-color: #2f2f2f;
    }

    :global(input),
    :global(button),
    :global(select) {
      color: #ffffff;
      background-color: #0f0f0f98;
    }
    :global(button:active) {
      background-color: #0f0f0f69;
    }
  }
</style>
