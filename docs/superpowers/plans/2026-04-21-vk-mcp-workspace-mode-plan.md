# VK MCP Workspace Mode Implementation Plan (v4.2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the "AI in a VK workspace chat window completes an issue end-to-end" path work out of the box after a fresh install, with a self-sufficient tool surface (add_repo, delete_repo, create_tag, update_tag, delete_tag, list_repos with path/display_name) and scope-safe default behavior via a new `Workspace` MCP mode that becomes the default.

**Architecture:** Introduce an `McpMode::Workspace` variant that shares Global's full tool surface but adds CWD-based auto-scope with graceful fallback when no VK worktree is detected. The 5 new tools live in `repos.rs` (add/delete) and a new `remote_tags.rs` module (create/update/delete, plus relocated `list_tags`). `delete_repo` and `delete_tag` gate destructive calls through a 409-then-`?force=true` backend handshake — no new precheck endpoints needed (existing `delete_repo` already returns 409 with active workspaces; `delete_tag` gets an issue_tags count check added). The default launch args flip (`default_mcp.json` + `npx-cli/src/cli.ts`) so fresh installs get Workspace mode; existing implicit configs upgrade transparently, explicit `--mode global` users keep their choice.

**Tech Stack:** Rust (rmcp, schemars, reqwest, sqlx, axum, ts-rs), TypeScript (pnpm, Vite), Mintlify MDX docs.

---

## Deviations from Spec

The spec's **"Backend Additions"** section proposes two new `/usage` endpoints:

- `GET /api/repos/:id/usage`
- `GET /api/remote/tags/:id/usage`

Both are **dropped from this plan**. Rationale:

1. `DELETE /api/repos/:id` at `crates/server/src/routes/repo.rs:343` **already returns `409 CONFLICT`** with `DeleteRepoConflict { message, workspaces: Vec<String> }` when active workspaces reference the repo. The MCP tool surfaces that error payload via the existing `envelope_to_error` → `ToolError::error_data` path. No separate precheck is needed.
2. For tags, we add the same pattern **at the backend**: the existing `delete_tag` at `crates/server/src/routes/v1.rs:782` gains an `issue_tags` reference-count check that returns `409 CONFLICT` with `DeleteTagConflict { message, issue_tag_count }` when non-zero. A `?force=true` query param bypasses the check.
3. For parity, `delete_repo` gets the same `?force=true` query param to let AI explicitly opt into deleting a repo with active workspaces.

**AI handshake (identical UX to the spec's two-step flow):**

1. AI calls `delete_repo(repo_id)` or `delete_tag(tag_id)`.
2. Backend returns `409` with structured `error_data` listing what blocks the delete.
3. AI decides: abort, or retry with `force=true` (for tags: cascades issue_tag rows via FK; for repos: unlinks active workspaces and deletes).

**Schema impact:** Plan registers one new TS type (`DeleteTagConflict`) via `pnpm run generate-types`. No other backend/TS type changes beyond what the spec required.

**What is preserved from spec:** Every other decision — mode architecture, router composition, tool surface shape, mode dispatch, docs outline, PR/commit structure, testing strategy.

---

## File Structure

Files this plan creates or modifies, grouped by responsibility.

**New files:**
- `crates/mcp/src/task_server/tools/remote_tags.rs` — new tools module (create_tag, update_tag, delete_tag, relocated list_tags).
- `docs/integrations/mcp-modes.mdx` — mode reference page.

**Modified files:**

MCP server:
- `crates/mcp/src/bin/vibe_kanban_mcp.rs` — add `Workspace` variant, dispatch branch, help text.
- `crates/mcp/src/task_server/mod.rs` — `McpMode::Workspace`, `new_workspace` constructor, graceful-fallback branch in `fetch_context_at_startup`.
- `crates/mcp/src/task_server/tools/mod.rs` — `workspace_mode_router`, extend `check_scope_allows_workspace` to Workspace mode, declare new `remote_tags` module, router-surface tests.
- `crates/mcp/src/task_server/tools/repos.rs` — extend `McpRepoSummary` with `display_name` + `path`, add `add_repo`, add `delete_repo`.
- `crates/mcp/src/task_server/tools/issue_tags.rs` — **remove** `list_tags` (moves to `remote_tags.rs`).
- `crates/mcp/src/task_server/api_client.rs` — `register_repo`, `delete_repo`, `create_tag`, `update_tag`, `delete_tag` methods.

Backend:
- `crates/server/src/routes/repo.rs` — accept `?force=true` on `DELETE /api/repos/:id`.
- `crates/server/src/routes/v1.rs` — accept `?force=true` on `DELETE /v1/tags/:id` + 409 with `DeleteTagConflict` when referenced.

Config & CLI:
- `crates/executors/default_mcp.json` — args flip to `["--mcp", "--mode", "workspace"]`.
- `npx-cli/src/cli.ts` — default `buildMcpArgs` to Workspace.

Docs:
- `docs/integrations/vibe-kanban-mcp-server.mdx` — add "Modes at a glance" + "Upgrading from older configs".
- `docs/integrations/mcp-server-configuration.mdx` — update per-agent examples + callout.
- `docs/docs.json` — new nav entry.

Generated (do not hand-edit):
- `shared/types.ts` — regenerated via `pnpm run generate-types` after backend type additions.

---

## Task 1: Workspace mode foundation (enum variant, constructor, graceful fallback, scope check)

Introduces the `McpMode::Workspace` variant, `new_workspace` constructor, graceful-fallback branch in `fetch_context_at_startup`, and extends `check_scope_allows_workspace` to treat Workspace like Orchestrator for scope checks. **No tool-surface change yet** — routing still uses the global router (the variant just exists).

**Files:**
- Modify: `crates/mcp/src/task_server/mod.rs` (around lines 44-48 and 129-144)
- Modify: `crates/mcp/src/task_server/tools/mod.rs` (around line 216-246)
- Test: `crates/mcp/src/task_server/tools/mod.rs` (existing `check_scope_tests` module, line 867)

### Steps

- [ ] **Step 1.1: Write a failing test for Workspace graceful fallback in scope check**

In `crates/mcp/src/task_server/tools/mod.rs` within the `check_scope_tests` module (after the existing `cache_short_circuits_second_call` test, around line 1022), add:

```rust
#[tokio::test]
async fn workspace_mode_with_none_scope_allows_all() {
    install_rustls();
    let mock_server = MockServer::start();
    // Any backend call would fail the test — assert zero hits.
    let catch_all = mock_server.mock(|when, then| {
        when.any_request();
        then.status(500);
    });

    let server = McpServer::new_workspace(&mock_server.base_url());
    // context is None by default; scope check must short-circuit to true.
    let mut cache = HashMap::new();
    assert!(check_scope_allows_workspace(&server, &mut cache, Uuid::new_v4()).await);
    assert_eq!(catch_all.hits(), 0);
}

#[tokio::test]
async fn workspace_mode_rejects_unrelated_when_scoped() {
    install_rustls();
    let mock_server = MockServer::start();
    let scope = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let child = Uuid::new_v4();
    let other_parent = Uuid::new_v4();

    mock_server.mock(|when, then| {
        when.path(format!("/api/workspaces/{child}"));
        then.status(200)
            .json_body(ws_envelope(child, Some(task_id)));
    });
    mock_server.mock(|when, then| {
        when.path(format!("/api/tasks/{task_id}"));
        then.status(200)
            .json_body(task_envelope(task_id, Some(other_parent)));
    });

    let server =
        McpServer::new_workspace(&mock_server.base_url()).with_scope_for_test(scope);
    let mut cache = HashMap::new();
    assert!(!check_scope_allows_workspace(&server, &mut cache, child).await);
}
```

- [ ] **Step 1.2: Run tests to verify failure**

Run: `cargo test -p mcp --lib check_scope_tests::workspace_ -- --nocapture`
Expected: FAIL with "no function or associated item named `new_workspace`" and "no variant named `Workspace`" in `McpMode`.

- [ ] **Step 1.3: Add `Workspace` variant to `McpMode`**

In `crates/mcp/src/task_server/mod.rs`, replace the enum (currently at lines 44-48):

```rust
#[derive(Debug, Clone)]
pub enum McpMode {
    Global,
    Workspace,
    Orchestrator,
}
```

- [ ] **Step 1.4: Add `new_workspace` constructor**

In `crates/mcp/src/task_server/mod.rs`, immediately after the existing `new_orchestrator` method (ends at line 83), add:

```rust
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
```

This references `Self::workspace_mode_router()`, which doesn't exist yet — Task 2 introduces it. For this task, temporarily alias it to the global router by adding (in the same `impl McpServer` block, directly under the constructor):

```rust
// Temporary alias until Task 2 introduces the real workspace router.
// Remove this shim in Task 2 when `workspace_mode_router` is defined
// in `tools/mod.rs`.
#[doc(hidden)]
fn workspace_mode_router() -> rmcp::handler::server::tool::ToolRouter<Self> {
    Self::global_mode_router()
}
```

> Plan execution note: Task 2 will remove this shim and move the real definition next to `global_mode_router` in `tools/mod.rs`. Keep the `#[doc(hidden)]` and the inline comment exactly as shown so the replacement is a clean find-and-delete.

- [ ] **Step 1.5: Extend graceful-fallback branch in `fetch_context_at_startup`**

In `crates/mcp/src/task_server/mod.rs`, replace the `match` block at lines 134-143:

```rust
match self.try_fetch_attempt_context(&normalized_path).await {
    Ok(Some(ctx)) => Ok(Some(
        self.build_mcp_context_from_workspace_context(&ctx).await,
    )),
    Ok(None) | Err(_)
        if matches!(self.mode(), McpMode::Global | McpMode::Workspace) =>
    {
        Ok(None)
    }
    Ok(None) => anyhow::bail!(
        "Failed to load orchestrator MCP context from /api/containers/attempt-context"
    ),
    Err(error) => Err(error.context("Failed to load orchestrator MCP context")),
}
```

- [ ] **Step 1.6: Extend `check_scope_allows_workspace` to Workspace mode**

In `crates/mcp/src/task_server/tools/mod.rs`, replace the first match at line 221 (inside `check_scope_allows_workspace`):

```rust
pub(crate) async fn check_scope_allows_workspace(
    server: &McpServer,
    scope_cache: &mut HashMap<Uuid, bool>,
    target: Uuid,
) -> bool {
    // Global has no scope at all.
    if matches!(server.mode(), McpMode::Global) {
        return true;
    }
    // Workspace + Orchestrator: scope check runs, but a missing scope
    // (Workspace graceful fallback; Orchestrator test paths) is allow-all.
    let scoped = match server.scoped_workspace_id() {
        Some(x) => x,
        None => return true,
    };
    if target == scoped {
        return true;
    }
    if let Some(cached) = scope_cache.get(&target) {
        return *cached;
    }

    let allowed = async {
        let ws = server.api().get_workspace(target).await.ok()?;
        let tid = ws.task_id?;
        let t = server.api().get_task(tid).await.ok()?;
        Some(t.parent_workspace_id == Some(scoped))
    }
    .await
    .unwrap_or(false);

    scope_cache.insert(target, allowed);
    allowed
}
```

- [ ] **Step 1.7: Run tests to verify pass**

Run: `cargo test -p mcp --lib check_scope_tests -- --nocapture`
Expected: PASS all five tests (four existing + two new).

Run: `cargo test -p mcp --lib` (full crate)
Expected: PASS. Note: the existing `orchestrator_scope_requires_context_when_missing` test still passes because its mode is `Orchestrator` and scope is `None` → the new allow-all branch applies consistently.

- [ ] **Step 1.8: Add Workspace graceful-fallback init test**

In `crates/mcp/src/task_server/mod.rs`, add at the bottom of the file (create a `#[cfg(test)] mod tests` block if one doesn't exist — currently there isn't one in this file; the test file is adjacent):

```rust
#[cfg(test)]
mod init_tests {
    use super::*;
    use httpmock::MockServer;

    fn install_rustls() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    #[tokio::test]
    async fn workspace_mode_init_is_graceful_when_cwd_unmatched() {
        install_rustls();
        let mock_server = MockServer::start();
        // Backend returns 404 for the attempt-context lookup — mimics
        // "AI launched outside any VK worktree".
        mock_server.mock(|when, then| {
            when.path("/api/containers/attempt-context");
            then.status(404);
        });

        let server = McpServer::new_workspace(&mock_server.base_url());
        let initialized = server.init().await.expect("init must not fail");
        assert!(initialized.context.is_none());
        assert!(matches!(initialized.mode(), McpMode::Workspace));
    }

    #[tokio::test]
    async fn orchestrator_mode_init_fails_when_cwd_unmatched() {
        install_rustls();
        let mock_server = MockServer::start();
        mock_server.mock(|when, then| {
            when.path("/api/containers/attempt-context");
            then.status(404);
        });

        let server = McpServer::new_orchestrator(&mock_server.base_url());
        let err = server.init().await.expect_err("init must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Failed to load orchestrator MCP context"),
            "unexpected error message: {msg}"
        );
    }
}
```

- [ ] **Step 1.9: Run init tests**

Run: `cargo test -p mcp --lib init_tests -- --nocapture`
Expected: PASS both tests.

- [ ] **Step 1.10: Format and commit**

```bash
pnpm run format
cd crates/mcp && cargo check && cd -
git add crates/mcp/src/task_server/mod.rs crates/mcp/src/task_server/tools/mod.rs
git commit -m "$(cat <<'EOF'
feat(mcp): introduce Workspace launch mode with graceful context fallback

Adds `McpMode::Workspace` as a superset of Global plus CWD auto-scope.
Init gracefully falls back to scope-disabled when the CWD does not
match a VK worktree, matching Global's UX for that session. Extends
`check_scope_allows_workspace` so Workspace enforces scope when set,
and both Workspace and Orchestrator share the None-scope allow-all
short-circuit.

No tool-surface change yet — the router shim aliases to the global
router and will be replaced in the next commit.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Workspace router + CLI dispatch

Removes the shim from Task 1, defines the real `workspace_mode_router`, and wires `--mode workspace` into the CLI.

**Files:**
- Modify: `crates/mcp/src/task_server/tools/mod.rs` (around line 262-285, router block)
- Modify: `crates/mcp/src/task_server/mod.rs` (remove shim added in Task 1)
- Modify: `crates/mcp/src/bin/vibe_kanban_mcp.rs` (enum + dispatch + help text)
- Test: `crates/mcp/src/task_server/tools/mod.rs` (extend the existing `mod tests` block)
- Test: `crates/mcp/src/bin/vibe_kanban_mcp.rs` (add arg-parsing test)

### Steps

- [ ] **Step 2.1: Write failing router-surface test**

In `crates/mcp/src/task_server/tools/mod.rs`, inside the existing `#[cfg(test)] mod tests { … }` block (around line 564, after `global_mode_keeps_workspace_admin_and_discovery_tools` at line 614), add:

```rust
#[test]
fn workspace_mode_exposes_global_superset() {
    let workspace = tool_names(McpServer::workspace_mode_router());
    let global = tool_names(McpServer::global_mode_router());
    assert!(
        global.is_subset(&workspace),
        "workspace mode must include every global tool; missing: {:?}",
        global.difference(&workspace).collect::<Vec<_>>()
    );
}
```

- [ ] **Step 2.2: Run test to verify failure**

Run: `cargo test -p mcp --lib tests::workspace_mode_exposes_global_superset -- --nocapture`
Expected: PASS (because the shim aliases to global). This test is a guard that will still pass after Task 5 adds the 5 new tools — every global tool stays in workspace.

If it fails, the shim was removed prematurely — restore it.

- [ ] **Step 2.3: Remove shim and add real `workspace_mode_router`**

In `crates/mcp/src/task_server/mod.rs`, remove the `#[doc(hidden)] fn workspace_mode_router` shim (added in Task 1, Step 1.4).

In `crates/mcp/src/task_server/tools/mod.rs`, inside the existing `impl McpServer { … }` block that holds `global_mode_router` (around line 261), add immediately after `global_mode_router`:

```rust
pub fn workspace_mode_router() -> rmcp::handler::server::tool::ToolRouter<Self> {
    // Workspace = Global superset. Scope protection lives inside each
    // mutation tool, gated by McpMode, so the router itself is identical
    // to Global's until Task 5 registers the new repos/tags tools.
    Self::global_mode_router()
}
```

- [ ] **Step 2.4: Run router test**

Run: `cargo test -p mcp --lib tests::workspace_mode_exposes_global_superset -- --nocapture`
Expected: PASS.

- [ ] **Step 2.5: Write failing arg-parser test for `--mode workspace`**

In `crates/mcp/src/bin/vibe_kanban_mcp.rs`, scroll to the existing `#[cfg(test)] mod tests { … }` block (if none exists, create one at the end of the file). Add:

```rust
#[cfg(test)]
mod arg_tests {
    use super::*;

    #[test]
    fn mode_workspace_parses() {
        let cfg = resolve_launch_config_from_iter(
            ["--mode", "workspace"].iter().map(|s| s.to_string()),
        )
        .expect("workspace mode must parse");
        assert_eq!(cfg.mode, McpLaunchMode::Workspace);
    }

    #[test]
    fn default_mode_is_workspace() {
        let cfg = resolve_launch_config_from_iter(std::iter::empty())
            .expect("empty args must default to workspace");
        assert_eq!(cfg.mode, McpLaunchMode::Workspace);
    }

    #[test]
    fn mode_global_still_explicit_opt_in() {
        let cfg = resolve_launch_config_from_iter(
            ["--mode", "global"].iter().map(|s| s.to_string()),
        )
        .expect("global mode must parse");
        assert_eq!(cfg.mode, McpLaunchMode::Global);
    }
}
```

- [ ] **Step 2.6: Run tests to verify failure**

Run: `cargo test -p mcp --bin vibe_kanban_mcp arg_tests -- --nocapture`
Expected: FAIL with "no variant named `Workspace`".

- [ ] **Step 2.7: Add `Workspace` variant to `McpLaunchMode`**

In `crates/mcp/src/bin/vibe_kanban_mcp.rs`, replace the enum at lines 15-35:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
enum McpLaunchMode {
    Global,
    Workspace,
    Orchestrator,
    /// v4 stdio bridge for Cursor IDE's Composer Agent. **Workspace-
    /// agnostic**: a single bridge serves all Composer chats; the
    /// backend routes by `sessionId`. The optional `label` shows up in
    /// the Inbox UI to disambiguate which Cursor window / machine
    /// produced a conversation.
    CursorBridge {
        label: Option<String>,
    },
    /// Long-lived no-op process used as the placeholder OS child for a
    /// `CURSOR_MCP` coding-agent session. The vibe-kanban executor
    /// framework requires a real `SpawnedChild`; this mode lives in that
    /// role and exits cleanly when its parent closes stdin or when an
    /// `EXIT` line is written.
    SessionPlaceholder {
        session_id: uuid::Uuid,
    },
}
```

- [ ] **Step 2.8: Add dispatch branch in `main`**

In `crates/mcp/src/bin/vibe_kanban_mcp.rs`, add an arm to the `match launch_config.mode` block at line 58 (after the `Global` arm ends at line 67):

```rust
McpLaunchMode::Workspace => {
    let base_url = base_url_or_err?;
    let server = McpServer::new_workspace(&base_url);
    let service = server.init().await?.serve(stdio()).await.map_err(|error| {
        tracing::error!("serving error: {:?}", error);
        error
    })?;
    service.waiting().await?;
}
```

- [ ] **Step 2.9: Add "workspace" match arm in `resolve_launch_config_from_iter`**

In `crates/mcp/src/bin/vibe_kanban_mcp.rs`, extend the `match mode_str.as_str()` block at line 184. After the `"global" =>` arm (ends at line 192), add:

```rust
"workspace" => {
    if session_id_arg.is_some() || label_arg.is_some() {
        return Err(anyhow::anyhow!(
            "--session-id / --label are not valid with --mode workspace"
        ));
    }
    McpLaunchMode::Workspace
}
```

- [ ] **Step 2.10: Flip default mode to `workspace`**

In `crates/mcp/src/bin/vibe_kanban_mcp.rs`, at line 180, change `unwrap_or("global")` to `unwrap_or("workspace")`:

```rust
let mode_str = mode_arg
    .as_deref()
    .unwrap_or("workspace")
    .trim()
    .to_ascii_lowercase();
```

> Note: this flip is complementary to the npx-cli default flip in Task 6. CLI default here governs direct `vibe-kanban-mcp` invocations (e.g. when a user launches the binary directly); npx-cli governs agents spawning `npx vibe-kanban@latest --mcp`.

- [ ] **Step 2.11: Update help text**

In `crates/mcp/src/bin/vibe_kanban_mcp.rs` at lines 163-166:

```rust
"-h" | "--help" => {
    println!(
        "Usage:\n  \
         vibe-kanban-mcp --mode <workspace|global|orchestrator>\n  \
         vibe-kanban-mcp --mode cursor-bridge [--label <text>]\n  \
         vibe-kanban-mcp --mode session-placeholder --session-id <UUID>\n\
         \nDefault mode: workspace (scoped to the VK worktree when CWD matches one, else graceful fallback)."
    );
    std::process::exit(0);
}
```

Also update the error message for missing `--mode` value at line 135:

```rust
"--mode" => {
    mode_arg = Some(args.next().ok_or_else(|| {
        anyhow::anyhow!(
            "Missing value for --mode. Expected 'workspace', 'global', 'orchestrator', 'cursor-bridge', or 'session-placeholder'"
        )
    })?);
}
```

- [ ] **Step 2.12: Run all arg tests**

Run: `cargo test -p mcp --bin vibe_kanban_mcp arg_tests -- --nocapture`
Expected: PASS all three tests.

Run: `cargo test -p mcp --lib`
Expected: PASS.

- [ ] **Step 2.13: Format and commit**

```bash
pnpm run format
git add crates/mcp/src/bin/vibe_kanban_mcp.rs \
        crates/mcp/src/task_server/mod.rs \
        crates/mcp/src/task_server/tools/mod.rs
git commit -m "$(cat <<'EOF'
feat(mcp): wire Workspace router and CLI dispatch

Replaces the Task 1 shim with a real `workspace_mode_router` (Global
superset for now — new tools arrive in later commits). Adds the
`McpLaunchMode::Workspace` variant, dispatch branch, and `--mode
workspace` arg parsing. Flips the binary's own default to `workspace`
so direct invocations without `--mode` also pick the scoped path; the
npx-cli default flip lands in the final commit of this series.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Extend `list_repos` with path and display_name

Two new fields on `McpRepoSummary` so AI can correlate CWD → repo without a second `get_repo` call.

**Files:**
- Modify: `crates/mcp/src/task_server/tools/repos.rs` (struct at lines 11-17, builder at lines 91-97)
- Test: `crates/mcp/src/task_server/tools/repos.rs` (new `#[cfg(test)] mod` block)

### Steps

- [ ] **Step 3.1: Write failing test**

Append to `crates/mcp/src/task_server/tools/repos.rs`:

```rust
#[cfg(test)]
mod tests {
    use httpmock::MockServer;
    use rmcp::{handler::server::tool::ToolCallContext, model::CallToolRequestParam};

    use super::*;
    use crate::task_server::McpServer;

    fn install_rustls() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    #[tokio::test]
    async fn list_repos_returns_path_and_display_name() {
        install_rustls();
        let mock_server = MockServer::start();
        let repo_id = Uuid::new_v4();
        mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/api/repos");
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": [{
                    "id": repo_id.to_string(),
                    "name": "vibe-kanban",
                    "display_name": "Vibe Kanban",
                    "path": "/home/alice/code/vibe-kanban",
                    "setup_script": null,
                    "cleanup_script": null,
                    "dev_server_script": null,
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z"
                }]
            }));
        });

        let server = McpServer::new_global(&mock_server.base_url());
        let result = server.list_repos().await.expect("list_repos must succeed");
        let text = match &result.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        // Parse and assert.
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let first = &value["repos"][0];
        assert_eq!(first["id"], repo_id.to_string());
        assert_eq!(first["name"], "vibe-kanban");
        assert_eq!(first["display_name"], "Vibe Kanban");
        assert_eq!(first["path"], "/home/alice/code/vibe-kanban");
    }
}
```

- [ ] **Step 3.2: Run test to verify failure**

Run: `cargo test -p mcp --lib task_server::tools::repos::tests -- --nocapture`
Expected: FAIL with "display_name" / "path" fields missing from the pretty-printed JSON.

- [ ] **Step 3.3: Extend `McpRepoSummary` struct**

In `crates/mcp/src/task_server/tools/repos.rs`, replace the struct at lines 11-17:

```rust
#[derive(Debug, Serialize, schemars::JsonSchema)]
struct McpRepoSummary {
    #[schemars(description = "The unique identifier of the repository")]
    id: String,
    #[schemars(description = "The short (slug) name of the repository")]
    name: String,
    #[schemars(description = "The human-readable display name of the repository")]
    display_name: String,
    #[schemars(description = "Absolute filesystem path of the repository on this machine")]
    path: String,
}
```

- [ ] **Step 3.4: Populate new fields in `list_repos`**

In `crates/mcp/src/task_server/tools/repos.rs`, replace the builder at lines 91-97:

```rust
let repo_summaries: Vec<McpRepoSummary> = repos
    .into_iter()
    .map(|r| McpRepoSummary {
        id: r.id.to_string(),
        name: r.name,
        display_name: r.display_name,
        path: r.path.to_string_lossy().into_owned(),
    })
    .collect();
```

> Note: `Repo::path` is a `PathBuf`. Confirm by reading the struct definition in `crates/db/src/models/repo.rs` before implementing; if it's already a `String`, drop the `to_string_lossy().into_owned()` and just clone.

- [ ] **Step 3.5: Run test to verify pass**

Run: `cargo test -p mcp --lib task_server::tools::repos::tests::list_repos_returns_path_and_display_name -- --nocapture`
Expected: PASS.

Run: `cargo test -p mcp --lib`
Expected: PASS (the router-surface test from Task 2 still passes — no tool added or removed).

- [ ] **Step 3.6: Run the full Rust workspace check**

Run: `pnpm run backend:check`
Expected: PASS.

- [ ] **Step 3.7: Commit**

```bash
pnpm run format
git add crates/mcp/src/task_server/tools/repos.rs
git commit -m "$(cat <<'EOF'
feat(mcp): extend list_repos with path and display_name

Adds `display_name` and `path` to `McpRepoSummary` so an AI can
correlate its CWD with the correct repo in one call. `count` and the
wrapper response shape are unchanged.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: add_repo + delete_repo with `?force=true`

Two new tools on the MCP side and a `?force=true` query param on the existing `DELETE /api/repos/:id`.

**Files:**
- Modify: `crates/server/src/routes/repo.rs` (delete_repo signature + body at lines 343-366)
- Modify: `crates/mcp/src/task_server/api_client.rs` (add `register_repo`, `delete_repo`)
- Modify: `crates/mcp/src/task_server/tools/repos.rs` (two new `#[tool]` methods)
- Modify: `crates/mcp/src/task_server/tools/mod.rs` (no change to router composition — `repos_tools_router` auto-picks up new tools)
- Test: `crates/mcp/src/task_server/tools/repos.rs` (add tests to `mod tests`)
- Test: `crates/mcp/src/task_server/api_client.rs` (add two envelope-decoding tests)

### Steps

- [ ] **Step 4.1: Write failing backend test for `?force=true`**

In `crates/server/src/routes/repo.rs`, add at the bottom of the file:

```rust
#[cfg(test)]
mod delete_repo_query_tests {
    use super::*;

    #[derive(Deserialize)]
    pub struct ForceDeleteQuery {
        #[serde(default)]
        pub force: bool,
    }

    #[test]
    fn force_query_defaults_false_when_missing() {
        let q: ForceDeleteQuery = serde_urlencoded::from_str("").unwrap();
        assert!(!q.force);
    }

    #[test]
    fn force_query_parses_true() {
        let q: ForceDeleteQuery = serde_urlencoded::from_str("force=true").unwrap();
        assert!(q.force);
    }

    #[test]
    fn force_query_parses_false_explicit() {
        let q: ForceDeleteQuery = serde_urlencoded::from_str("force=false").unwrap();
        assert!(!q.force);
    }
}
```

- [ ] **Step 4.2: Run backend test**

Run: `cargo test -p server --lib routes::repo::delete_repo_query_tests`
Expected: PASS (pure struct parsing; verifies we have a ready shape).

- [ ] **Step 4.3: Add `force` query struct + wire into `delete_repo`**

In `crates/server/src/routes/repo.rs`, at line 337 (right above `DeleteRepoConflict`), add:

```rust
#[derive(Debug, Default, Deserialize)]
pub struct DeleteRepoQuery {
    #[serde(default)]
    pub force: bool,
}
```

Replace `delete_repo` at lines 343-366 with:

```rust
pub async fn delete_repo(
    State(deployment): State<DeploymentImpl>,
    Path(repo_id): Path<Uuid>,
    Query(query): Query<DeleteRepoQuery>,
) -> Result<
    (
        StatusCode,
        ResponseJson<ApiResponse<(), DeleteRepoConflict>>,
    ),
    ApiError,
> {
    if !query.force {
        let active = Repo::active_workspace_names(&deployment.db().pool, repo_id).await?;
        if !active.is_empty() {
            return Ok((
                StatusCode::CONFLICT,
                ResponseJson(ApiResponse::error_with_data(DeleteRepoConflict {
                    message: format!(
                        "Repository is used by {} active workspace(s). Retry with ?force=true to delete anyway.",
                        active.len()
                    ),
                    workspaces: active,
                })),
            ));
        }
    }

    Repo::delete(&deployment.db().pool, repo_id).await?;
    Ok((StatusCode::OK, ResponseJson(ApiResponse::success(()))))
}
```

- [ ] **Step 4.4: Run backend check**

Run: `cd crates/server && cargo check && cd -`
Expected: PASS.

- [ ] **Step 4.5: Write failing MCP ApiClient test for `register_repo`**

In `crates/mcp/src/task_server/api_client.rs`, add inside the existing `mod api_client_tests` (at the bottom of the file, around line 57):

```rust
#[tokio::test]
async fn register_repo_posts_payload_and_decodes_envelope() {
    let server = httpmock::MockServer::start();
    let rid = uuid::Uuid::new_v4();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/api/repos")
            .json_body(serde_json::json!({
                "path": "/tmp/x",
                "display_name": "X"
            }));
        then.status(200).json_body(serde_json::json!({
            "success": true,
            "data": {
                "id": rid.to_string(),
                "name": "x",
                "display_name": "X",
                "path": "/tmp/x",
                "setup_script": null,
                "cleanup_script": null,
                "dev_server_script": null,
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z"
            }
        }));
    });
    let client = ApiClient::new(reqwest::Client::new(), server.base_url());
    let repo = client
        .register_repo("/tmp/x", Some("X"))
        .await
        .expect("must succeed");
    assert_eq!(repo.id, rid);
    assert_eq!(repo.name, "x");
}

#[tokio::test]
async fn delete_repo_with_force_sends_query_param() {
    let server = httpmock::MockServer::start();
    let rid = uuid::Uuid::new_v4();
    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::DELETE)
            .path(format!("/api/repos/{rid}"))
            .query_param("force", "true");
        then.status(200).json_body(serde_json::json!({
            "success": true, "data": null
        }));
    });
    let client = ApiClient::new(reqwest::Client::new(), server.base_url());
    client.delete_repo(rid, true).await.expect("must succeed");
    mock.assert_hits(1);
}

#[tokio::test]
async fn delete_repo_without_force_omits_query_param() {
    let server = httpmock::MockServer::start();
    let rid = uuid::Uuid::new_v4();
    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::DELETE)
            .path(format!("/api/repos/{rid}"))
            .matches(|req| !req.query_params.as_ref()
                .map(|p| p.iter().any(|(k, _)| k == "force"))
                .unwrap_or(false));
        then.status(200).json_body(serde_json::json!({
            "success": true, "data": null
        }));
    });
    let client = ApiClient::new(reqwest::Client::new(), server.base_url());
    client.delete_repo(rid, false).await.expect("must succeed");
    mock.assert_hits(1);
}
```

- [ ] **Step 4.6: Run to verify failure**

Run: `cargo test -p mcp --lib task_server::api_client -- --nocapture`
Expected: FAIL with "no method named `register_repo`" and "no method named `delete_repo`".

- [ ] **Step 4.7: Add `ApiClient` methods**

In `crates/mcp/src/task_server/api_client.rs`, add to the `impl ApiClient` block (after `get_task`):

```rust
pub async fn register_repo(
    &self,
    path: &str,
    display_name: Option<&str>,
) -> ApiResult<Repo> {
    let url = format!("{}/api/repos", self.base_url);
    let body = serde_json::json!({
        "path": path,
        "display_name": display_name,
    });
    let resp = self.client.post(url).json(&body).send().await?;
    let envelope: ApiResponse<Repo> = resp.json().await?;
    if !envelope.is_success() {
        return Err(ApiClientError::Server(
            envelope.message().unwrap_or("").to_string(),
        ));
    }
    envelope.into_data().ok_or(ApiClientError::BadShape)
}

pub async fn delete_repo(&self, id: Uuid, force: bool) -> ApiResult<()> {
    let url = format!("{}/api/repos/{id}", self.base_url);
    let mut req = self.client.delete(url);
    if force {
        req = req.query(&[("force", "true")]);
    }
    let resp = req.send().await?;
    let envelope: ApiResponse<serde_json::Value> = resp.json().await?;
    if !envelope.is_success() {
        return Err(ApiClientError::Server(
            envelope.message().unwrap_or("").to_string(),
        ));
    }
    Ok(())
}
```

- [ ] **Step 4.8: Run tests to verify pass**

Run: `cargo test -p mcp --lib task_server::api_client -- --nocapture`
Expected: PASS.

> Note on `delete_repo` error path: the ApiClient wrapper collapses the backend's 409 conflict payload into an `ApiClientError::Server(message)`. The MCP tool layer below uses `self.client.delete(...)` directly (not through `ApiClient`) so it can surface the `DeleteRepoConflict` `error_data` to the caller. Tests above cover the happy path; error-path coverage lives in the `repos.rs` tool tests (Step 4.12).

- [ ] **Step 4.9: Write failing MCP tool tests for `add_repo` / `delete_repo`**

In `crates/mcp/src/task_server/tools/repos.rs`, extend the `mod tests` block (introduced in Task 3) with:

```rust
#[tokio::test]
async fn add_repo_happy_path() {
    install_rustls();
    let mock_server = MockServer::start();
    let repo_id = Uuid::new_v4();
    mock_server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/api/repos");
        then.status(200).json_body(serde_json::json!({
            "success": true,
            "data": {
                "id": repo_id.to_string(),
                "name": "acme",
                "display_name": "Acme",
                "path": "/home/alice/code/acme",
                "setup_script": null,
                "cleanup_script": null,
                "dev_server_script": null,
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z"
            }
        }));
    });
    let server = McpServer::new_global(&mock_server.base_url());
    let req = AddRepoRequest {
        path: "/home/alice/code/acme".to_string(),
        display_name: Some("Acme".to_string()),
    };
    let result = server
        .add_repo(rmcp::handler::server::wrapper::Parameters(req))
        .await
        .expect("must succeed");
    let text = match &result.content[0].raw {
        rmcp::model::RawContent::Text(t) => t.text.clone(),
        _ => panic!("expected text"),
    };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["id"], repo_id.to_string());
    assert_eq!(v["path"], "/home/alice/code/acme");
}

#[tokio::test]
async fn add_repo_surfaces_invalid_repo_error_kind() {
    install_rustls();
    let mock_server = MockServer::start();
    mock_server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/api/repos");
        then.status(400).json_body(serde_json::json!({
            "success": false,
            "message": "not a git repository",
            "error_kind": "invalid_repo"
        }));
    });
    let server = McpServer::new_global(&mock_server.base_url());
    let req = AddRepoRequest {
        path: "/tmp/not-a-repo".to_string(),
        display_name: None,
    };
    let result = server
        .add_repo(rmcp::handler::server::wrapper::Parameters(req))
        .await
        .expect("tool wraps error as CallToolResult");
    assert!(result.is_error.unwrap_or(false));
    let text = match &result.content[0].raw {
        rmcp::model::RawContent::Text(t) => t.text.clone(),
        _ => panic!("expected text"),
    };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["error_kind"], "invalid_repo");
}

#[tokio::test]
async fn delete_repo_rejects_when_active_workspaces_without_force() {
    install_rustls();
    let mock_server = MockServer::start();
    let rid = Uuid::new_v4();
    mock_server.mock(|when, then| {
        when.method(httpmock::Method::DELETE)
            .path(format!("/api/repos/{rid}"))
            .matches(|r| !r.query_params.as_ref()
                .map(|p| p.iter().any(|(k, _)| k == "force"))
                .unwrap_or(false));
        then.status(409).json_body(serde_json::json!({
            "success": false,
            "message": "Repository is used by 2 active workspace(s). Retry with ?force=true to delete anyway.",
            "error_data": {
                "message": "Repository is used by 2 active workspace(s).",
                "workspaces": ["feature/login", "hotfix/cache"]
            }
        }));
    });
    let server = McpServer::new_global(&mock_server.base_url());
    let req = DeleteRepoRequest { repo_id: rid, force: None };
    let result = server
        .delete_repo(rmcp::handler::server::wrapper::Parameters(req))
        .await
        .expect("tool wraps error");
    assert!(result.is_error.unwrap_or(false));
    let text = match &result.content[0].raw {
        rmcp::model::RawContent::Text(t) => t.text.clone(),
        _ => panic!("expected text"),
    };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let workspaces = v["error_data"]["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 2);
    assert_eq!(workspaces[0], "feature/login");
}

#[tokio::test]
async fn delete_repo_with_force_cascades() {
    install_rustls();
    let mock_server = MockServer::start();
    let rid = Uuid::new_v4();
    let mock = mock_server.mock(|when, then| {
        when.method(httpmock::Method::DELETE)
            .path(format!("/api/repos/{rid}"))
            .query_param("force", "true");
        then.status(200).json_body(serde_json::json!({
            "success": true, "data": null
        }));
    });
    let server = McpServer::new_global(&mock_server.base_url());
    let req = DeleteRepoRequest { repo_id: rid, force: Some(true) };
    let result = server
        .delete_repo(rmcp::handler::server::wrapper::Parameters(req))
        .await
        .expect("must succeed");
    assert!(!result.is_error.unwrap_or(false));
    mock.assert_hits(1);
}
```

- [ ] **Step 4.10: Run tests to verify failure**

Run: `cargo test -p mcp --lib task_server::tools::repos::tests -- --nocapture`
Expected: FAIL with "AddRepoRequest not found" and "DeleteRepoRequest not found".

- [ ] **Step 4.11: Add `add_repo` tool**

In `crates/mcp/src/task_server/tools/repos.rs`, add after the existing struct declarations (before the `#[tool_router(...)]` block):

```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct AddRepoRequest {
    #[schemars(
        description = "Absolute filesystem path to an existing git repository on this machine."
    )]
    pub path: String,
    #[schemars(
        description = "Optional human-readable display name. Defaults to the folder name."
    )]
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct AddRepoResponse {
    id: String,
    name: String,
    display_name: String,
    path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct DeleteRepoRequest {
    #[schemars(description = "ID of the repository to delete.")]
    pub repo_id: Uuid,
    #[schemars(
        description = "If true, delete even when active workspaces reference this repo. Default: false."
    )]
    pub force: Option<bool>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DeleteRepoResponse {
    success: bool,
    repo_id: String,
}
```

Then, inside the `#[tool_router(...)] impl McpServer { ... }` block, add two methods after `update_dev_server_script`:

```rust
#[tool(
    description = "Register an existing git repository by absolute path. Returns the new repo record. Fails with error_kind='invalid_repo' if path is not a git repo."
)]
async fn add_repo(
    &self,
    Parameters(AddRepoRequest { path, display_name }): Parameters<AddRepoRequest>,
) -> Result<CallToolResult, ErrorData> {
    let url = self.url("/api/repos");
    let payload = serde_json::json!({
        "path": path,
        "display_name": display_name,
    });
    let repo: Repo = match self.send_json(self.client.post(&url).json(&payload)).await {
        Ok(r) => r,
        Err(e) => return Ok(Self::tool_error(e)),
    };
    McpServer::success(&AddRepoResponse {
        id: repo.id.to_string(),
        name: repo.name,
        display_name: repo.display_name,
        path: repo.path.to_string_lossy().into_owned(),
    })
}

#[tool(
    description = "Delete a repository. Rejects with error_kind/error_data surfacing active workspaces if any reference the repo; pass force=true to delete anyway."
)]
async fn delete_repo(
    &self,
    Parameters(DeleteRepoRequest { repo_id, force }): Parameters<DeleteRepoRequest>,
) -> Result<CallToolResult, ErrorData> {
    let url = self.url(&format!("/api/repos/{}", repo_id));
    let mut req = self.client.delete(&url);
    if force.unwrap_or(false) {
        req = req.query(&[("force", "true")]);
    }
    if let Err(e) = self.send_empty_json(req).await {
        return Ok(Self::tool_error(e));
    }
    McpServer::success(&DeleteRepoResponse {
        success: true,
        repo_id: repo_id.to_string(),
    })
}
```

- [ ] **Step 4.12: Run tool tests**

Run: `cargo test -p mcp --lib task_server::tools::repos::tests -- --nocapture`
Expected: PASS all five tests (one from Task 3 + four new).

- [ ] **Step 4.13: Verify router auto-registers new tools**

In `crates/mcp/src/task_server/tools/mod.rs`, extend the existing `global_mode_keeps_workspace_admin_and_discovery_tools` test (around line 615) — or add a new `global_mode_registers_new_repo_tools` test:

```rust
#[test]
fn global_mode_registers_new_repo_tools() {
    let names = tool_names(McpServer::global_mode_router());
    assert!(names.contains("add_repo"));
    assert!(names.contains("delete_repo"));
}
```

Run: `cargo test -p mcp --lib tests::global_mode_registers_new_repo_tools`
Expected: PASS.

- [ ] **Step 4.14: Format, check, commit**

```bash
pnpm run format
pnpm run backend:check
git add crates/server/src/routes/repo.rs \
        crates/mcp/src/task_server/api_client.rs \
        crates/mcp/src/task_server/tools/repos.rs \
        crates/mcp/src/task_server/tools/mod.rs
git commit -m "$(cat <<'EOF'
feat(mcp): add add_repo and delete_repo tools with force handshake

Registers a new repo by absolute path; non-git paths surface via the
backend's `error_kind=invalid_repo` envelope. `delete_repo` relies on
the existing 409-with-active-workspaces backend behaviour (now extended
with a `?force=true` query param) so the AI sees which workspaces
block the delete and can decide to abort or retry with force.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: remote_tags module + create/update/delete_tag with `?force=true`

Splits a new `remote_tags.rs` module out of `issue_tags.rs`, moves `list_tags` into it, adds three new mutation tools, and adds a `?force=true` guard to backend `DELETE /v1/tags/:id`.

**Files:**
- Create: `crates/mcp/src/task_server/tools/remote_tags.rs`
- Modify: `crates/mcp/src/task_server/tools/issue_tags.rs` (remove `list_tags` + its helper types)
- Modify: `crates/mcp/src/task_server/tools/mod.rs` (declare `remote_tags` module, add to `global_mode_router`)
- Modify: `crates/mcp/src/task_server/api_client.rs` (add `create_tag`, `update_tag`, `delete_tag`)
- Modify: `crates/server/src/routes/v1.rs` (409 guard + `?force=true` on `delete_tag`)
- Modify: `crates/api-types/src/tag.rs` or a new `DeleteTagConflict` type location + export
- Test: `crates/mcp/src/task_server/tools/remote_tags.rs` (new `mod tests`)
- Generated: `shared/types.ts` via `pnpm run generate-types`

### Steps

- [ ] **Step 5.1: Write failing backend test for `?force=true` delete_tag**

In `crates/server/src/routes/v1.rs`, near the top (after imports, before `router()` at line 61), confirm there's a `#[cfg(test)] mod tests` block — if not, add at the bottom of the file:

```rust
#[cfg(test)]
mod delete_tag_query_tests {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Q {
        #[serde(default)]
        force: bool,
    }

    #[test]
    fn force_defaults_false() {
        let q: Q = serde_urlencoded::from_str("").unwrap();
        assert!(!q.force);
    }

    #[test]
    fn force_true_parses() {
        let q: Q = serde_urlencoded::from_str("force=true").unwrap();
        assert!(q.force);
    }
}
```

Run: `cargo test -p server --lib routes::v1::delete_tag_query_tests`
Expected: PASS.

- [ ] **Step 5.2: Add `DeleteTagConflict` type to `api-types`**

In `crates/api-types/src/tag.rs`, at the bottom of the file:

```rust
#[derive(Debug, Serialize, TS)]
pub struct DeleteTagConflict {
    pub message: String,
    pub issue_tag_count: i64,
}
```

Ensure `use` statements at the top include `Serialize` and `TS` (add if missing):

```rust
use serde::{Deserialize, Serialize};
use ts_rs::TS;
```

- [ ] **Step 5.3: Re-export `DeleteTagConflict`**

In `crates/api-types/src/lib.rs`, find the `pub use tag::*;` line (or `pub use tag::{...};`) and include `DeleteTagConflict`. Add an explicit re-export if it uses named list style:

```rust
pub use tag::{CreateTagRequest, DeleteTagConflict, Tag, UpdateTagRequest};
```

> If the existing re-export is `pub use tag::*;`, no change is needed.

- [ ] **Step 5.4: Write failing MCP test for tag conflict surfacing**

In `crates/mcp/src/task_server/tools/remote_tags.rs` (create the file), add:

```rust
use api_types::{CreateTagRequest, MutationResponse, Tag, UpdateTagRequest};
use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{McpServer, ToolError};

/// MCP-side default when the caller omits `color`. A neutral gray that
/// reads as "unstyled" against any theme; callers who care pass an
/// explicit value.
const DEFAULT_TAG_COLOR: &str = "#6B7280";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpListTagsRequest {
    #[schemars(
        description = "The project ID to list tags from. Optional if running inside a workspace linked to a remote project."
    )]
    project_id: Option<Uuid>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TagSummary {
    #[schemars(description = "Tag ID")]
    id: String,
    #[schemars(description = "Project ID")]
    project_id: String,
    #[schemars(description = "Tag name")]
    name: String,
    #[schemars(description = "Tag color value")]
    color: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct McpListTagsResponse {
    project_id: String,
    tags: Vec<TagSummary>,
    count: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct CreateTagArgs {
    #[schemars(description = "Project to create the tag in. Optional when workspace context provides it.")]
    pub project_id: Option<Uuid>,
    #[schemars(description = "Tag name (required, unique per project).")]
    pub name: String,
    #[schemars(
        description = "Optional hex color like #6B7280. Defaults to neutral gray if omitted."
    )]
    pub color: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct UpdateTagArgs {
    #[schemars(description = "Tag to update.")]
    pub tag_id: Uuid,
    #[schemars(description = "New name (optional).")]
    pub name: Option<String>,
    #[schemars(description = "New hex color (optional).")]
    pub color: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct DeleteTagArgs {
    #[schemars(description = "Tag to delete.")]
    pub tag_id: Uuid,
    #[schemars(
        description = "If true, delete even when issue_tags reference the tag. Default: false."
    )]
    pub force: Option<bool>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DeleteTagResponse {
    success: bool,
    tag_id: String,
}

#[tool_router(router = remote_tags_tools_router, vis = "pub")]
impl McpServer {
    #[tool(
        description = "List tags for a project. `project_id` is optional if running inside a workspace linked to a remote project."
    )]
    async fn list_tags(
        &self,
        Parameters(McpListTagsRequest { project_id }): Parameters<McpListTagsRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let project_id = match self.resolve_project_id(project_id) {
            Ok(id) => id,
            Err(e) => return Ok(Self::tool_error(e)),
        };

        let url = self.url(&format!("/api/remote/tags?project_id={}", project_id));
        let response: api_types::ListTagsResponse =
            match self.send_json(self.client.get(&url)).await {
                Ok(r) => r,
                Err(e) => return Ok(Self::tool_error(e)),
            };

        let tags = response
            .tags
            .into_iter()
            .map(|tag| TagSummary {
                id: tag.id.to_string(),
                project_id: tag.project_id.to_string(),
                name: tag.name,
                color: tag.color,
            })
            .collect::<Vec<_>>();

        McpServer::success(&McpListTagsResponse {
            project_id: project_id.to_string(),
            count: tags.len(),
            tags,
        })
    }

    #[tool(
        description = "Create a project-scoped tag. `project_id` falls back to workspace context. `color` defaults to #6B7280 if omitted. Fails if the caller's workspace context targets a different project."
    )]
    async fn create_tag(
        &self,
        Parameters(CreateTagArgs { project_id, name, color }): Parameters<CreateTagArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project_id = match self.resolve_project_id(project_id) {
            Ok(id) => id,
            Err(e) => return Ok(Self::tool_error(e)),
        };

        // Cross-project guard when workspace context is present.
        if let Some(ctx) = &self.context
            && let Some(scoped_project) = ctx.project_id
            && scoped_project != project_id
        {
            return Ok(Self::tool_error(ToolError::new(
                "Cannot create tag outside the current workspace's project",
                Some(format!(
                    "requested project_id={}, scoped project_id={}",
                    project_id, scoped_project
                )),
            )));
        }

        let payload = CreateTagRequest {
            id: None,
            project_id,
            name,
            color: color.unwrap_or_else(|| DEFAULT_TAG_COLOR.to_string()),
        };

        let url = self.url("/api/v1/tags");
        let response: MutationResponse<Tag> =
            match self.send_json(self.client.post(&url).json(&payload)).await {
                Ok(r) => r,
                Err(e) => return Ok(Self::tool_error(e)),
            };

        let tag = response.data;
        McpServer::success(&TagSummary {
            id: tag.id.to_string(),
            project_id: tag.project_id.to_string(),
            name: tag.name,
            color: tag.color,
        })
    }

    #[tool(
        description = "Update a tag's name and/or color. At least one field must be provided."
    )]
    async fn update_tag(
        &self,
        Parameters(UpdateTagArgs { tag_id, name, color }): Parameters<UpdateTagArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if name.is_none() && color.is_none() {
            return Ok(Self::tool_error(ToolError::message(
                "No fields to update (provide at least one of `name`, `color`)",
            )));
        }
        let payload = UpdateTagRequest { name, color };
        let url = self.url(&format!("/api/v1/tags/{}", tag_id));
        let response: MutationResponse<Tag> =
            match self.send_json(self.client.patch(&url).json(&payload)).await {
                Ok(r) => r,
                Err(e) => return Ok(Self::tool_error(e)),
            };
        let tag = response.data;
        McpServer::success(&TagSummary {
            id: tag.id.to_string(),
            project_id: tag.project_id.to_string(),
            name: tag.name,
            color: tag.color,
        })
    }

    #[tool(
        description = "Delete a tag. Rejects with error_data.issue_tag_count when referenced by issue_tags; pass force=true to cascade-delete the relation rows."
    )]
    async fn delete_tag(
        &self,
        Parameters(DeleteTagArgs { tag_id, force }): Parameters<DeleteTagArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/v1/tags/{}", tag_id));
        let mut req = self.client.delete(&url);
        if force.unwrap_or(false) {
            req = req.query(&[("force", "true")]);
        }
        if let Err(e) = self.send_empty_json(req).await {
            return Ok(Self::tool_error(e));
        }
        McpServer::success(&DeleteTagResponse {
            success: true,
            tag_id: tag_id.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::MockServer;

    fn install_rustls() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    fn tag_envelope(id: Uuid, project_id: Uuid, name: &str, color: &str) -> serde_json::Value {
        serde_json::json!({
            "data": {
                "id": id.to_string(),
                "project_id": project_id.to_string(),
                "name": name,
                "color": color,
                "created_at": "2025-01-01T00:00:00Z"
            },
            "txid": 0
        })
    }

    #[tokio::test]
    async fn create_tag_defaults_color_when_omitted() {
        install_rustls();
        let mock_server = MockServer::start();
        let pid = Uuid::new_v4();
        let tid = Uuid::new_v4();
        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/v1/tags")
                .json_body_partial(
                    serde_json::json!({ "color": DEFAULT_TAG_COLOR }).to_string(),
                );
            then.status(200)
                .json_body(tag_envelope(tid, pid, "bug", DEFAULT_TAG_COLOR));
        });
        let server = McpServer::new_global(&mock_server.base_url());
        let req = CreateTagArgs {
            project_id: Some(pid),
            name: "bug".to_string(),
            color: None,
        };
        let result = server
            .create_tag(Parameters(req))
            .await
            .expect("must succeed");
        assert!(!result.is_error.unwrap_or(false));
        mock.assert_hits(1);
    }

    #[tokio::test]
    async fn create_tag_respects_caller_color() {
        install_rustls();
        let mock_server = MockServer::start();
        let pid = Uuid::new_v4();
        let tid = Uuid::new_v4();
        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/v1/tags")
                .json_body_partial(
                    serde_json::json!({ "color": "#FF0000" }).to_string(),
                );
            then.status(200)
                .json_body(tag_envelope(tid, pid, "critical", "#FF0000"));
        });
        let server = McpServer::new_global(&mock_server.base_url());
        let req = CreateTagArgs {
            project_id: Some(pid),
            name: "critical".to_string(),
            color: Some("#FF0000".to_string()),
        };
        server
            .create_tag(Parameters(req))
            .await
            .expect("must succeed");
        mock.assert_hits(1);
    }

    #[tokio::test]
    async fn create_tag_rejects_cross_project_when_scoped() {
        install_rustls();
        let mock_server = MockServer::start();
        // Backend must NOT be called — scope gate short-circuits.
        let backend = mock_server.mock(|when, then| {
            when.any_request();
            then.status(500);
        });
        let scoped_project = Uuid::new_v4();
        let requested_project = Uuid::new_v4();

        let mut server = McpServer::new_workspace(&mock_server.base_url())
            .with_scope_for_test(Uuid::new_v4());
        // Inject a project_id into context so the guard can fire.
        if let Some(ctx) = server.context.as_mut() {
            ctx.project_id = Some(scoped_project);
        }

        let req = CreateTagArgs {
            project_id: Some(requested_project),
            name: "x".to_string(),
            color: None,
        };
        let result = server
            .create_tag(Parameters(req))
            .await
            .expect("must return error tool result");
        assert!(result.is_error.unwrap_or(false));
        assert_eq!(backend.hits(), 0);
    }

    #[tokio::test]
    async fn update_tag_requires_at_least_one_field() {
        install_rustls();
        let mock_server = MockServer::start();
        let backend = mock_server.mock(|when, then| {
            when.any_request();
            then.status(500);
        });
        let server = McpServer::new_global(&mock_server.base_url());
        let req = UpdateTagArgs {
            tag_id: Uuid::new_v4(),
            name: None,
            color: None,
        };
        let result = server
            .update_tag(Parameters(req))
            .await
            .expect("must return error tool result");
        assert!(result.is_error.unwrap_or(false));
        assert_eq!(backend.hits(), 0);
    }

    #[tokio::test]
    async fn delete_tag_rejects_in_use_without_force() {
        install_rustls();
        let mock_server = MockServer::start();
        let tid = Uuid::new_v4();
        mock_server.mock(|when, then| {
            when.method(httpmock::Method::DELETE)
                .path(format!("/api/v1/tags/{tid}"))
                .matches(|r| !r.query_params.as_ref()
                    .map(|p| p.iter().any(|(k, _)| k == "force"))
                    .unwrap_or(false));
            then.status(409).json_body(serde_json::json!({
                "success": false,
                "message": "Tag is referenced by 3 issue_tags. Retry with ?force=true.",
                "error_kind": "tag_in_use",
                "error_data": {
                    "message": "Tag is referenced by 3 issue_tags",
                    "issue_tag_count": 3
                }
            }));
        });
        let server = McpServer::new_global(&mock_server.base_url());
        let result = server
            .delete_tag(Parameters(DeleteTagArgs { tag_id: tid, force: None }))
            .await
            .expect("must return error tool result");
        assert!(result.is_error.unwrap_or(false));
        let text = match &result.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text"),
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error_kind"], "tag_in_use");
        assert_eq!(v["error_data"]["issue_tag_count"], 3);
    }

    #[tokio::test]
    async fn delete_tag_with_force_cascades() {
        install_rustls();
        let mock_server = MockServer::start();
        let tid = Uuid::new_v4();
        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::DELETE)
                .path(format!("/api/v1/tags/{tid}"))
                .query_param("force", "true");
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": { "txid": 0 }
            }));
        });
        let server = McpServer::new_global(&mock_server.base_url());
        server
            .delete_tag(Parameters(DeleteTagArgs {
                tag_id: tid,
                force: Some(true),
            }))
            .await
            .expect("must succeed");
        mock.assert_hits(1);
    }
}
```

- [ ] **Step 5.5: Register the new module in `tools/mod.rs`**

In `crates/mcp/src/task_server/tools/mod.rs`, add `mod remote_tags;` to the existing module-declaration block (around line 249, after `mod issue_tags;`).

Then update `global_mode_router` (at line 262) to include the new router:

```rust
pub fn global_mode_router() -> rmcp::handler::server::tool::ToolRouter<Self> {
    Self::context_tools_router()
        + Self::workspaces_tools_router()
        + Self::organizations_tools_router()
        + Self::repos_tools_router()
        + Self::remote_projects_tools_router()
        + Self::remote_issues_tools_router()
        + Self::issue_assignees_tools_router()
        + Self::issue_tags_tools_router()
        + Self::remote_tags_tools_router()
        + Self::issue_relationships_tools_router()
        + Self::task_attempts_tools_router()
        + Self::session_tools_router()
}
```

- [ ] **Step 5.6: Expose `ToolError` to the new module**

The new `remote_tags.rs` references `super::ToolError` (for `ToolError::new`/`ToolError::message`). The existing declaration at `tools/mod.rs:28` is `struct ToolError` (no `pub`). Make the two constructors used by siblings reachable. Pick the cleanest of:

- **Option A (preferred):** change `struct ToolError` to `pub(super) struct ToolError` at line 28, and mark its `new` / `message` methods `pub(super)` (already `fn`, add `pub(super)`). Other sibling tool modules already use `ToolError` via similar access — verify by running a grep for `use super::ToolError` in sibling modules like `sessions.rs`.

- **Option B:** keep `ToolError` private and export a helper like `pub(super) fn tool_error_msg(msg: &str) -> ToolError`. More friction, not recommended.

Confirm which path existing modules use (e.g. `sessions.rs`, `tasks.rs`). Mirror that.

> Plan execution note: at time of writing, sibling modules route errors via `Self::tool_error(ToolError::new(...))` inline, suggesting `ToolError` is already reachable. If `cargo check` complains about private visibility, apply Option A.

- [ ] **Step 5.7: Remove `list_tags` from `issue_tags.rs`**

In `crates/mcp/src/task_server/tools/issue_tags.rs`:

1. Delete the `McpListTagsRequest`, `TagSummary`, `McpListTagsResponse` structs (lines 13-38).
2. Delete the `list_tags` tool method (lines 90-124).
3. Remove the `ListTagsResponse` import from the `use api_types::{...}` line at line 1-3.

The resulting file keeps only `list_issue_tags`, `add_issue_tag`, `remove_issue_tag` and their shared types.

- [ ] **Step 5.8: Run MCP-side unit tests**

Run: `cargo test -p mcp --lib task_server::tools::remote_tags::tests -- --nocapture`
Expected: FAIL — `create_tag` / `update_tag` / `delete_tag` MCP tools compile, but their corresponding `ApiClient` methods don't exist yet (caller uses `self.client.post/patch/delete` directly, so the tool tests should pass) — however the `api-types::MutationResponse<Tag>` shape might not quite match the raw backend response (see `mutation_response` helper at `v1.rs`). Add a `mutation_response` alignment test:

Re-run: test failures, if any, indicate envelope-shape drift. Fix by matching the exact JSON the backend returns (use `data` + `txid`, which matches `MutationResponse<T>` definition in `api_types`).

Run again: `cargo test -p mcp --lib task_server::tools::remote_tags::tests`
Expected: PASS all six tests.

- [ ] **Step 5.9: Add `ApiClient` methods for tags**

In `crates/mcp/src/task_server/api_client.rs`, add:

```rust
pub async fn create_tag(
    &self,
    project_id: Uuid,
    name: &str,
    color: &str,
) -> ApiResult<Tag> {
    let url = format!("{}/api/v1/tags", self.base_url);
    let body = serde_json::json!({
        "project_id": project_id,
        "name": name,
        "color": color,
    });
    let resp = self.client.post(url).json(&body).send().await?;
    let envelope: api_types::MutationResponse<Tag> = resp.json().await?;
    Ok(envelope.data)
}

pub async fn update_tag(
    &self,
    tag_id: Uuid,
    name: Option<&str>,
    color: Option<&str>,
) -> ApiResult<Tag> {
    let url = format!("{}/api/v1/tags/{tag_id}", self.base_url);
    let body = serde_json::json!({
        "name": name,
        "color": color,
    });
    let resp = self.client.patch(url).json(&body).send().await?;
    let envelope: api_types::MutationResponse<Tag> = resp.json().await?;
    Ok(envelope.data)
}

pub async fn delete_tag(&self, tag_id: Uuid, force: bool) -> ApiResult<()> {
    let url = format!("{}/api/v1/tags/{tag_id}", self.base_url);
    let mut req = self.client.delete(url);
    if force {
        req = req.query(&[("force", "true")]);
    }
    let _resp = req.send().await?.error_for_status()?;
    Ok(())
}
```

Import `Tag` at the top of the file:

```rust
use db::models::{task::Task, workspace::Workspace};
use api_types::Tag;
```

> Note: the tool layer (`remote_tags.rs`) does not call these `ApiClient` wrappers in the initial landing — it uses `self.client.post/patch/delete` inline for consistency with the existing tool code. The `ApiClient` methods exist for future callers (e.g. backend admin endpoints that want a local-first mutation path). Tests are minimal — see Step 5.10.

- [ ] **Step 5.10: Write minimal ApiClient tests for new tag methods**

Append to `mod api_client_tests` in `crates/mcp/src/task_server/api_client.rs`:

```rust
#[tokio::test]
async fn create_tag_decodes_envelope() {
    let server = httpmock::MockServer::start();
    let tid = uuid::Uuid::new_v4();
    let pid = uuid::Uuid::new_v4();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/api/v1/tags");
        then.status(200).json_body(serde_json::json!({
            "data": {
                "id": tid.to_string(),
                "project_id": pid.to_string(),
                "name": "bug",
                "color": "#6B7280",
                "created_at": "2025-01-01T00:00:00Z"
            },
            "txid": 0
        }));
    });
    let client = ApiClient::new(reqwest::Client::new(), server.base_url());
    let tag = client
        .create_tag(pid, "bug", "#6B7280")
        .await
        .expect("must succeed");
    assert_eq!(tag.id, tid);
}

#[tokio::test]
async fn delete_tag_with_force_sets_query() {
    let server = httpmock::MockServer::start();
    let tid = uuid::Uuid::new_v4();
    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::DELETE)
            .path(format!("/api/v1/tags/{tid}"))
            .query_param("force", "true");
        then.status(200).body("");
    });
    let client = ApiClient::new(reqwest::Client::new(), server.base_url());
    client.delete_tag(tid, true).await.expect("must succeed");
    mock.assert_hits(1);
}
```

- [ ] **Step 5.11: Run ApiClient tests**

Run: `cargo test -p mcp --lib task_server::api_client -- --nocapture`
Expected: PASS.

- [ ] **Step 5.12: Update backend `delete_tag` to honor `?force=true`**

In `crates/server/src/routes/v1.rs`, replace `delete_tag` (lines 782-793):

```rust
#[derive(Debug, Default, Deserialize)]
struct DeleteTagQuery {
    #[serde(default)]
    force: bool,
}

async fn delete_tag(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
    Query(query): Query<DeleteTagQuery>,
) -> Result<
    (
        axum::http::StatusCode,
        ResponseJson<
            utils::response::ApiResponse<DeleteResponse, api_types::DeleteTagConflict>,
        >,
    ),
    ApiError,
> {
    let pool = deployment.db().pool.clone();

    if !query.force {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM remote_issue_tags WHERE tag_id = ?1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .map_err(ApiError::Database)?;
        if count > 0 {
            return Ok((
                axum::http::StatusCode::CONFLICT,
                ResponseJson(utils::response::ApiResponse::error_with_data(
                    api_types::DeleteTagConflict {
                        message: format!(
                            "Tag is referenced by {count} issue_tag(s). Retry with ?force=true to cascade-delete."
                        ),
                        issue_tag_count: count,
                    },
                )),
            ));
        }
    }

    sqlx::query("DELETE FROM remote_tags WHERE id = ?1")
        .bind(id)
        .execute(&pool)
        .await
        .map_err(ApiError::Database)?;
    Ok((
        axum::http::StatusCode::OK,
        ResponseJson(utils::response::ApiResponse::success(DeleteResponse {
            txid: 0,
        })),
    ))
}
```

> The `Query` import is already active in this file. Ensure `axum::http::StatusCode` is in scope — the file imports `axum::{...}` at line 30 but does not pull in `http::StatusCode`. Either add `StatusCode` to the existing `use axum::{...}` block or use the fully qualified path as above.

> If the existing `delete_tag` returns `ResponseJson<DeleteResponse>` directly (no `ApiResponse` wrapping), other mutation routes in this file are consistent — we're introducing a non-uniform return type here. Verify by checking the route registration:
>
> ```rust
> .route("/tags/{id}", patch(update_tag).delete(delete_tag))
> ```
>
> Axum accepts the new return type as long as it implements `IntoResponse`. If type-check fails, wrap both success and conflict arms in `Result<ResponseJson<Value>, ApiError>` using `serde_json::json!` instead — both code paths remain observable by the MCP tool via `send_empty_json`'s envelope parser.

- [ ] **Step 5.13: Run backend tests**

Run: `pnpm run backend:check`
Expected: PASS.

Run: `cargo test -p server --lib`
Expected: PASS.

- [ ] **Step 5.14: Regenerate TS types**

Run: `pnpm run generate-types`
Expected: `shared/types.ts` updated with `DeleteTagConflict` (and no other diffs).

Verify by running `git status` — only `shared/types.ts` should be modified among generated files. If additional types show up, ensure the diff is intentional and commit them together.

- [ ] **Step 5.15: Verify router registers tags module**

Add to the `mod tests` block in `crates/mcp/src/task_server/tools/mod.rs`:

```rust
#[test]
fn global_mode_registers_remote_tag_tools() {
    let names = tool_names(McpServer::global_mode_router());
    for expected in ["list_tags", "create_tag", "update_tag", "delete_tag"] {
        assert!(
            names.contains(expected),
            "missing {expected} in global router"
        );
    }
}
```

Run: `cargo test -p mcp --lib tests::global_mode_registers_remote_tag_tools`
Expected: PASS.

- [ ] **Step 5.16: Format, check, commit**

```bash
pnpm run format
pnpm run backend:check
pnpm run check
git add crates/mcp/src/task_server/tools/remote_tags.rs \
        crates/mcp/src/task_server/tools/issue_tags.rs \
        crates/mcp/src/task_server/tools/mod.rs \
        crates/mcp/src/task_server/api_client.rs \
        crates/server/src/routes/v1.rs \
        crates/api-types/src/tag.rs \
        crates/api-types/src/lib.rs \
        shared/types.ts
git commit -m "$(cat <<'EOF'
feat(mcp): split remote_tags module; add create/update/delete_tag

Extracts `list_tags` from `issue_tags.rs` into a new `remote_tags.rs`
module alongside three new mutation tools (create/update/delete).
Adds a `?force=true` query param + 409 guard to backend
`DELETE /v1/tags/:id` so delete_tag surfaces issue_tag reference counts
to the AI before cascading. `create_tag` defaults `color=#6B7280` when
omitted and rejects cross-project calls when workspace context pins a
different project.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Flip default mode + documentation

The user-visible atomic switch: npx-cli default, `default_mcp.json`, three docs files, and nav.

**Files:**
- Modify: `crates/executors/default_mcp.json`
- Modify: `npx-cli/src/cli.ts` (line 122-124)
- Modify: `docs/integrations/vibe-kanban-mcp-server.mdx`
- Modify: `docs/integrations/mcp-server-configuration.mdx`
- Create: `docs/integrations/mcp-modes.mdx`
- Modify: `docs/docs.json`

### Steps

- [ ] **Step 6.1: Flip `default_mcp.json`**

In `crates/executors/default_mcp.json`, update the vibe_kanban entry:

```json
{
  "vibe_kanban": {
    "command": "npx",
    "args": ["-y", "vibe-kanban@latest", "--mcp", "--mode", "workspace"]
  }
}
```

- [ ] **Step 6.2: Flip npx-cli default**

In `npx-cli/src/cli.ts` at line 122-124, replace:

```ts
function buildMcpArgs(args: string[]): string[] {
  return args.length > 0 ? args : ["--mode", "workspace"];
}
```

- [ ] **Step 6.3: Write minimal unit test for `buildMcpArgs`**

If a test file exists next to `cli.ts` (e.g. `npx-cli/src/cli.test.ts`), add. Otherwise create `npx-cli/src/cli.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { buildMcpArgs } from "./cli";

describe("buildMcpArgs", () => {
  it("defaults to workspace when empty", () => {
    expect(buildMcpArgs([])).toEqual(["--mode", "workspace"]);
  });

  it("returns caller args when non-empty", () => {
    expect(buildMcpArgs(["--mode", "global"])).toEqual(["--mode", "global"]);
  });
});
```

If `buildMcpArgs` is not already exported, export it:

```ts
export function buildMcpArgs(args: string[]): string[] {
  return args.length > 0 ? args : ["--mode", "workspace"];
}
```

- [ ] **Step 6.4: Run JS tests**

Run: `pnpm --filter vibe-kanban exec vitest run src/cli.test.ts`
Expected: PASS.

> If vitest isn't configured in `npx-cli`, skip Step 6.3-6.4 and rely on the Rust-side `default_mode_is_workspace` binary test from Task 2 as the defaulting guarantee.

- [ ] **Step 6.5: Edit `docs/integrations/vibe-kanban-mcp-server.mdx`**

Near the top of the page (right after the frontmatter and intro paragraph), insert a new section:

```markdown
## Modes at a glance

<CardGroup cols={3}>
  <Card title="Workspace (default)" icon="folder-open">
  Auto-scopes to the VK worktree the agent was launched from. Full tool
  surface. Gracefully falls back to Global-equivalent when the CWD
  doesn't match a VK worktree.
  </Card>
  <Card title="Global" icon="globe">
  No CWD auto-scope. Every tool is available. Use for admin scripts or
  CI where a single agent works across arbitrary workspaces.
  </Card>
  <Card title="Orchestrator" icon="robot">
  VK-internal only. A lean 14-tool surface pinned to one parent task.
  Not user-facing.
  </Card>
</CardGroup>

Starting with this release, fresh installs default to Workspace mode.
See [MCP Modes Reference](/integrations/mcp-modes) for the full
comparison, including which tools each mode exposes.
```

Then, near the end of the page (before "See also" or similar existing closing section), add:

```markdown
## Upgrading from older configs

Existing `~/.claude.json` (or equivalent) entries fall into two shapes:

1. **Implicit args** — `["-y", "vibe-kanban@latest", "--mcp"]`.
   After this release, the next launch automatically picks up
   Workspace mode. No file edit required.
2. **Explicit `--mode global`** — your choice is respected and you
   stay on Global. To switch, replace `global` with `workspace`:

   ```json
   "args": ["-y", "vibe-kanban@latest", "--mcp", "--mode", "workspace"]
   ```

Workspace mode is a strict superset of Global. Inside a VK worktree,
mutations on workspaces outside your current scope are rejected (a
safety feature — the error message points back to `--mode global` if
you genuinely need cross-workspace admin).
```

- [ ] **Step 6.6: Edit `docs/integrations/mcp-server-configuration.mdx`**

Near the top (right after frontmatter), add:

```markdown
<Info>
**Which mode should I use?** New installs default to `workspace`, which
pins the MCP server to the VK worktree your agent was launched from.
For cross-workspace admin scripts or CI, use `global`. See [MCP Modes
Reference](/integrations/mcp-modes).
</Info>
```

For every per-agent example currently showing `"--mcp"` as the last
arg, update to include `"--mode", "workspace"`. List of agents that
need updating (search in page for each):

- Claude
- Cursor
- Codex
- Gemini
- Amp
- Opencode
- Copilot

Example of the pattern before/after:

```diff
- "args": ["-y", "vibe-kanban@latest", "--mcp"]
+ "args": ["-y", "vibe-kanban@latest", "--mcp", "--mode", "workspace"]
```

- [ ] **Step 6.7: Create `docs/integrations/mcp-modes.mdx`**

```markdown
---
title: "MCP Modes"
description: "How vibe-kanban's MCP server chooses a tool surface and scope based on launch mode."
---

Vibe Kanban's MCP server exposes different tool surfaces and scope
rules depending on the mode chosen at launch. Pick the mode that
matches how the AI was started.

## Mode comparison

| Capability | Workspace (default) | Global | Orchestrator |
|---|---|---|---|
| Tool surface | Full (~45 tools) | Full (~45 tools) | Lean 14 tools |
| CWD auto-scope | Yes | No | Yes |
| Behavior when CWD doesn't match a VK worktree | Graceful fallback (scope disabled, all tools work) | N/A — no scope | Launch fails |
| Scope protection on mutations | Yes | No | Yes |
| Intended use | AI chat inside a VK worktree | Admin scripts, CI, multi-workspace | VK-internal subagent spawning |

## Launch args

<Tabs>
  <Tab title="Workspace (default)">
  ```json
  "args": ["-y", "vibe-kanban@latest", "--mcp", "--mode", "workspace"]
  ```
  </Tab>
  <Tab title="Global">
  ```json
  "args": ["-y", "vibe-kanban@latest", "--mcp", "--mode", "global"]
  ```
  </Tab>
</Tabs>

Orchestrator mode is not a user-facing launch option; VK's executor
framework uses it internally.

## Scope protection semantics

In Workspace mode (and Orchestrator), the MCP server tracks a *scoped
workspace ID* derived from the agent's current working directory.

- **Read tools** (`list_*`, `get_*`) never perform scope checks — AI
  must be able to discover resources before operating on them.
- **Mutations that name a workspace ID** (for example
  `update_workspace`, `start_workspace`, `create_task(parent_workspace_id=...)`)
  are rejected unless the target is the scoped workspace or a descendant
  of it via the task parent-chain.
- **Mutations with implicit workspace** (session and repo-script tools)
  inherit scope from the resource they act on.

If the agent's CWD doesn't match any VK worktree, Workspace mode
disables the scope check for that session — equivalent to Global.

## When does scope protection kick in?

```
AI calls a mutation with workspace_id = X
│
├─ Mode is Global? ──── allow (no scope)
│
├─ Mode is Workspace/Orchestrator ┐
│                                 │
│                    scoped_workspace_id is None? ─── allow (graceful fallback)
│                                 │
│                    X == scope? ─── allow (self)
│                                 │
│                    X's task.parent_workspace_id == scope? ─── allow (child)
│                                 │
│                                 └── reject with error_kind="scope_denied"
```

## Tool availability by mode

**Workspace and Global** expose the full surface:

- `get_context`
- Workspaces: `list_workspaces`, `get_workspace`, `start_workspace`, `update_workspace`, `delete_workspace`
- Organizations: `list_organizations`, `get_organization`
- Repos: `list_repos`, `get_repo`, `add_repo`, `delete_repo`, `update_setup_script`, `update_cleanup_script`, `update_dev_server_script`
- Remote projects: `list_remote_projects`, `get_remote_project`
- Remote issues: `list_issues`, `get_issue`, `create_issue`, `update_issue`, `delete_issue`
- Issue assignees: `list_issue_assignees`, `add_issue_assignee`, `remove_issue_assignee`
- Issue tags: `list_issue_tags`, `add_issue_tag`, `remove_issue_tag`
- Remote tags: `list_tags`, `create_tag`, `update_tag`, `delete_tag`
- Issue relationships: `list_issue_relationships`, `add_issue_relationship`, `remove_issue_relationship`
- Task attempts: `list_task_attempts`, `get_task_attempt`
- Sessions: `list_sessions`, `create_session`, `update_session`, `read_session_messages`, `run_session_prompt`, `get_execution`
- Tasks: `list_tasks`, `get_task`, `create_task`, `create_and_start_task`, `update_task_status`, `delete_task`

**Orchestrator** exposes only:

- `get_context`, `update_workspace`
- Sessions: `create_session`, `list_sessions`, `read_session_messages`, `run_session_prompt`, `update_session`, `get_execution`
- Tasks: `list_tasks`, `get_task`, `create_task`, `create_and_start_task`, `update_task_status`, `delete_task`
```

- [ ] **Step 6.8: Insert nav entry in `docs/docs.json`**

Open `docs/docs.json`, find the `Integrations` navigation group, and insert `"integrations/mcp-modes"` immediately after `"integrations/mcp-server-configuration"`.

- [ ] **Step 6.9: Verify docs build**

If Mintlify local preview is available, run `mintlify dev` in `docs/` and manually spot-check that the new page renders and is linked from the nav.

Otherwise, ensure JSON is valid:

```bash
python3 -c "import json; json.load(open('docs/docs.json'))"
```

- [ ] **Step 6.10: Final full-tree checks**

```bash
pnpm run format
pnpm run check
pnpm run lint
pnpm run backend:check
cargo test --workspace
```

Expected: all PASS.

- [ ] **Step 6.11: Commit**

```bash
git add crates/executors/default_mcp.json \
        npx-cli/src/cli.ts \
        npx-cli/src/cli.test.ts \
        docs/integrations/vibe-kanban-mcp-server.mdx \
        docs/integrations/mcp-server-configuration.mdx \
        docs/integrations/mcp-modes.mdx \
        docs/docs.json
git commit -m "$(cat <<'EOF'
feat(config,docs): flip default MCP mode to Workspace and document modes

Flips `default_mcp.json` and `npx-cli/src/cli.ts` so fresh installs
default to `--mode workspace`. Existing implicit `["--mcp"]` configs
automatically upgrade on the next agent launch; explicit `--mode global`
users are unaffected.

Adds a new `docs/integrations/mcp-modes.mdx` reference page and
updates the two existing MCP docs pages to cover the default change,
the upgrade path for older configs, and the full tool-surface table
per mode.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Post-implementation verification checklist

Before opening the PR, run:

1. `pnpm run format` — format across all workspaces.
2. `pnpm run lint` — lint (web + cargo clippy).
3. `pnpm run check` — type-check frontend + Rust.
4. `cargo test --workspace` — all tests green.
5. Manually verify in a scratch environment:
   - Launch an agent inside a VK worktree → `get_context` returns the expected `workspace_id` and the 5 new tools are callable.
   - Launch an agent outside any VK worktree → `get_context` reports no scope, scope-enforced mutations still work (allow-all fallback).
   - Attempt `delete_repo` on a repo with an active workspace → receive 409 with workspaces list; retry with `force=true` → success.
   - Attempt `delete_tag` on a tag used by issue_tags → receive 409 with count; retry with `force=true` → success.
6. Verify docs nav: `docs/integrations/mcp-modes` appears immediately after MCP configuration.

---

## Notes for the implementer

- **TDD discipline:** follow the failing-test-first order in every step. If a step's test seems redundant (e.g. "Run the `force` query struct parses" in Step 4.1), keep it — it pins the shape so the refactor later can't silently drop it.
- **`send_json` vs `send_empty_json`:** the MCP tool layer already has both helpers (`tools/mod.rs:340-377`). Delete paths that don't return a data body should use `send_empty_json`; POST/PATCH responses that return a `MutationResponse<T>` body should use `send_json`.
- **Error surface parity:** every new mutation tool routes errors through `Self::tool_error(e)` so the existing `envelope_to_error` machinery (status / error_kind / error_data / body_tail) surfaces uniformly. Avoid inventing new error shapes.
- **`MutationResponse<T>` from `api-types`:** the backend returns `{ "data": T, "txid": 0 }` for v1 mutations (see `crates/server/src/routes/v1.rs:752`). The `api_types::MutationResponse<T>` shape matches. Tool-layer tests should mock that exact shape.
- **`with_scope_for_test` variants:** `McpServer::with_scope_for_test(workspace_id)` exists for Orchestrator (mod.rs:116). Verify by running `cargo check` — if Workspace tests need a companion or the existing helper works unchanged for both modes (it does: it only sets `context`, not `mode`), use it as-is.
- **`pnpm run generate-types`:** only needed once, after the `DeleteTagConflict` type is added in Task 5. Don't hand-edit `shared/types.ts` at any point.
- **Docs language:** the project uses British English (`behaviour`, `colour`) per `docs/CLAUDE.md` — follow the prevailing style when editing existing pages, and match it in the new `mcp-modes.mdx`.
