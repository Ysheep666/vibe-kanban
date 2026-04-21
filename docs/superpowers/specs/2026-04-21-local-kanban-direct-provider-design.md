# Local-Direct Kanban Provider Design

**Goal:** In local-only desktop mode, bypass the remote/Electric provider tree and load the kanban board through React Query against two dedicated snapshot endpoints instead of ten `useShape` fallback polls.

**Context:** Vibe Kanban's desktop app runs with `local_only=true`. Today the kanban still renders through `packages/web-core/src/shared/providers/remote/ProjectProvider.tsx`, which mounts ten `useShape` hooks that detect `lockElectricToFallback()` and poll ten separate `/v1/fallback/*` endpoints every 30 seconds. That adds: Provider tree initialisation overhead, serial REST round-trips (issues and statuses block render; eight others load in the background), and a `/api/config` gate before the fallback lock takes effect.

**Target outcome:** Opening the kanban in local mode makes **two** HTTP data requests (org snapshot + project snapshot) instead of 10+ fallback shape calls, with optimistic mutations preserved.

## Success criteria

1. In local mode, the kanban route issues exactly two data requests for its hot path: `GET /api/organizations/{id}/snapshot` and `GET /api/projects/{id}/kanban-snapshot`.
2. All 19 mutations exposed by `ProjectContext` (8 inserts + 3 updates + 8 removes, across eight collections) and all three exposed by `OrgContext` (project insert/update/remove) work with optimistic updates: the UI reflects the change on the same frame the user commits it, and rolls back on server failure.
3. `cargo test --workspace`, `pnpm run check`, `pnpm run lint` all pass.
4. Remote/cloud mode (`local_only=false`) renders through the unchanged `remote/*` providers and behaves identically to today.
5. Dragging an issue across kanban columns feels indistinguishable from the current remote behaviour (no visible latency on release).

## Architecture

### Backend: two snapshot endpoints

Two new read-only routes registered in `crates/server/src/routes/`. Both return a single JSON body assembled from existing list methods run on one pooled SQLite connection.

**`GET /api/organizations/{organization_id}/snapshot`**

```rust
#[derive(Serialize, TS)]
pub struct OrgSnapshot {
    organization_id: Uuid,
    projects: Vec<Project>,
    members_with_profiles: Vec<OrganizationMemberWithProfile>,
}
```

Handler flow:
1. Acquire one `Pool::acquire()` connection.
2. Call `LocalRemote::list_projects_for_org(&mut conn, org_id)`.
3. Call `LocalRemote::list_members_with_profiles(&mut conn, org_id)` (new method: LEFT JOIN `remote_organization_members` + `remote_user_profiles` on `user_id`).
4. Return `OrgSnapshot`.

**`GET /api/projects/{project_id}/kanban-snapshot`**

```rust
#[derive(Serialize, TS)]
pub struct ProjectKanbanSnapshot {
    project_id: Uuid,
    issues: Vec<Issue>,
    statuses: Vec<ProjectStatus>,
    tags: Vec<Tag>,
    issue_assignees: Vec<IssueAssignee>,
    issue_followers: Vec<IssueFollower>,
    issue_tags: Vec<IssueTag>,
    issue_relationships: Vec<IssueRelationship>,
    pull_requests: Vec<PullRequest>,
    pull_request_issues: Vec<PullRequestIssue>,
    workspaces: Vec<Workspace>,
}
```

Handler flow:
1. Acquire one `Pool::acquire()` connection.
2. Run the ten existing `LocalRemote::list_*` methods sequentially on that single connection.
3. Return `ProjectKanbanSnapshot`.

Both structs get `#[derive(TS)]` and are registered in `crates/server/src/bin/generate_types.rs` so they appear in `shared/types.ts` after running `pnpm run generate-types`.

**Why sequential on one connection, not parallel:** the ten queries are all single-table filtered lookups on indexed columns (`WHERE project_id = ?`). Each is O(rows in that project). SQLite's write-ahead log plus prepared-statement cache makes sequential reads on one connection faster in practice than acquiring ten connections from the pool. Sequential also keeps transaction semantics trivial (a snapshot is a consistent read, even without explicit `BEGIN`).

### Frontend: two local providers

New directory: `packages/web-core/src/shared/providers/local/`.

**`LocalOrgProvider.tsx`** — identical public API to `remote/OrgProvider`, internal mechanism uses React Query:

```tsx
export function LocalOrgProvider({ organizationId, children }: Props) {
  const queryClient = useQueryClient();
  const { data, isLoading, error, refetch } = useQuery({
    queryKey: ['org-snapshot', organizationId],
    queryFn: () => api.getOrgSnapshot(organizationId),
    staleTime: 5 * 60 * 1000,
    refetchInterval: 5 * 60 * 1000,
    enabled: Boolean(organizationId),
  });

  const mutations = useOrgOptimisticMutations(queryClient, organizationId);
  const lookups = useOrgLookups(data);

  const value: OrgContextValue = {
    organizationId,
    projects: data?.projects ?? [],
    // membersWithProfilesById computed in lookups
    isLoading,
    error: error ? toSyncError(error) : null,
    retry: () => refetch(),
    ...mutations,
    ...lookups,
  };

  return <OrgContext.Provider value={value}>{children}</OrgContext.Provider>;
}
```

**`LocalProjectProvider.tsx`** — same shape, targeting `ProjectContext`:

```tsx
export function LocalProjectProvider({ projectId, children }: Props) {
  const queryClient = useQueryClient();
  const { data, isLoading, error, refetch } = useQuery({
    queryKey: ['kanban-snapshot', projectId],
    queryFn: () => api.getKanbanSnapshot(projectId),
    staleTime: 30 * 1000,
    refetchInterval: 30 * 1000,
    enabled: Boolean(projectId),
  });

  const mutations = useProjectOptimisticMutations(queryClient, projectId);
  const lookups = useProjectLookups(data);

  const value: ProjectContextValue = {
    projectId,
    issues: data?.issues ?? [],
    statuses: data?.statuses ?? [],
    tags: data?.tags ?? [],
    // ... other 7 arrays
    isLoading,
    error: error ? toSyncError(error) : null,
    retry: () => refetch(),
    ...mutations,
    ...lookups,
  };

  return <ProjectContext.Provider value={value}>{children}</ProjectContext.Provider>;
}
```

Both providers publish the **existing** `OrgContext` / `ProjectContext` defined in `packages/web-core/src/shared/hooks/useOrgContext.ts` and `useProjectContext.ts`. Consumer hooks (`useOrgContext`, `useProjectContext`) do not change; neither do their 20+ call sites.

### Shared lookup helpers

`ProjectContext` exposes 11 pure lookup functions (`getIssue`, `getIssuesForStatus`, `getAssigneesForIssue`, `getFollowersForIssue`, `getTagsForIssue`, `getTagObjectsForIssue`, `getRelationshipsForIssue`, `getStatus`, `getTag`, `getPullRequestsForIssue`, `getWorkspacesForIssue`) plus three computed maps (`issuesById`, `statusesById`, `tagsById`). `OrgContext` exposes one helper (`getProject`) and two maps (`projectsById`, `membersWithProfilesById`). They currently live inline in `remote/ProjectProvider.tsx` / `remote/OrgProvider.tsx`. They get lifted into shared files:

```
packages/web-core/src/shared/providers/shared/projectLookups.ts
packages/web-core/src/shared/providers/shared/orgLookups.ts
```

Both the Local and Remote providers import these. The Remote provider is refactored to use them too, so the lookup implementation is defined once.

### Optimistic mutation helper

A generic helper that turns "collection + REST API triple" into three optimistic mutation functions. Located at `packages/web-core/src/shared/providers/local/useOptimisticCollection.ts`:

```tsx
type CollectionAccessor<TSnapshot, TRow> = {
  get: (snap: TSnapshot) => TRow[];
  set: (snap: TSnapshot, rows: TRow[]) => TSnapshot;
};

type CollectionApi<TRow, TCreate, TUpdate> = {
  create: (req: TCreate, optimisticId: string) => Promise<TRow>;
  update: (id: string, changes: Partial<TUpdate>) => Promise<TRow>;
  remove: (id: string) => Promise<void>;
};

function useOptimisticCollection<TSnapshot, TRow extends { id: string }, TCreate, TUpdate>(
  queryClient: QueryClient,
  queryKey: QueryKey,
  accessor: CollectionAccessor<TSnapshot, TRow>,
  buildOptimisticRow: (req: TCreate, id: string) => TRow,
  buildOptimisticUpdate: (existing: TRow, changes: Partial<TUpdate>) => TRow,
  api: CollectionApi<TRow, TCreate, TUpdate>,
): {
  insert: (req: TCreate) => InsertResult<TRow>;
  update: (id: string, changes: Partial<TUpdate>) => MutationResult;
  remove: (id: string) => MutationResult;
};
```

Each of the three returned functions follows the same shape:

- **insert:** generate optimistic UUID → write optimistic row to cache via `setQueryData` → fire REST `create` → on success replace the row with the server's version (IDs may match if the client generated one; even so, timestamps and computed fields come from the server); on failure remove the optimistic row and throw via `persisted`.
- **update:** snapshot the current row → write the merged optimistic row → fire REST `update` → on success replace with server row; on failure restore the original.
- **remove:** snapshot the current row → remove it from the cache → fire REST `remove` → on failure re-insert at the same position; throw via `persisted`.

All three return the same shapes as the existing `remote/ProjectProvider`:

```tsx
type InsertResult<TRow> = { data: TRow; persisted: Promise<TRow> };
type MutationResult = { persisted: Promise<void> };
```

`ProjectContext` exposes 19 mutations across eight mutable collections (the snapshot contains ten collections total; `pullRequests` and `workspaces` are data-only):

| Collection           | insert | update | remove |
|----------------------|--------|--------|--------|
| issues               | ✓      | ✓      | ✓      |
| statuses             | ✓      | ✓      | ✓      |
| tags                 | ✓      | ✓      | ✓      |
| issue_assignees      | ✓      |        | ✓      |
| issue_followers      | ✓      |        | ✓      |
| issue_tags           | ✓      |        | ✓      |
| issue_relationships  | ✓      |        | ✓      |
| pull_request_issues  | ✓      |        | ✓      |

That is 8 inserts + 3 updates + 8 removes = **19 mutations** exposed on `ProjectContext` (matches the `useProjectContext.ts` interface). Plus three on `OrgContext` (projects: insert/update/remove).

`LocalProjectProvider` calls `useOptimisticCollection` eight times, once per mutable collection. For collections without update, it passes `undefined` for the update API and exposes only insert/remove.

### Snapshot mutation of cache

Every optimistic write in the helper is expressed as a pure function from `TSnapshot → TSnapshot`, e.g.:

```tsx
queryClient.setQueryData<TSnapshot>(queryKey, (old) => {
  if (!old) return old;
  return accessor.set(old, [...accessor.get(old), optimisticRow]);
});
```

This keeps the cache shape intact (React Query's reference identity per key) and lets React's re-render reach only the affected `useQuery` consumer.

### Provider integration: dispatcher shell

`ProjectProvider` and `OrgProvider` are currently mounted at five sites across `packages/web-core/`:

- `packages/web-core/src/pages/kanban/ProjectKanban.tsx` (both providers — kanban + issue-detail routes)
- `packages/web-core/src/shared/dialogs/command-bar/WorkspaceSelectionDialog.tsx`
- `packages/web-core/src/shared/dialogs/command-bar/LinkPrToIssueDialog.tsx`
- `packages/web-core/src/shared/dialogs/command-bar/selections/ProjectSelectionDialog.tsx`
- `packages/web-core/src/shared/dialogs/kanban/AssigneeSelectionDialog.tsx`

Rather than change every call site, the existing provider names become thin **dispatchers** that pick the implementation based on `userSystemInfo.local_only`. The current Electric implementation is renamed; the name `ProjectProvider` / `OrgProvider` becomes the shell.

**Rename**
- `packages/web-core/src/shared/providers/remote/ProjectProvider.tsx` → `RemoteProjectProvider` (same file, default export renamed).
- `packages/web-core/src/shared/providers/remote/OrgProvider.tsx` → `RemoteOrgProvider`.

**New dispatcher** at `packages/web-core/src/shared/providers/ProjectProvider.tsx`:

```tsx
import { useUserSystem } from '@/shared/hooks/useUserSystem';
import { RemoteProjectProvider } from '@/shared/providers/remote/ProjectProvider';
import { LocalProjectProvider } from '@/shared/providers/local/LocalProjectProvider';

export function ProjectProvider({ projectId, children }: Props) {
  const { userSystemInfo } = useUserSystem();
  if (userSystemInfo?.local_only) {
    return (
      <LocalProjectProvider projectId={projectId}>
        {children}
      </LocalProjectProvider>
    );
  }
  return (
    <RemoteProjectProvider projectId={projectId}>
      {children}
    </RemoteProjectProvider>
  );
}
```

A matching `OrgProvider` dispatcher lives at `packages/web-core/src/shared/providers/OrgProvider.tsx`.

**Import migration:** the five existing consumer sites update their imports from `@/shared/providers/remote/ProjectProvider` (and `/OrgProvider`) to `@/shared/providers/ProjectProvider` (the new dispatcher path). Pure string rewrite; no behavioral changes at consumer sites.

**Mounting guard:** while `useUserSystem()` is still resolving (`userSystemInfo === null`), the dispatcher renders `<div aria-busy="true" />` (or re-uses the existing kanban loading placeholder from `ProjectKanban.tsx:190`). This matches current behaviour — `ProjectKanban` already gates on `useFindProjectById`'s `isLoading`, so the config load adds no visible latency.

`ProjectKanban`, `KanbanContainer`, `KanbanIssuePanelContainer`, and all 18 other downstream consumer components are **not touched**. The route files (`_app.projects.$projectId.tsx`, `_app.projects.$projectId_.issues.$issueId.tsx`, etc.) are also **not touched** — they continue to mount `LocalProjectKanban` → `ProjectKanban`, which now internally picks the Local or Remote provider via the dispatcher.

### Cache invalidation

After each mutation's REST call succeeds, the helper issues a background invalidation:

```tsx
queryClient.invalidateQueries({ queryKey: ['kanban-snapshot', projectId] });
```

For org-level mutations (`insertProject` / `updateProject` / `removeProject`), also invalidate `['org-snapshot', organizationId]`.

This schedules a refetch that runs in the background — `staleTime: 30000` keeps the current cache visible while the refetch is in flight. Combined with optimistic updates, the user sees the change instantly; the background refetch merely reconciles any server-side computed fields (timestamps, computed sort orders from `/v1/issues/bulk`).

## Data types

All row types already exist in `shared/remote-types.ts` (generated from Rust, do not edit). Both new snapshot response types get generated into `shared/types.ts` via `crates/server/src/bin/generate_types.rs`.

The optimistic row builders need to produce objects matching the Rust-generated row types. For fields the client cannot know (server-side `created_at`, `updated_at`, `issue_number`, computed `sort_order`), the builder inserts placeholder values (current local timestamp, `-1` for counters); they get replaced by the server's version in `persisted`.

## API client additions

`packages/web-core/src/shared/lib/remoteApi.ts` (the existing home for `/v1/*` and fallback REST functions) gets two new exports:

```tsx
async function getOrgSnapshot(organizationId: string): Promise<OrgSnapshot>;
async function getKanbanSnapshot(projectId: string): Promise<ProjectKanbanSnapshot>;
```

Both are plain `GET` calls via the module's existing `makeRequest` helper. No new auth plumbing. Response types are imported from `shared/types.ts` (generated via `pnpm run generate-types`).

## Testing

### Backend

- **Unit tests** in `crates/services/src/services/local_remote.rs`:
  - `get_kanban_snapshot_returns_all_collections_for_project` — seed a project with one row in each collection, assert all ten arrays populated.
  - `get_kanban_snapshot_isolates_by_project` — seed rows in project A and project B, assert snapshot for A contains only A's rows.
  - `get_org_snapshot_returns_projects_and_members` — seed org with two projects, two members, assert both appear.

- **Route tests** in `crates/server/src/routes/`:
  - GET 200 for a valid project id, body shape matches.
  - GET 404 for an unknown project id.

### Frontend

- **`useOptimisticCollection.test.ts`** (Vitest):
  - insert: cache updates synchronously, REST stub resolves with server row, cache replaces optimistic with server row, `persisted` resolves.
  - insert rollback: REST stub rejects, cache reverts, `persisted` rejects.
  - update: same as insert but for existing row.
  - remove: row disappears from cache, REST succeeds, stays removed.
  - remove rollback: row reappears at original index after REST rejection.

- **`LocalProjectProvider.test.tsx`** (Vitest + RTL):
  - Mount with mocked `getKanbanSnapshot` returning a seeded snapshot.
  - Assert `useProjectContext` inside consumers returns matching arrays + working lookup helpers.
  - Assert `isLoading` transitions false once the query resolves.

- **`LocalOrgProvider.test.tsx`** (Vitest + RTL): mirror of above for org context.

### Manual smoke

After building and installing the DMG:

1. Open the kanban for a project. In Tauri DevTools Network, filter `/api/` and `/v1/` — there must be exactly two data requests (`org/.../snapshot` + `projects/.../kanban-snapshot`), no `/v1/fallback/*` calls.
2. Drag an issue across columns — the card must land in the new column on the same animation frame as mouse-up.
3. Create an issue via the `+` control — the card must appear immediately; confirm the final row replaces the optimistic one after the REST call resolves.
4. Delete an issue — it must disappear immediately; if the server rejects (simulate by killing the backend), the card must reappear.
5. Switch to another project, then back — the kanban must re-render instantly from cached snapshot.

## Out of scope

- **Config gate optimisation** (approach C from brainstorming): injecting `local_only` via Tauri's `initialization_script` to eliminate the `/api/config` round-trip. Future work; tracked separately.
- **Remote provider implementation**: the Electric / `useShape` / `tanstack/react-db` infrastructure inside `RemoteProjectProvider` / `RemoteOrgProvider` is untouched behaviourally. The only remote-side edits are (a) the file renames and (b) replacing the 11 + 1 inline lookup helpers with imports from `shared/projectLookups.ts` / `shared/orgLookups.ts` so both provider implementations share the same lookup logic.
- **Routes other than the kanban**: because the dispatcher swaps the implementation behind the existing `ProjectProvider` / `OrgProvider` names, every current mount site (kanban, issue-detail routes that share `LocalProjectKanban`, and the four command-bar / selection dialogs listed above) picks the Local implementation automatically in local mode. No per-route branching needed, and no other routes or dialogs are added or changed by this work.
- **New mutation endpoints**: every mutation calls an existing REST endpoint (`POST /v1/issues`, `POST /v1/issues/bulk`, `DELETE /v1/issues/:id`, etc.). No new write routes are added.
- **Deep schema changes**: no new tables, no new migrations, no changes to existing columns. Only two read-only route handlers and one new SQL method for the members join.

## Success metrics

- Network panel on kanban cold-load: **2 data requests**, not 10+.
- Time from route navigation to first issue visible: drop from the current ~500–1500 ms in local mode to ~50–200 ms (roughly one RTT plus React render).
- Zero observable behaviour change on cloud/remote deployments.
- Optimistic mutation latency: target "same frame" for drag-drop and click-to-toggle operations (< 16 ms).

## Risk register

- **Cache shape drift**: the snapshot endpoint returns a shape slightly different from the ten individual fallback endpoints the remote path uses today. Mitigation: the local providers are the only consumers of the snapshot shape; the existing `OrgContext` / `ProjectContext` interfaces insulate the rest of the codebase.
- **Mutation endpoint divergence**: the REST endpoints used for mutations today were written for Electric's read-sync model. Mitigation: verify with manual smoke (step 3 and 4 above) that the server's post-mutation responses contain the full row; fall back to a snapshot invalidation if not.
- **Organisation id discovery**: `LocalOrgProvider` and `LocalProjectProvider` receive `organizationId` / `projectId` as props from their current mount site (`ProjectKanban.tsx:291-327`, which resolves the org id via `useFindProjectById` → `useOrganizationStore.selectedOrgId` or `useUserOrganizations()[0].id`). Fresh installs without any org row would already fail before reaching the provider. The dispatcher adds no new discovery logic.
- **React Query version lock**: the project already uses `@tanstack/react-query` (same family as `@tanstack/react-db`). Confirm the version in `packages/web-core/package.json` supports `refetchInterval` + `staleTime` the way we use them. If it does not, bump before starting.
