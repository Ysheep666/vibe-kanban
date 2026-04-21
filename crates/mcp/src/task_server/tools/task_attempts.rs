use db::models::requests::{
    CreateAndStartWorkspaceRequest, CreateAndStartWorkspaceResponse, LinkedIssueInfo,
    StartTaskRequest, StartTaskResponse, StartTaskTaskSpec, StartTaskWorkspaceSpec,
    WorkspaceRepoInput,
};
use executors::profile::ExecutorConfig;
use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{McpServer, ToolError, check_scope_allows_workspace};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpWorkspaceRepoInput {
    #[schemars(description = "The repository ID")]
    repo_id: Uuid,
    #[schemars(description = "The branch for this repository")]
    branch: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StartWorkspaceRequest {
    #[schemars(description = "Name for the workspace")]
    name: String,
    #[schemars(
        description = "Optional prompt for the first workspace session. If omitted/empty, the linked issue title/description is used."
    )]
    prompt: Option<String>,
    #[schemars(
        description = "The coding agent executor to run ('CLAUDE_CODE', 'AMP', 'GEMINI', 'CODEX', 'OPENCODE', 'CURSOR_AGENT', 'QWEN_CODE', 'COPILOT', 'DROID')"
    )]
    executor: String,
    #[schemars(description = "Optional executor variant, if needed")]
    variant: Option<String>,
    #[schemars(description = "Repository selection for the workspace")]
    repositories: Vec<McpWorkspaceRepoInput>,
    #[schemars(
        description = "Optional issue ID to link the workspace to. When provided, the workspace will be associated with this remote issue."
    )]
    issue_id: Option<Uuid>,
    #[schemars(
        description = "Optional parent workspace ID. When provided, the workspace is created atomically together with a new task via /api/tasks/start (D6) that is nested under the given parent workspace. Must not be combined with `issue_id`. Requires `project_id`."
    )]
    parent_workspace_id: Option<Uuid>,
    #[schemars(
        description = "Project ID for the task row when `parent_workspace_id` is set. Required whenever `parent_workspace_id` is provided."
    )]
    project_id: Option<Uuid>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct StartWorkspaceResponse {
    workspace_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LinkWorkspaceIssueRequest {
    #[schemars(description = "The workspace ID to link")]
    workspace_id: Uuid,
    #[schemars(description = "The issue ID to link the workspace to")]
    issue_id: Uuid,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct LinkWorkspaceIssueResponse {
    #[schemars(description = "Whether the linking was successful")]
    success: bool,
    #[schemars(description = "The workspace ID that was linked")]
    workspace_id: String,
    #[schemars(description = "The issue ID it was linked to")]
    issue_id: String,
}

fn build_workspace_prompt_from_issue(issue: &api_types::Issue) -> Option<String> {
    let title = issue.title.trim();
    let description = issue
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or_default();

    if title.is_empty() && description.is_empty() {
        return None;
    }

    if description.is_empty() {
        return Some(title.to_string());
    }

    if title.is_empty() {
        return Some(description.to_string());
    }

    Some(format!("{title}\n\n{description}"))
}

#[tool_router(router = task_attempts_tools_router, vis = "pub")]
impl McpServer {
    #[tool(
        description = "Create a new workspace and start its first session. When `parent_workspace_id` is set in Workspace or Orchestrator mode, the parent must be the scoped workspace or a descendant via the task parent-chain — otherwise the tool rejects with error_kind=\"scope_denied\". Cross-workspace parenting requires --mode global."
    )]
    async fn start_workspace(
        &self,
        Parameters(StartWorkspaceRequest {
            name,
            prompt,
            executor,
            variant,
            repositories,
            issue_id,
            parent_workspace_id,
            project_id,
        }): Parameters<StartWorkspaceRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        if repositories.is_empty() {
            return Self::err("At least one repository must be specified.", None::<&str>);
        }

        let executor_trimmed = executor.trim();
        if executor_trimmed.is_empty() {
            return Self::err("Executor must not be empty.", None::<&str>);
        }

        if parent_workspace_id.is_some() && issue_id.is_some() {
            return Ok(Self::tool_error(ToolError::message(
                "parent_workspace_id and issue_id cannot be combined",
            )));
        }

        let prompt = prompt.and_then(|prompt| {
            let trimmed = prompt.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let base_executor = match Self::parse_executor_agent(executor_trimmed) {
            Ok(exec) => exec,
            Err(_) => {
                return Self::err(
                    format!("Unknown executor '{executor_trimmed}'."),
                    None::<String>,
                );
            }
        };

        let variant = variant.and_then(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let workspace_repos: Vec<WorkspaceRepoInput> = repositories
            .into_iter()
            .map(|r| WorkspaceRepoInput {
                repo_id: r.repo_id,
                target_branch: r.branch,
            })
            .collect();

        // Parent-workspace branch: route through /api/tasks/start (atomic tx).
        if let Some(parent) = parent_workspace_id {
            // Keep `start_workspace` in lock-step with `create_and_start_task`
            // — both land at the same `/api/tasks/start` handler, so scope
            // enforcement must be symmetric. Short-circuits before any HTTP
            // round-trip to `/api/tasks/start`.
            if let Err(e) = self.enforce_parent_scope(parent).await {
                return Ok(Self::tool_error(e));
            }

            let workspace_prompt = match prompt {
                Some(prompt) => prompt,
                None => {
                    return Self::err(
                        "`prompt` is required when `parent_workspace_id` is set.",
                        None::<&str>,
                    );
                }
            };
            let project_id = match project_id {
                Some(id) => id,
                None => {
                    return Ok(Self::tool_error(
                        ToolError::message(
                            "`project_id` is required when `parent_workspace_id` is set",
                        )
                        .with_error_kind("missing_project_id"),
                    ));
                }
            };

            let payload = StartTaskRequest {
                task: StartTaskTaskSpec {
                    project_id,
                    title: name.clone(),
                    description: None,
                    parent_workspace_id: Some(parent),
                },
                workspace: StartTaskWorkspaceSpec {
                    name: Some(name),
                    repos: workspace_repos,
                    executor_config: ExecutorConfig {
                        executor: base_executor,
                        variant,
                        model_id: None,
                        agent_id: None,
                        reasoning_id: None,
                        permission_policy: None,
                    },
                    prompt: workspace_prompt,
                },
            };

            let url = self.url("/api/tasks/start");
            let response: StartTaskResponse =
                match self.send_json(self.client.post(&url).json(&payload)).await {
                    Ok(value) => value,
                    Err(e) => return Ok(Self::tool_error(e)),
                };

            return McpServer::success(&StartWorkspaceResponse {
                workspace_id: response.workspace_id.to_string(),
            });
        }

        let (linked_issue, issue_prompt) = if let Some(issue_id) = issue_id {
            let issue_url = self.url(&format!("/api/remote/issues/{issue_id}"));
            let issue: api_types::Issue = match self.send_json(self.client.get(&issue_url)).await {
                Ok(issue) => issue,
                Err(e) => return Ok(Self::tool_error(e)),
            };

            (
                Some(LinkedIssueInfo {
                    remote_project_id: issue.project_id,
                    issue_id,
                }),
                build_workspace_prompt_from_issue(&issue),
            )
        } else {
            (None, None)
        };

        let workspace_prompt = match prompt.or(issue_prompt) {
            Some(prompt) => prompt,
            None => {
                return Self::err(
                    "Provide `prompt`, or `issue_id` that has a non-empty title/description.",
                    None::<&str>,
                );
            }
        };

        let create_and_start_payload = CreateAndStartWorkspaceRequest {
            name: Some(name.clone()),
            repos: workspace_repos,
            linked_issue,
            executor_config: ExecutorConfig {
                executor: base_executor,
                variant,
                model_id: None,
                agent_id: None,
                reasoning_id: None,
                permission_policy: None,
            },
            prompt: workspace_prompt,
            attachment_ids: None,
            adopt_cursor_mcp_lobby_bridge_session_id: None,
        };

        let create_and_start_url = self.url("/api/workspaces/start");
        let create_and_start_response: CreateAndStartWorkspaceResponse = match self
            .send_json(
                self.client
                    .post(&create_and_start_url)
                    .json(&create_and_start_payload),
            )
            .await
        {
            Ok(response) => response,
            Err(e) => return Ok(Self::tool_error(e)),
        };

        // Link workspace to remote issue if issue_id is provided
        if let Some(issue_id) = issue_id
            && let Err(e) = self
                .link_workspace_to_issue(create_and_start_response.workspace.id, issue_id)
                .await
        {
            return Ok(Self::tool_error(e));
        }

        let response = StartWorkspaceResponse {
            workspace_id: create_and_start_response.workspace.id.to_string(),
        };

        McpServer::success(&response)
    }

    #[tool(
        description = "Link an existing workspace to a remote issue. In Workspace or Orchestrator mode, the target `workspace_id` must be the scoped workspace or a descendant via the task parent-chain — otherwise the tool rejects with error_kind=\"scope_denied\". Cross-workspace linking requires --mode global."
    )]
    async fn link_workspace_issue(
        &self,
        Parameters(LinkWorkspaceIssueRequest {
            workspace_id,
            issue_id,
        }): Parameters<LinkWorkspaceIssueRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        // Scope guard — keep `link_workspace_issue` in lock-step with
        // `update_workspace` / `delete_workspace`: a Workspace-mode agent
        // must not be able to attach an arbitrary other workspace to a
        // remote issue. Short-circuits before any HTTP round-trip.
        let mut scope_cache = std::collections::HashMap::new();
        if !check_scope_allows_workspace(self, &mut scope_cache, workspace_id).await {
            return Ok(Self::tool_error(self.scope_denied_error(workspace_id)));
        }

        if let Err(e) = self.link_workspace_to_issue(workspace_id, issue_id).await {
            return Ok(Self::tool_error(e));
        }

        McpServer::success(&LinkWorkspaceIssueResponse {
            success: true,
            workspace_id: workspace_id.to_string(),
            issue_id: issue_id.to_string(),
        })
    }
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Once;

    use httpmock::MockServer;
    use rmcp::handler::server::wrapper::Parameters;

    use super::*;

    static RUSTLS_PROVIDER: Once = Once::new();

    fn install_rustls() {
        RUSTLS_PROVIDER.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    /// Decode a tool-error `CallToolResult` body to JSON so `error_kind` can be
    /// asserted directly. Mirrors the helper in `tasks.rs` / `remote_tags.rs`.
    fn error_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
        assert!(
            result.is_error.unwrap_or(false),
            "expected tool error, got success: {result:?}"
        );
        let text = match &result.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        serde_json::from_str(&text).expect("tool error body must be JSON")
    }

    #[tokio::test]
    async fn workspace_mode_link_workspace_issue_rejects_unrelated_workspace() {
        install_rustls();
        let mock = MockServer::start();
        let scope = Uuid::new_v4();
        let unrelated = Uuid::new_v4();
        let unrelated_task_id = Uuid::new_v4();
        let other_parent = Uuid::new_v4();
        let issue_id = Uuid::new_v4();

        // Scope check climbs the chain: fetch the target workspace, then its
        // task row, and discovers the task's parent workspace is not the
        // scoped one — deny.
        mock.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/workspaces/{unrelated}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": unrelated.to_string(),
                    "task_id": unrelated_task_id.to_string(),
                    "container_ref": null,
                    "branch": "main",
                    "setup_completed_at": null,
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                    "archived": false,
                    "pinned": false,
                    "name": null,
                    "worktree_deleted": false,
                }
            }));
        });
        mock.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/tasks/{unrelated_task_id}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": unrelated_task_id.to_string(),
                    "project_id": Uuid::new_v4().to_string(),
                    "title": "t",
                    "description": null,
                    "status": "todo",
                    "parent_workspace_id": other_parent.to_string(),
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                }
            }));
        });
        // The issue lookup AND the link mutation must never fire when scope
        // denies — catch-all them with 500 so a regression surfaces loudly.
        let issue_guard = mock.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/remote/issues/{issue_id}"));
            then.status(500);
        });
        let link_guard = mock.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path(format!("/api/workspaces/{unrelated}/links"));
            then.status(500);
        });

        let server = McpServer::new_workspace(&mock.base_url()).with_scope_for_test(scope);
        let req = LinkWorkspaceIssueRequest {
            workspace_id: unrelated,
            issue_id,
        };
        let result = server.link_workspace_issue(Parameters(req)).await.unwrap();

        let body = error_json(&result);
        assert_eq!(
            body.get("error_kind").and_then(|v| v.as_str()),
            Some("scope_denied"),
            "expected scope_denied: {body}"
        );
        assert_eq!(
            issue_guard.hits(),
            0,
            "GET /api/remote/issues/{{id}} must not fire when scope denies",
        );
        assert_eq!(
            link_guard.hits(),
            0,
            "POST /api/workspaces/{{id}}/links must not fire when scope denies",
        );
    }

    #[tokio::test]
    async fn workspace_mode_link_workspace_issue_allows_scoped_workspace() {
        // Complement the negative-path test above: when the caller targets
        // the *scoped* workspace, the guard must not short-circuit — the
        // tool must reach the issue lookup and the link mutation, and return
        // success. Lock this in so a future refactor can't accidentally
        // widen the guard to also deny the scope itself.
        install_rustls();
        let mock = MockServer::start();
        let scope = Uuid::new_v4();
        let issue_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let status_id = Uuid::new_v4();

        // `workspace_id == scope` → `check_scope_allows_workspace` returns
        // true immediately without any HTTP round-trip. Any GET against
        // `/api/workspaces/{scope}` in this test would indicate the guard
        // started climbing a parent chain it should not — catch-all 500s
        // make that regression surface loudly.
        let scope_guard = mock.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/workspaces/{scope}"));
            then.status(500);
        });

        let issue_hit = mock.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/remote/issues/{issue_id}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": issue_id.to_string(),
                    "project_id": project_id.to_string(),
                    "issue_number": 1,
                    "simple_id": "ABC-1",
                    "status_id": status_id.to_string(),
                    "title": "test issue",
                    "description": null,
                    "priority": null,
                    "start_date": null,
                    "target_date": null,
                    "completed_at": null,
                    "sort_order": 1.0,
                    "parent_issue_id": null,
                    "parent_issue_sort_order": null,
                    "extension_metadata": {},
                    "creator_user_id": null,
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                }
            }));
        });

        // The link endpoint is the mutation under test — assert it got the
        // expected JSON body, not just that it was hit.
        let link_hit = mock.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path(format!("/api/workspaces/{scope}/links"))
                .json_body(serde_json::json!({
                    "project_id": project_id.to_string(),
                    "issue_id": issue_id.to_string(),
                }));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": null,
            }));
        });

        let server = McpServer::new_workspace(&mock.base_url()).with_scope_for_test(scope);
        let req = LinkWorkspaceIssueRequest {
            workspace_id: scope,
            issue_id,
        };
        let result = server.link_workspace_issue(Parameters(req)).await.unwrap();

        assert!(
            !result.is_error.unwrap_or(false),
            "in-scope link_workspace_issue must succeed, got error: {result:?}",
        );
        assert_eq!(
            scope_guard.hits(),
            0,
            "self-scope target must not trigger a parent-chain climb",
        );
        assert_eq!(issue_hit.hits(), 1, "issue lookup must fire on success path");
        assert_eq!(link_hit.hits(), 1, "link mutation must fire on success path");
    }

    #[tokio::test]
    async fn workspace_mode_start_workspace_rejects_unrelated_parent() {
        // `start_workspace` with `parent_workspace_id` routes through
        // `POST /api/tasks/start` just like `create_and_start_task` — and
        // mcp-modes.mdx promises "Mutations that name a workspace ID are
        // rejected unless the target is the scoped workspace or a
        // descendant". Keep `start_workspace` in lock-step with
        // `create_and_start_task` (tasks.rs) rather than leaving a second,
        // silently-unguarded entrypoint onto the same atomic backend tx.
        install_rustls();
        let mock = MockServer::start();
        let scope = Uuid::new_v4();
        let unrelated_parent = Uuid::new_v4();
        let unrelated_parent_task = Uuid::new_v4();
        let other_parent = Uuid::new_v4();

        // Scope climb: parent workspace → its task → task's parent workspace
        // is `other_parent`, not the scoped workspace, so deny.
        mock.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/workspaces/{unrelated_parent}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": unrelated_parent.to_string(),
                    "task_id": unrelated_parent_task.to_string(),
                    "container_ref": null,
                    "branch": "main",
                    "setup_completed_at": null,
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                    "archived": false,
                    "pinned": false,
                    "name": null,
                    "worktree_deleted": false,
                }
            }));
        });
        mock.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/tasks/{unrelated_parent_task}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": unrelated_parent_task.to_string(),
                    "project_id": Uuid::new_v4().to_string(),
                    "title": "t",
                    "description": null,
                    "status": "todo",
                    "parent_workspace_id": other_parent.to_string(),
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                }
            }));
        });
        // `POST /api/tasks/start` must NEVER fire when scope denies — catch
        // it with 500 so a regression (guard dropped, or new code path
        // bypassing the guard) surfaces loudly instead of silently
        // consuming backend capacity.
        let start_guard = mock.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/tasks/start");
            then.status(500);
        });

        let server = McpServer::new_workspace(&mock.base_url()).with_scope_for_test(scope);
        let req = StartWorkspaceRequest {
            name: "child-ws".to_string(),
            prompt: Some("do the thing".to_string()),
            executor: "CLAUDE_CODE".to_string(),
            variant: None,
            repositories: vec![McpWorkspaceRepoInput {
                repo_id: Uuid::new_v4(),
                branch: "main".to_string(),
            }],
            issue_id: None,
            parent_workspace_id: Some(unrelated_parent),
            project_id: Some(Uuid::new_v4()),
        };
        let result = server.start_workspace(Parameters(req)).await.unwrap();

        let body = error_json(&result);
        assert_eq!(
            body.get("error_kind").and_then(|v| v.as_str()),
            Some("scope_denied"),
            "expected scope_denied: {body}"
        );
        assert_eq!(
            start_guard.hits(),
            0,
            "POST /api/tasks/start must not fire when the parent workspace is out of scope",
        );
    }
}
