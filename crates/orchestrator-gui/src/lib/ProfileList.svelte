<script>
  import { scopeSummary, actionSummary } from "./format.js";

  /** @type {{ profiles: any[], onAdd: () => void, onEdit: (p: any) => void, onRemove: (name: string) => void }} */
  let { profiles, onAdd, onEdit, onRemove } = $props();
</script>

<div class="toolbar">
  <h2>Profiles</h2>
  <button onclick={onAdd}>Add profile</button>
</div>

{#if profiles.length === 0}
  <p class="empty">No profiles configured yet.</p>
{:else}
  <ul class="profile-list">
    {#each profiles as profile (profile.name)}
      <li class="profile-row">
        <div class="profile-info">
          <span class="name">{profile.name}</span>
          <span class="badge">{scopeSummary(profile.scope)}</span>
          <span class="badge">{actionSummary(profile.action)}</span>
          <span class="debounce">debounce={profile.debounce_ms}ms</span>
        </div>
        <div class="profile-actions">
          <button onclick={() => onEdit(profile)}>Edit</button>
          <button class="danger" onclick={() => onRemove(profile.name)}
            >Remove</button
          >
        </div>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1em;
  }

  .empty {
    opacity: 0.7;
  }

  .profile-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5em;
  }

  .profile-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1em;
    padding: 0.6em 0.9em;
    border: 1px solid rgba(127, 127, 127, 0.35);
    border-radius: 8px;
    flex-wrap: wrap;
  }

  .profile-info {
    display: flex;
    align-items: center;
    gap: 0.75em;
    flex-wrap: wrap;
  }

  .name {
    font-weight: 600;
  }

  .badge {
    font-size: 0.9em;
    opacity: 0.85;
  }

  .debounce {
    font-size: 0.85em;
    opacity: 0.6;
  }

  .profile-actions {
    display: flex;
    gap: 0.5em;
  }

  .danger {
    color: #c0392b;
  }
</style>
