# VK MCP Workspace Mode — Design Spec (v4.2)

**Status:** Draft · 2026-04-21
**Supersedes:** Extends `2026-04-20-vk-mcp-extensions-design.md` (v4.1, Tier A++, PR-X1..X4)
**Scope:** One PR (6 sequential commits)

## Goal

Make the "AI in a VK workspace chat window completes an issue end-to-end" B-path **work out of the box** after a fresh install, with a **self-sufficient tool set** (register repos, create tags, manage tasks, spawn child workspaces) and **scope-safe default behavior** (mutations outside the current workspace scope are rejected).

v4.1 landed the backend plumbing (atomic task+workspace creation, scope relaxation for parent→children, transactional cascade on task deletion, breadcrumb UI). v4.2 closes the **configuration** gap (default mode is currently `Global`, which disables CWD auto-scope) and the **tool-surface** gap (`add_repo`, `create_tag`, `delete_repo`, `update_tag`, `delete_tag` are missing; `list_repos` returns only `{id, name}` — insufficient for AI to identify "the repo I'm standing in").

## Non-Goals

- Rework the existing Orchestrator mode router (keeps its lean 14-tool surface for internal VK subagent spawning — no regression).
- `git init` flows (the "create a brand-new empty repo" UX stays in the GUI; `add_repo` registers existing git repos only).
- `get_repo_remotes` / surfacing `git remote -v` (not required for B-path; defer).
- Auto-migration of existing users' `~/.claude.json` (forward-compatible only — existing configs keep running in Global mode unchanged).

## Locked Decisions (from brainstorming)

| # | Decision | Choice |
|---|---|---|
| D2-1 | `delete_repo` safety | **Default reject when referenced + `force=true` cascades** (backend has `ON DELETE CASCADE`; MCP gates it with a usage precheck) |
| D2-2 | `add_repo` scope | **Register existing git repos only** (`path + display_name?`). Non-git path → backend returns `error_kind="invalid_repo"` |
| D2-3a | `create_tag` color | **Optional, MCP-side default `#6B7280`** (backend requires `color`; MCP fills the default if omitted) |
| D2-3b | `delete_tag` safety | **Same pattern as `delete_repo`**: default reject when referenced by `issue_tags`, `force=true` cascades |
| D2-4 | Mode architecture | **New `Workspace` mode = Global tool superset + scope protection**; Orchestrator unchanged; Workspace becomes the new default |
| D2-5 | Docs scope | **Update 2 existing pages + create `docs/integrations/mcp-modes.mdx` reference page** |
| D2-6 | `Workspace` mode with `context=None` | **Graceful fallback (same as Global)** — no CWD match → scope disabled, all tools still work |

## Architecture

### Mode taxonomy (after v4.2)

| Mode | Tool surface | CWD auto-scope | context=None behavior | Launched by |
|---|---|---|---|---|
| `Global` | Full (~40 tools) | ❌ off | Fine (Global never requires context) | Explicit `--mode global` |
| `Workspace` **(new default)** | Full (Global superset + 5 new tools) | ✅ on | Graceful — scope disabled, all tools still callable | Default (`--mcp` with no args → `--mode workspace`); `default_mcp.json` baseline |
| `Orchestrator` | Lean 14 tools (unchanged from v4.1) | ✅ on | Hard failure (unchanged — VK internal path must have context) | VK internal subagent spawning |
| `CursorBridge` | Unchanged | — | Unchanged | VK Cursor integration |
| `SessionPlaceholder` | Unchanged | — | Unchanged | VK session bootstrap |

### Scope-protection rule (applies to Workspace + Orchestrator)

- **Read-only tools** (`list_*`, `get_*`) do **not** perform scope checks — AI must be able to discover resources before it can work on them.
- **Mutations on workspace-ID-bearing tools** (`update_workspace`, `start_workspace`, `create_task(parent_workspace_id=...)`, etc.) call `check_scope_allows_workspace` — reject if target is not the scoped workspace or a descendant via parent-chain lookup.
- **Mutations with implicit workspace** (sessions, repo scripts) inherit scope from the resource they operate on; checked the same way.
- `Global` mode: check always returns `true` (no scope in effect).
- `Workspace` mode with `context=None` (AI launched outside any VK worktree): check returns `true` — equivalent to Global for this session.

### Why a new mode instead of "make Orchestrator fat"?

1. **Semantic separation.** `Orchestrator` is now defined as "VK spawns an internal subagent bound to one parent task" — a lean, controlled contract. `Workspace` is "a human-directed AI working out of a VK worktree." Mixing both into one router long-term causes governance drift.
2. **Zero regression.** Orchestrator's 14-tool surface came from explicit governance (PR-X3 D12). Expanding it in place would require re-justifying every excluded tool.
3. **Natural naming.** `--mode workspace` reads as "I am in a workspace" — exactly the user's mental model when they open an AI chat in a VK worktree.

## Tool Surface Changes

### New tools (5)

#### `add_repo(path, display_name?)`

- **File:** `crates/mcp/src/task_server/tools/repos.rs`
- **Backend:** `POST /api/repos` with `RegisterRepoRequest { path, display_name? }` (already exists, see `crates/server/src/routes/repo.rs:370`)
- **Behavior:**
  - `path`: absolute path to an existing git repository (required).
  - `display_name`: optional; backend derives from folder name if omitted.
  - Non-git path → backend returns 4xx with `error_kind="invalid_repo"` (forward-compat: surfaces via existing `envelope_to_error` machinery).
  - Returns `{ id, name, display_name, path }`.

#### `delete_repo(repo_id, force?)`

- **File:** `crates/mcp/src/task_server/tools/repos.rs`
- **Backend:** `DELETE /api/repos/:id` (already exists; FK cascades `project_repos`/`attempt_repos`/`execution_process_repo_states`/`merges` per `20251209000000_add_project_repositories.sql`)
- **Behavior:**
  1. **Usage precheck.** `GET /api/repos/:id/usage` (new endpoint — see Backend Additions below) returns `{ project_repos_count, attempt_repos_count, merges_count }`.
  2. If any count > 0 and `force != true` → reject with `error_kind="repo_in_use"` and `error_data={ usage: { … }, hint: "pass force=true to cascade-delete references" }`.
  3. Otherwise call backend `DELETE`.
  - Returns `{ success: true, repo_id }`.

#### `create_tag(project_id?, name, color?)`

- **File:** `crates/mcp/src/task_server/tools/remote_tags.rs` **(new module, split from `issue_tags.rs`)**
- **Backend:** `POST /api/v1/tags` with `CreateTagRequest { id?, project_id, name, color }` (`crates/server/src/routes/v1.rs:738`)
- **Behavior:**
  - `project_id` optional — falls back to `context.project_id` (same pattern as `list_tags`). If neither is available → `ToolError("project_id is required …")`.
  - `color` optional — MCP fills `"#6B7280"` (neutral gray) if omitted.
  - Returns `{ id, project_id, name, color }`.
  - **Scope check:** when in Workspace/Orchestrator mode with a scoped workspace, verify that the resolved `project_id` matches `context.project_id` (prevents cross-project tag creation).

#### `update_tag(tag_id, name?, color?)`

- **File:** `crates/mcp/src/task_server/tools/remote_tags.rs`
- **Backend:** `PATCH /api/v1/tags/:id` with `UpdateTagRequest { name?, color? }` (`v1.rs:756`)
- **Behavior:**
  - At least one of `name` / `color` must be provided; otherwise `ToolError("No fields to update")`.
  - Returns the updated tag.

#### `delete_tag(tag_id, force?)`

- **File:** `crates/mcp/src/task_server/tools/remote_tags.rs`
- **Backend:** `DELETE /api/v1/tags/:id` (`v1.rs:782`)
- **Behavior:**
  1. **Usage precheck.** `GET /api/remote/issue-tags?tag_id=...` (already exists) → count references.
  2. If count > 0 and `force != true` → reject with `error_kind="tag_in_use"` and `error_data={ issue_tag_count: N, hint: "pass force=true to cascade-delete issue-tag links" }`.
  3. Otherwise call backend `DELETE`. (The `issue_tags` FK to `remote_tags` cascades, so the relation rows go with the tag.)
  - Returns `{ success: true, tag_id }`.

### Extended tool (1)

#### `list_repos` — response field extension

```rust
// Before
struct McpRepoSummary { id: String, name: String }

// After
struct McpRepoSummary {
    id: String,
    name: String,
    display_name: String,
    path: String,      // absolute filesystem path
}
```

`count` and wrapper struct unchanged. AI can now correlate CWD → repo without a second `get_repo` call per entry.

### Unchanged tools

Every other existing MCP tool keeps its current signature and behavior. No breaking changes to `create_task`, `create_and_start_task`, `start_workspace`, session tools, remote issue tools, etc.

## Router Composition

```rust
// crates/mcp/src/task_server/tools/mod.rs

impl McpServer {
    pub fn global_mode_router() -> ToolRouter<Self> {
        // unchanged from v4.1, plus the 5 new tools registered via their modules
        Self::context_tools_router()
            + Self::workspaces_tools_router()
            + Self::organizations_tools_router()
            + Self::repos_tools_router()           // now also contains add_repo, delete_repo
            + Self::remote_projects_tools_router()
            + Self::remote_issues_tools_router()
            + Self::issue_assignees_tools_router()
            + Self::issue_tags_tools_router()      // issue-relation tools stay here
            + Self::remote_tags_tools_router()     // NEW: create_tag, update_tag, delete_tag, list_tags (moved)
            + Self::issue_relationships_tools_router()
            + Self::task_attempts_tools_router()
            + Self::session_tools_router()
    }

    pub fn workspace_mode_router() -> ToolRouter<Self> {
        // Workspace = Global superset. Scope protection lives inside each
        // mutation tool (not in the router layer), gated by McpMode.
        Self::global_mode_router()
    }

    pub fn orchestrator_mode_router() -> ToolRouter<Self> {
        // UNCHANGED — continues to be the lean 14-tool surface from v4.1 D12
        // (context + workspaces minus list/delete + sessions + tasks)
        let mut router = Self::context_tools_router()
            + Self::workspaces_tools_router()
            + Self::session_tools_router();
        router.remove_route("list_workspaces");
        router.remove_route("delete_workspace");
        router += Self::tasks_tools_router();
        router
    }
}
```

**Note on `list_tags`:** currently in `issue_tags.rs`. It moves to `remote_tags.rs` alongside the new CRUD tools (tags are the project-scoped entity, issue_tags are the join relation — cleaner separation). `list_issue_tags`, `add_issue_tag`, `remove_issue_tag` stay in `issue_tags.rs`.

## Mode Dispatch

### `crates/executors/default_mcp.json` — default args flip

```diff
 {
   "vibe_kanban": {
     "command": "npx",
-    "args": ["-y", "vibe-kanban@latest", "--mcp"]
+    "args": ["-y", "vibe-kanban@latest", "--mcp", "--mode", "workspace"]
   }
 }
```

### `npx-cli/src/cli.ts` — CLI default

```diff
 function buildMcpArgs(args: string[]): string[] {
-  return args.length > 0 ? args : ["--mode", "global"];
+  return args.length > 0 ? args : ["--mode", "workspace"];
 }
```

### `crates/mcp/src/bin/vibe_kanban_mcp.rs` — new mode variant

```rust
#[derive(Debug, Clone, ValueEnum)]
pub enum McpLaunchMode {
    Global,
    Workspace,         // NEW
    Orchestrator,
    CursorBridge,
    SessionPlaceholder,
}

// main() match:
McpLaunchMode::Workspace => {
    run(McpServer::new_workspace(&url).init().await?).await
}
```

### `crates/mcp/src/task_server/mod.rs` — server construction + graceful fallback

```rust
pub enum McpMode {
    Global,
    Workspace,         // NEW
    Orchestrator,
}

impl McpServer {
    pub fn new_workspace(base_url: &str) -> Self {
        let client = reqwest::Client::new();
        Self {
            api_client: api_client::ApiClient::new(client.clone(), base_url),
            client,
            base_url: base_url.to_string(),
            tool_router: Self::workspace_mode_router(),
            context: None,
            mode: McpMode::Workspace,
        }
    }
}

// fetch_context_at_startup (graceful fallback for Workspace matches Global):
match self.try_fetch_attempt_context(&normalized_path).await {
    Ok(Some(ctx)) => Ok(Some(self.build_mcp_context_from_workspace_context(&ctx).await)),
    Ok(None) | Err(_)
        if matches!(self.mode(), McpMode::Global | McpMode::Workspace) => Ok(None),
    Ok(None) => anyhow::bail!(
        "Failed to load orchestrator MCP context from /api/containers/attempt-context"
    ),
    Err(error) => Err(error.context("Failed to load orchestrator MCP context")),
}
```

### `check_scope_allows_workspace` — extended to Workspace mode

```rust
pub(crate) async fn check_scope_allows_workspace(
    server: &McpServer,
    scope_cache: &mut HashMap<Uuid, bool>,
    target: Uuid,
) -> bool {
    // Global: no scope → always allow
    if matches!(server.mode(), McpMode::Global) {
        return true;
    }
    // Workspace + Orchestrator: scope check runs, but None scope → allow-all
    let scoped = match server.scoped_workspace_id() {
        Some(x) => x,
        None => return true,  // Workspace graceful fallback; Orchestrator
                              // reaches here only via test paths
    };
    // ...rest unchanged (self / child task walk / cache)
}
```

## Backend Additions

### `GET /api/repos/:id/usage` (new)

Returns the reference counts `delete_repo` needs for its precheck. Implementation in `crates/server/src/routes/repo.rs`:

```rust
#[derive(Debug, Serialize, TS)]
pub struct RepoUsage {
    pub project_repos: i64,
    pub attempt_repos: i64,
    pub merges: i64,
    pub execution_process_repo_states: i64,
}

async fn get_repo_usage(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<RepoUsage>>, ApiError> {
    let pool = deployment.db().pool.clone();
    let usage = RepoUsage {
        project_repos: sqlx::query_scalar("SELECT COUNT(*) FROM project_repos WHERE repo_id = ?1")
            .bind(id).fetch_one(&pool).await?,
        attempt_repos: sqlx::query_scalar("SELECT COUNT(*) FROM attempt_repos WHERE repo_id = ?1")
            .bind(id).fetch_one(&pool).await?,
        merges: sqlx::query_scalar("SELECT COUNT(*) FROM merges WHERE repo_id = ?1")
            .bind(id).fetch_one(&pool).await?,
        execution_process_repo_states: sqlx::query_scalar(
            "SELECT COUNT(*) FROM execution_process_repo_states WHERE repo_id = ?1"
        ).bind(id).fetch_one(&pool).await?,
    };
    Ok(ResponseJson(ApiResponse::success(usage)))
}
```

Routed in `crates/server/src/routes/repo.rs` router block:

```rust
.route("/repos/{id}/usage", get(get_repo_usage))
```

### `GET /api/remote/tags/:id/usage` (new)

Parallel to `get_repo_usage`. Returns the `issue_tags` reference count for a given tag. Added because the existing `GET /api/remote/issue-tags` endpoint's filter contract (only `issue_id` is documented) is not guaranteed to cover `tag_id`; introducing a dedicated usage endpoint avoids coupling `delete_tag` to an undocumented query filter.

```rust
#[derive(Debug, Serialize, TS)]
pub struct TagUsage {
    pub issue_tags: i64,
}

async fn get_tag_usage(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<TagUsage>>, ApiError> {
    let pool = deployment.db().pool.clone();
    let issue_tags: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM issue_tags WHERE tag_id = ?1"
    ).bind(id).fetch_one(&pool).await?;
    Ok(ResponseJson(ApiResponse::success(TagUsage { issue_tags })))
}
```

Mounted in the `remote` routes module (alongside existing `/api/remote/tags` reads):

```rust
.route("/tags/{id}/usage", get(get_tag_usage))
```

## ApiClient Additions

```rust
// crates/mcp/src/task_server/api_client.rs

impl ApiClient {
    pub async fn register_repo(
        &self,
        path: &str,
        display_name: Option<&str>,
    ) -> Result<Repo, ApiError> { /* POST /api/repos */ }

    pub async fn get_repo_usage(&self, repo_id: Uuid) -> Result<RepoUsage, ApiError> {
        /* GET /api/repos/:id/usage */
    }

    pub async fn delete_repo(&self, repo_id: Uuid) -> Result<(), ApiError> {
        /* DELETE /api/repos/:id */
    }

    pub async fn create_tag(
        &self,
        project_id: Uuid,
        name: &str,
        color: &str,
    ) -> Result<Tag, ApiError> { /* POST /api/v1/tags */ }

    pub async fn update_tag(
        &self,
        tag_id: Uuid,
        name: Option<&str>,
        color: Option<&str>,
    ) -> Result<Tag, ApiError> { /* PATCH /api/v1/tags/:id */ }

    pub async fn delete_tag(&self, tag_id: Uuid) -> Result<(), ApiError> {
        /* DELETE /api/v1/tags/:id */
    }

    pub async fn get_tag_usage(
        &self,
        tag_id: Uuid,
    ) -> Result<TagUsage, ApiError> {
        /* GET /api/remote/tags/:id/usage → { issue_tags: i64 } */
    }
}
```

## Testing Strategy

All new tests use the existing `httpmock` + `with_scope_for_test` pattern established in PR-X3 (`crates/mcp/src/task_server/tools/mod.rs` `check_scope_tests` module).

| Test module | Tests |
|---|---|
| `task_server/mod.rs` | `workspace_mode_graceful_on_missing_context` — assert `init()` succeeds with `context=None` for Workspace (parallels existing `orchestrator_scope_requires_context_when_missing`) |
| `task_server/tools/mod.rs` (router tests) | `workspace_mode_exposes_full_tool_surface` — assert Workspace router tool set ⊇ Global router tool set and includes the 5 new tools |
| `task_server/tools/mod.rs` (check_scope_tests) | `workspace_mode_with_scope_rejects_unrelated` / `workspace_mode_none_context_allows_all` / `workspace_mode_allows_child_workspace` |
| `task_server/tools/repos.rs` | `add_repo_happy_path` / `add_repo_invalid_path_surfaces_error_kind` / `delete_repo_rejects_in_use` / `delete_repo_force_cascades` / `list_repos_returns_path_and_display_name` |
| `task_server/tools/remote_tags.rs` | `create_tag_defaults_color_when_omitted` / `create_tag_respects_caller_color` / `create_tag_rejects_cross_project_in_scope` / `update_tag_requires_at_least_one_field` / `delete_tag_rejects_in_use` / `delete_tag_force_deletes` |

Total: ~14 new unit tests.

## Documentation

### `docs/integrations/vibe-kanban-mcp-server.mdx` — edited

1. Add "Modes at a glance" section near the top (3-column table: Global / Workspace / Orchestrator).
2. Mention that recent `vibe-kanban` releases default to `Workspace` mode. Do not hardcode a version number in the spec (the actual release version is filled in at PR time by release-notes tooling); the doc copy should read "starting with this release" and the PR description links to the release notes.
3. Add "Upgrading from older configs" subsection. Existing configs fall into two shapes:
   - `["-y", "vibe-kanban@latest", "--mcp"]` — implicit; automatically picks up Workspace mode on the next launch without any edit.
   - `["-y", "vibe-kanban@latest", "--mcp", "--mode", "global"]` — explicit; no change. To switch, replace `global` with `workspace`.

### `docs/integrations/mcp-server-configuration.mdx` — edited

Update every per-agent example (`Claude`, `Cursor`, `Codex`, `Gemini`, `Amp`, `Opencode`, `Copilot`) to show `"--mcp", "--mode", "workspace"` as the recommended default.

Add a top-of-page callout:
> **What mode should I use?** New installs default to `workspace`, which pins the MCP server to the VK worktree your agent was launched from. For cross-workspace admin scripts or CI, use `global`. See [MCP Modes Reference](./mcp-modes).

### `docs/integrations/mcp-modes.mdx` — **new file**

Full reference:

- **Global** — no CWD auto-scope; every tool available; no safety pin. Use for admin scripts, CI, or when your agent may work across arbitrary workspaces.
- **Workspace** — CWD auto-scope when launched from a VK worktree; gracefully falls back to Global-equivalent when the CWD doesn't match a workspace. Full tool surface. This is the **new default** and matches "AI in a VK chat window" intent.
- **Orchestrator** — VK-internal only, pinned to one parent task, 14-tool lean surface. Not user-facing.

Include:

- Per-agent `args` templates side-by-side (`--mode workspace` vs `--mode global`)
- Scope protection semantics (which mutations are gated)
- Full tool list per mode (auto-generated checklist would be ideal, but manual is fine for v4.2)
- "When does scope protection kick in?" decision tree

### `docs.json` — navigation

Insert new page under the `Integrations` group, immediately after `mcp-server-configuration`.

## PR Structure

**Single PR, 6 sequential commits.** Each commit leaves the tree in a green state (`cargo check`, `cargo test`, `pnpm run check`, `pnpm run lint`) and the `style: apply prettier` convention follows prior PRs.

| Commit | Title | Scope |
|---|---|---|
| 1 | `feat(mcp): introduce Workspace launch mode with graceful context fallback` | `McpMode::Workspace` enum + `new_workspace()` constructor + `fetch_context_at_startup` Workspace branch + `check_scope_allows_workspace` extension; 3–4 scope/init tests; **no tool-surface change yet** |
| 2 | `feat(mcp): wire Workspace router and CLI dispatch` | `workspace_mode_router` (= global_mode_router for now) + `bin/vibe_kanban_mcp.rs` dispatch + router-surface test |
| 3 | `feat(mcp): extend list_repos with path and display_name` | `McpRepoSummary` struct expansion + `list_repos_returns_path_and_display_name` test; regenerate `shared/types.ts` via `pnpm run generate-types` |
| 4 | `feat(mcp): add add_repo and delete_repo with usage precheck` | New tools + `GET /api/repos/:id/usage` backend endpoint + `ApiClient` methods + 5 tests |
| 5 | `feat(mcp): split remote_tags module; add create/update/delete_tag` | New `remote_tags.rs` module + move `list_tags` from `issue_tags.rs` + 3 new tools + `GET /api/remote/tags/:id/usage` backend endpoint + `ApiClient` methods + 6 tests |
| 6 | `feat(config,docs): flip default MCP mode to Workspace and document modes` | `default_mcp.json` + `npx-cli/src/cli.ts` + 3 docs files (`vibe-kanban-mcp-server.mdx`, `mcp-server-configuration.mdx`, new `mcp-modes.mdx`) + `docs.json` navigation |

**Why single PR:** Commits 1-5 are backend-facing and each is individually reviewable; commit 6 is the single user-visible "flip the switch" atomic change that reviewers and release-notes authors care about. Splitting into multiple PRs would fragment review context without reducing risk (each commit already leaves the tree green).

**Rollback strategy:** `git revert <commit-6>` alone restores the pre-v4.2 default behavior for new installs without losing any of the new tools (they'd still be reachable via `--mode workspace` or by sticking them into `--mode global`).

## Compatibility & Migration

### Existing users

VK never rewrites `~/.claude.json`; changes to the npx-cli default only take effect on the **next agent launch** and apply via the CLI flow, not by file mutation.

The `buildMcpArgs` helper in `npx-cli/src/cli.ts` returns a default **only when the incoming arg list after `--mcp` is empty**. Two cases cover every existing config:

1. **Implicit config** (`args: ["-y", "vibe-kanban@latest", "--mcp"]`, nothing after `--mcp`):
   - After the flip, `buildMcpArgs([])` returns `["--mode", "workspace"]`.
   - On next launch these users run in Workspace mode automatically.
2. **Explicit config** (`args: ["-y", "vibe-kanban@latest", "--mcp", "--mode", "global"]`):
   - `buildMcpArgs(["--mode", "global"])` is a no-op — the user's explicit choice is respected, and they stay on Global.

Case 1 is **non-breaking** because Workspace mode is a strict superset of Global:

- Identical tool surface (Workspace router = Global router plus the 5 new tools from this PR).
- Graceful fallback when the agent's CWD isn't inside a VK worktree (scope disabled, every tool callable — indistinguishable from Global for that session).
- The only new behavior a user can observe is a scope-denied error when their agent, running inside a VK worktree, tries to mutate a *different* workspace — which is a safety win, not a capability loss.

### Agents launched outside VK worktrees

- MCP server starts → `fetch_context_at_startup` → `/api/containers/attempt-context?container_ref=<CWD>` returns 404/None → `context = None` → graceful fallback → all tools available, scope checks return `true`.
- Indistinguishable from Global mode for this session.

### Orchestrator mode

- Unchanged. VK's internal subagent spawning code (`executors/…`) still uses `new_orchestrator` with the lean 14-tool surface.
- `check_scope_allows_workspace`'s new Workspace branch is an additive match arm; the Orchestrator branch is untouched.

### Schema regeneration

- `RepoUsage` (new TS type) is regenerated via `pnpm run generate-types` in commit 4.
- `TagUsage` (new TS type) is regenerated via `pnpm run generate-types` in commit 5.
- No remote-types impact (`remote_tags` mutations are already in `v1.rs` and their DTOs are in `api-types`).

## Risks & Open Questions

**Risk 1:** Users who assumed Global behavior might see unexpected scope-denied errors when their AI tries to operate on workspaces other than where it was launched. **Mitigation:** commit 6 docs call this out explicitly; the error payload includes a `hint` field pointing to `--mode global` for admin workflows.

**Risk 2:** `list_repos` response size grows (+2 fields). For orgs with hundreds of repos this is still small compared to `get_repo`. **Mitigation:** accept — the UX win is large, the bandwidth cost is negligible.

**Risk 3:** `create_tag` auto-color `#6B7280` may clash with an existing "gray" tag. **Mitigation:** AI can inspect `list_tags` response and pass a specific color; default is a backstop, not an opinion.

**Open question (none — all answered during brainstorming):** N/A.

## Success Criteria

- A fresh `vibe-kanban` install lets the user open an AI chat in any VK worktree and successfully call `create_and_start_task`, `add_repo` (for a repo VK doesn't know about yet), and `create_tag` (for a fresh label) without editing any config file by hand.
- Users who had a working VK MCP setup before v4.2 get Workspace mode automatically on the next agent launch, see no capability regression, and gain scope-safety by default.
- `cargo test --workspace`, `pnpm run check`, `pnpm run lint` all green on each commit.
- Final code review (same rigor as v4.1) yields no blocking findings.
