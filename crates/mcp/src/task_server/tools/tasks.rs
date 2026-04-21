use std::str::FromStr;

use db::models::{
    requests::{
        StartTaskRequest, StartTaskResponse, StartTaskTaskSpec, StartTaskWorkspaceSpec,
        WorkspaceRepoInput,
    },
    task::{Task, TaskStatus},
};
use executors::profile::ExecutorConfig;
use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{McpMode, McpServer, ToolError, check_scope_allows_workspace};

// --------------------------------------------------------------------------
// Request / response shapes
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpCreateTaskRequest {
    #[schemars(
        description = "Project ID that owns the new task. Optional when running inside a scoped MCP where project_id is known from context."
    )]
    project_id: Option<Uuid>,
    #[schemars(description = "Task title (non-empty).")]
    title: String,
    #[schemars(description = "Optional task description.")]
    description: Option<String>,
    #[schemars(
        description = "Parent workspace ID. In orchestrator mode, defaults to the scoped workspace when omitted and must equal or be descended from it."
    )]
    parent_workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpWorkspaceRepoInput {
    #[schemars(description = "The repository ID")]
    repo_id: Uuid,
    #[schemars(description = "The branch for this repository")]
    branch: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpCreateAndStartTaskRequest {
    #[schemars(
        description = "Project ID that owns the task. Optional when running inside a scoped MCP where project_id is known from context."
    )]
    project_id: Option<Uuid>,
    #[schemars(description = "Task title (non-empty).")]
    title: String,
    #[schemars(description = "Optional task description.")]
    description: Option<String>,
    #[schemars(
        description = "Parent workspace ID. In orchestrator mode, defaults to the scoped workspace when omitted and must equal or be descended from it."
    )]
    parent_workspace_id: Option<Uuid>,
    #[schemars(description = "Optional display name for the workspace that will be created.")]
    workspace_name: Option<String>,
    #[schemars(description = "Repository selection for the workspace (must not be empty).")]
    repositories: Vec<McpWorkspaceRepoInput>,
    #[schemars(
        description = "The coding agent executor to run ('CLAUDE_CODE', 'AMP', 'GEMINI', 'CODEX', 'OPENCODE', 'CURSOR_AGENT', 'QWEN_CODE', 'COPILOT', 'DROID')"
    )]
    executor: String,
    #[schemars(description = "Optional executor variant, if needed")]
    variant: Option<String>,
    #[schemars(description = "Prompt for the first workspace session (non-empty).")]
    prompt: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpListTasksRequest {
    #[schemars(
        description = "Parent workspace ID. Required in global mode; in orchestrator mode defaults to the scoped workspace when omitted."
    )]
    parent_workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpGetTaskRequest {
    #[schemars(description = "Task ID to fetch.")]
    task_id: Uuid,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpUpdateTaskStatusRequest {
    #[schemars(description = "Task ID to update.")]
    task_id: Uuid,
    #[schemars(
        description = "New status: one of 'todo', 'inprogress', 'inreview', 'done', 'cancelled'."
    )]
    status: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpDeleteTaskRequest {
    #[schemars(description = "Task ID to delete.")]
    task_id: Uuid,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TaskSummary {
    id: String,
    project_id: String,
    title: String,
    description: Option<String>,
    /// Wire format: lowercase string.
    #[schemars(with = "String")]
    status: TaskStatus,
    parent_workspace_id: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct CreateTaskResponse {
    task: TaskSummary,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct CreateAndStartTaskResponse {
    task_id: String,
    workspace_id: String,
    execution_id: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ListTasksResponse {
    #[schemars(description = "Parent workspace ID the results are scoped to.")]
    parent_workspace_id: String,
    total_count: usize,
    tasks: Vec<TaskSummary>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct UpdateTaskStatusResponse {
    success: bool,
    task_id: String,
    /// Wire format: lowercase string.
    #[schemars(with = "String")]
    status: TaskStatus,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DeleteTaskResponse {
    success: bool,
    task_id: String,
}

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

impl McpServer {
    fn task_summary(task: Task) -> TaskSummary {
        TaskSummary {
            id: task.id.to_string(),
            project_id: task.project_id.to_string(),
            title: task.title,
            description: task.description,
            status: task.status,
            parent_workspace_id: task.parent_workspace_id.map(|id| id.to_string()),
            created_at: task.created_at.to_rfc3339(),
            updated_at: task.updated_at.to_rfc3339(),
        }
    }

    /// Resolve the `parent_workspace_id` that a task-creating tool should
    /// use. In orchestrator/workspace mode, auto-fill with the scoped
    /// workspace when the caller didn't supply one. In global mode the
    /// caller decides.
    fn resolve_parent_workspace_id(&self, explicit: Option<Uuid>) -> Option<Uuid> {
        if explicit.is_some() {
            return explicit;
        }
        if matches!(self.mode(), McpMode::Orchestrator | McpMode::Workspace) {
            return self.scoped_workspace_id();
        }
        None
    }

    /// Validate that the supplied `parent_workspace_id` is within scope for
    /// orchestrator mode. Returns Ok(()) for global mode or out-of-scope
    /// cases handled elsewhere. Returns Err with a ready-to-use ToolError
    /// when the parent is denied.
    ///
    /// Exposed as `pub(super)` so sibling modules (e.g. `task_attempts`
    /// which also accepts `parent_workspace_id` via `start_workspace`) can
    /// reuse the same guard without duplicating the scope-cache boilerplate.
    pub(super) async fn enforce_parent_scope(&self, parent: Uuid) -> Result<(), ToolError> {
        let mut scope_cache = std::collections::HashMap::new();
        if !check_scope_allows_workspace(self, &mut scope_cache, parent).await {
            return Err(self.scope_denied_error(parent));
        }
        Ok(())
    }

    /// For tools that operate on an existing `Task`, verify the task's
    /// `parent_workspace_id` lies within the orchestrator/workspace scope.
    ///
    /// - Global mode → always allowed.
    /// - Orchestrator/Workspace without a `scoped_workspace_id` → allowed
    ///   (no scope set — Workspace graceful fallback, or Orchestrator init
    ///   test paths).
    /// - Orchestrator/Workspace with scope AND task has no parent → denied
    ///   (D12 only relaxes for parent→child; top-level tasks stay out-of-scope).
    /// - Orchestrator/Workspace with scope AND task has parent →
    ///   `check_scope_allows_workspace(parent)`.
    async fn require_parent_in_scope(
        &self,
        task: &db::models::task::Task,
        scope_cache: &mut std::collections::HashMap<Uuid, bool>,
    ) -> Result<(), ToolError> {
        if matches!(self.mode(), McpMode::Global) {
            return Ok(());
        }
        if self.scoped_workspace_id().is_none() {
            return Ok(());
        }
        let parent = match task.parent_workspace_id {
            Some(p) => p,
            None => return Err(self.scope_denied_error(task.id)),
        };
        if !check_scope_allows_workspace(self, scope_cache, parent).await {
            return Err(self.scope_denied_error(task.id));
        }
        Ok(())
    }

    fn parse_task_status(raw: &str) -> Result<TaskStatus, ToolError> {
        let normalized = raw.trim().to_ascii_lowercase();
        // `TaskStatus` is `#[strum(serialize_all = "lowercase")]`, so the
        // trimmed lowercase input matches either `FromStr`.
        TaskStatus::from_str(&normalized).map_err(|_| {
            ToolError::message(format!(
                "Unknown task status '{raw}'. Valid values: 'todo', 'inprogress', 'inreview', 'done', 'cancelled'."
            ))
        })
    }
}

// --------------------------------------------------------------------------
// Tool router
// --------------------------------------------------------------------------

#[tool_router(router = tasks_tools_router, vis = "pub")]
impl McpServer {
    #[tool(
        description = "Create a new task row without starting a workspace. In orchestrator mode, `parent_workspace_id` defaults to the scoped workspace."
    )]
    async fn create_task(
        &self,
        Parameters(McpCreateTaskRequest {
            project_id,
            title,
            description,
            parent_workspace_id,
        }): Parameters<McpCreateTaskRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let title = title.trim();
        if title.is_empty() {
            return Self::err("title must not be empty", None::<&str>);
        }

        let project_id = match self.resolve_project_id(project_id) {
            Ok(id) => id,
            Err(error) => return Ok(Self::tool_error(error)),
        };

        let parent_workspace_id = self.resolve_parent_workspace_id(parent_workspace_id);
        if let Some(parent) = parent_workspace_id
            && let Err(error) = self.enforce_parent_scope(parent).await
        {
            return Ok(Self::tool_error(error));
        }

        let description = description.and_then(|d| {
            let trimmed = d.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let payload = serde_json::json!({
            "project_id": project_id,
            "title": title,
            "description": description,
            "parent_workspace_id": parent_workspace_id,
        });

        let url = self.url("/api/tasks");
        let task: Task = match self.send_json(self.client.post(&url).json(&payload)).await {
            Ok(value) => value,
            Err(error) => return Ok(Self::tool_error(error)),
        };

        Self::success(&CreateTaskResponse {
            task: Self::task_summary(task),
        })
    }

    #[tool(
        description = "Atomically create a task, workspace, and first session via /api/tasks/start. In orchestrator mode, `parent_workspace_id` defaults to the scoped workspace."
    )]
    async fn create_and_start_task(
        &self,
        Parameters(McpCreateAndStartTaskRequest {
            project_id,
            title,
            description,
            parent_workspace_id,
            workspace_name,
            repositories,
            executor,
            variant,
            prompt,
        }): Parameters<McpCreateAndStartTaskRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let title = title.trim();
        if title.is_empty() {
            return Self::err("title must not be empty", None::<&str>);
        }
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Self::err("prompt must not be empty", None::<&str>);
        }
        if repositories.is_empty() {
            return Self::err("At least one repository must be specified.", None::<&str>);
        }

        let executor_trimmed = executor.trim();
        if executor_trimmed.is_empty() {
            return Self::err("Executor must not be empty.", None::<&str>);
        }
        let base_executor = match Self::parse_executor_agent(executor_trimmed) {
            Ok(agent) => agent,
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

        let project_id = match self.resolve_project_id(project_id) {
            Ok(id) => id,
            Err(error) => return Ok(Self::tool_error(error)),
        };

        let parent_workspace_id = self.resolve_parent_workspace_id(parent_workspace_id);
        if let Some(parent) = parent_workspace_id
            && let Err(error) = self.enforce_parent_scope(parent).await
        {
            return Ok(Self::tool_error(error));
        }

        let description = description.and_then(|d| {
            let trimmed = d.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let workspace_name = workspace_name.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let repos: Vec<WorkspaceRepoInput> = repositories
            .into_iter()
            .map(|r| WorkspaceRepoInput {
                repo_id: r.repo_id,
                target_branch: r.branch,
            })
            .collect();

        let payload = StartTaskRequest {
            task: StartTaskTaskSpec {
                project_id,
                title: title.to_string(),
                description,
                parent_workspace_id,
            },
            workspace: StartTaskWorkspaceSpec {
                name: workspace_name,
                repos,
                executor_config: ExecutorConfig {
                    executor: base_executor,
                    variant,
                    model_id: None,
                    agent_id: None,
                    reasoning_id: None,
                    permission_policy: None,
                },
                prompt: prompt.to_string(),
            },
        };

        let url = self.url("/api/tasks/start");
        let response: StartTaskResponse =
            match self.send_json(self.client.post(&url).json(&payload)).await {
                Ok(value) => value,
                Err(error) => return Ok(Self::tool_error(error)),
            };

        Self::success(&CreateAndStartTaskResponse {
            task_id: response.task_id.to_string(),
            workspace_id: response.workspace_id.to_string(),
            execution_id: response.execution_id.to_string(),
        })
    }

    #[tool(
        description = "List tasks filtered by a parent workspace. In orchestrator mode, `parent_workspace_id` defaults to the scoped workspace; in global mode it is required."
    )]
    async fn list_tasks(
        &self,
        Parameters(McpListTasksRequest {
            parent_workspace_id,
        }): Parameters<McpListTasksRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let parent = match self.resolve_parent_workspace_id(parent_workspace_id) {
            Some(id) => id,
            None => {
                return Ok(Self::tool_error(
                    ToolError::message("parent_workspace_id is required in global mode")
                        .with_error_kind("missing_parent_workspace_id"),
                ));
            }
        };

        if let Err(error) = self.enforce_parent_scope(parent).await {
            return Ok(Self::tool_error(error));
        }

        let url = self.url(&format!("/api/tasks?parent_workspace_id={}", parent));
        let tasks: Vec<Task> = match self.send_json(self.client.get(&url)).await {
            Ok(value) => value,
            Err(error) => return Ok(Self::tool_error(error)),
        };

        let summaries = tasks
            .into_iter()
            .map(Self::task_summary)
            .collect::<Vec<_>>();

        Self::success(&ListTasksResponse {
            parent_workspace_id: parent.to_string(),
            total_count: summaries.len(),
            tasks: summaries,
        })
    }

    #[tool(description = "Fetch a single task by id.")]
    async fn get_task(
        &self,
        Parameters(McpGetTaskRequest { task_id }): Parameters<McpGetTaskRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let task_url = self.url(&format!("/api/tasks/{task_id}"));
        let task: Task = match self.send_json(self.client.get(&task_url)).await {
            Ok(value) => value,
            Err(error) => return Ok(Self::tool_error(error)),
        };

        let mut scope_cache = std::collections::HashMap::new();
        if let Err(e) = self.require_parent_in_scope(&task, &mut scope_cache).await {
            return Ok(Self::tool_error(e));
        }

        Self::success(&Self::task_summary(task))
    }

    #[tool(description = "Update a task's status (title/description are not editable here).")]
    async fn update_task_status(
        &self,
        Parameters(McpUpdateTaskStatusRequest { task_id, status }): Parameters<
            McpUpdateTaskStatusRequest,
        >,
    ) -> Result<CallToolResult, ErrorData> {
        let new_status = match Self::parse_task_status(&status) {
            Ok(value) => value,
            Err(error) => return Ok(Self::tool_error(error)),
        };

        // Fetch first for scope enforcement.
        let task_url = self.url(&format!("/api/tasks/{task_id}"));
        let task: Task = match self.send_json(self.client.get(&task_url)).await {
            Ok(value) => value,
            Err(error) => return Ok(Self::tool_error(error)),
        };

        let mut scope_cache = std::collections::HashMap::new();
        if let Err(e) = self.require_parent_in_scope(&task, &mut scope_cache).await {
            return Ok(Self::tool_error(e));
        }

        let payload = serde_json::json!({ "status": new_status });
        if let Err(error) = self
            .send_empty_json(self.client.put(&task_url).json(&payload))
            .await
        {
            return Ok(Self::tool_error(error));
        }

        Self::success(&UpdateTaskStatusResponse {
            success: true,
            task_id: task_id.to_string(),
            status: new_status,
        })
    }

    #[tool(
        description = "Delete a task. Workspaces referencing the task have their task_id cleared (workspaces are not cascade-deleted)."
    )]
    async fn delete_task(
        &self,
        Parameters(McpDeleteTaskRequest { task_id }): Parameters<McpDeleteTaskRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        // Fetch first for scope enforcement.
        let task_url = self.url(&format!("/api/tasks/{task_id}"));
        let task: Task = match self.send_json(self.client.get(&task_url)).await {
            Ok(value) => value,
            Err(error) => return Ok(Self::tool_error(error)),
        };

        let mut scope_cache = std::collections::HashMap::new();
        if let Err(e) = self.require_parent_in_scope(&task, &mut scope_cache).await {
            return Ok(Self::tool_error(e));
        }

        if let Err(error) = self.send_empty_json(self.client.delete(&task_url)).await {
            return Ok(Self::tool_error(error));
        }

        Self::success(&DeleteTaskResponse {
            success: true,
            task_id: task_id.to_string(),
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

    #[tokio::test]
    async fn list_tasks_without_scope_and_without_explicit_errors() {
        install_rustls();
        // Global mode, no parent_workspace_id arg -> tool returns
        // `missing_parent_workspace_id` error without any HTTP roundtrip.
        let mock = MockServer::start();
        // Catch-all that would register a hit if the tool accidentally called out.
        let catch_all = mock.mock(|when, then| {
            when.any_request();
            then.status(500);
        });

        let server = McpServer::new_global(&mock.base_url());
        let req = McpListTasksRequest {
            parent_workspace_id: None,
        };
        let result = server.list_tasks(Parameters(req)).await.unwrap();

        let rendered = format!("{:?}", result);
        assert!(
            rendered.contains("parent_workspace_id"),
            "expected error mentioning parent_workspace_id: {rendered}"
        );
        assert!(
            rendered.contains("missing_parent_workspace_id"),
            "expected error_kind missing_parent_workspace_id: {rendered}"
        );
        assert_eq!(catch_all.hits(), 0);
    }

    #[tokio::test]
    async fn create_and_start_task_round_trip() {
        install_rustls();
        let mock = MockServer::start();
        let task_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        let execution_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let repo_id = Uuid::new_v4();

        let start_mock = mock.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/tasks/start");
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "task_id": task_id.to_string(),
                    "workspace_id": workspace_id.to_string(),
                    "execution_id": execution_id.to_string(),
                }
            }));
        });

        let server = McpServer::new_global(&mock.base_url());
        let req = McpCreateAndStartTaskRequest {
            project_id: Some(project_id),
            title: "new task".to_string(),
            description: None,
            parent_workspace_id: None,
            workspace_name: Some("ws-1".to_string()),
            repositories: vec![McpWorkspaceRepoInput {
                repo_id,
                branch: "main".to_string(),
            }],
            executor: "CODEX".to_string(),
            variant: None,
            prompt: "do the thing".to_string(),
        };
        let result = server.create_and_start_task(Parameters(req)).await.unwrap();

        let rendered = format!("{:?}", result);
        assert!(
            rendered.contains(&task_id.to_string()),
            "expected task_id in response: {rendered}"
        );
        assert!(
            rendered.contains(&workspace_id.to_string()),
            "expected workspace_id in response: {rendered}"
        );
        assert!(
            rendered.contains(&execution_id.to_string()),
            "expected execution_id in response: {rendered}"
        );
        start_mock.assert_hits(1);
    }

    #[test]
    fn parse_task_status_accepts_canonical_variants() {
        assert_eq!(
            McpServer::parse_task_status("todo").unwrap(),
            TaskStatus::Todo
        );
        assert_eq!(
            McpServer::parse_task_status("InProgress").unwrap(),
            TaskStatus::InProgress
        );
        assert_eq!(
            McpServer::parse_task_status(" CANCELLED ").unwrap(),
            TaskStatus::Cancelled
        );
    }

    #[test]
    fn parse_task_status_rejects_unknown() {
        let err = McpServer::parse_task_status("frobnicated").unwrap_err();
        assert!(err.to_string().contains("Unknown task status"));
    }

    // ----------------------------------------------------------------------
    // `require_parent_in_scope` — Workspace-mode direct unit coverage.
    //
    // Workspace mode was added to the enforcement branch in the same commit
    // that introduced Task 2 of the v4.2 Workspace-mode plan. Prior to that,
    // only Orchestrator mode rejected out-of-scope parent tasks; Workspace
    // mode silently allowed them. The tests below pin the Workspace branches
    // so regressions (e.g. reverting to `matches!(.., McpMode::Orchestrator)`)
    // surface immediately rather than only transitively through per-tool tests.
    // ----------------------------------------------------------------------

    /// Build a `Task` fixture by deserializing JSON — avoids depending on
    /// chrono directly just to stamp `created_at`/`updated_at`.
    fn mock_task(id: Uuid, parent_workspace_id: Option<Uuid>) -> Task {
        serde_json::from_value(serde_json::json!({
            "id": id.to_string(),
            "project_id": Uuid::new_v4().to_string(),
            "title": "t",
            "description": null,
            "status": "todo",
            "parent_workspace_id": parent_workspace_id.map(|p| p.to_string()),
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z",
        }))
        .expect("mock Task fixture must deserialize")
    }

    #[tokio::test]
    async fn workspace_mode_require_parent_in_scope_rejects_task_with_no_parent() {
        install_rustls();
        let mock_server = MockServer::start();
        // No HTTP calls should be needed — the `parent_workspace_id.is_none()`
        // branch denies immediately. A catch-all fails the test if that
        // invariant regresses.
        let catch_all = mock_server.mock(|when, then| {
            when.any_request();
            then.status(500);
        });

        let scope = Uuid::new_v4();
        let task = mock_task(Uuid::new_v4(), None);

        let server = McpServer::new_workspace(&mock_server.base_url()).with_scope_for_test(scope);

        let mut cache = std::collections::HashMap::new();
        let result = server.require_parent_in_scope(&task, &mut cache).await;
        assert!(
            result.is_err(),
            "Workspace-mode with scope set must reject tasks that have no parent_workspace_id",
        );
        assert_eq!(catch_all.hits(), 0);
    }

    #[tokio::test]
    async fn workspace_mode_require_parent_in_scope_rejects_unrelated_parent() {
        install_rustls();
        let mock_server = MockServer::start();
        let scope = Uuid::new_v4();
        let unrelated_parent = Uuid::new_v4();
        let task_parent_ws_id = Uuid::new_v4();
        let grand_parent_task_id = Uuid::new_v4();

        // `check_scope_allows_workspace` climbs the parent chain: it fetches
        // the target workspace, then the task that workspace belongs to, then
        // compares that task's parent_workspace_id to the scope. Mock both so
        // the chain terminates in an unrelated parent (not the scope).
        mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/workspaces/{task_parent_ws_id}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": task_parent_ws_id.to_string(),
                    "task_id": grand_parent_task_id.to_string(),
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
        mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/tasks/{grand_parent_task_id}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": grand_parent_task_id.to_string(),
                    "project_id": Uuid::new_v4().to_string(),
                    "title": "parent-task",
                    "description": null,
                    "status": "todo",
                    "parent_workspace_id": unrelated_parent.to_string(),
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                }
            }));
        });

        let task = mock_task(Uuid::new_v4(), Some(task_parent_ws_id));
        let server = McpServer::new_workspace(&mock_server.base_url()).with_scope_for_test(scope);

        let mut cache = std::collections::HashMap::new();
        let result = server.require_parent_in_scope(&task, &mut cache).await;
        assert!(
            result.is_err(),
            "Workspace-mode with scope set must reject tasks whose parent chain does not reach the scope",
        );
    }

    #[tokio::test]
    async fn workspace_mode_require_parent_in_scope_allows_scope_self_parent() {
        install_rustls();
        let mock_server = MockServer::start();
        // parent_workspace_id == scope → `check_scope_allows_workspace`
        // short-circuits to true without any HTTP call.
        let catch_all = mock_server.mock(|when, then| {
            when.any_request();
            then.status(500);
        });

        let scope = Uuid::new_v4();
        let task = mock_task(Uuid::new_v4(), Some(scope));

        let server = McpServer::new_workspace(&mock_server.base_url()).with_scope_for_test(scope);

        let mut cache = std::collections::HashMap::new();
        let result = server.require_parent_in_scope(&task, &mut cache).await;
        assert!(
            result.is_ok(),
            "Workspace-mode task whose parent IS the scope must be allowed",
        );
        assert_eq!(catch_all.hits(), 0);
    }

    // ----------------------------------------------------------------------
    // Tool-level coverage for `create_task` + `create_and_start_task` in
    // Workspace mode.
    //
    // `require_parent_in_scope_*` above covers the in-memory guard directly.
    // These tests exercise the full tool path: parameters → scope resolution
    // → HTTP mutation (or refusal to mutate). They lock two behaviours:
    //
    //   1. When `parent_workspace_id` is omitted, Workspace mode auto-fills
    //      it with the scoped workspace id before sending the payload.
    //   2. When `parent_workspace_id` is explicit and unrelated to the
    //      scope, the tool rejects with `error_kind="scope_denied"` and
    //      does not reach the mutation endpoint (POST /api/tasks or
    //      POST /api/tasks/start).
    // ----------------------------------------------------------------------

    /// Render a tool `CallToolResult` to JSON so individual fields (notably
    /// `error_kind`) can be asserted on directly. Mirrors the helper in
    /// `remote_tags.rs` tests.
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
    async fn workspace_mode_create_task_auto_defaults_parent_to_scope() {
        install_rustls();
        let mock = MockServer::start();
        let scope = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();

        // `json_body_partial` on the create payload locks the defaulting
        // behaviour: when the caller omits `parent_workspace_id` in
        // Workspace mode, the server must still see it populated with the
        // scoped workspace id (the Workspace-mode equivalent of
        // Orchestrator's default).
        let create_mock = mock.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/tasks")
                .json_body_partial(
                    serde_json::json!({
                        "parent_workspace_id": scope.to_string(),
                    })
                    .to_string(),
                );
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": task_id.to_string(),
                    "project_id": project_id.to_string(),
                    "title": "t",
                    "description": null,
                    "status": "todo",
                    "parent_workspace_id": scope.to_string(),
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                }
            }));
        });

        let server = McpServer::new_workspace(&mock.base_url()).with_scope_for_test(scope);
        let req = McpCreateTaskRequest {
            project_id: Some(project_id),
            title: "child task".to_string(),
            description: None,
            parent_workspace_id: None,
        };
        let result = server.create_task(Parameters(req)).await.unwrap();
        assert!(
            !result.is_error.unwrap_or(false),
            "create_task should succeed when parent defaults to scope: {result:?}"
        );
        create_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn workspace_mode_create_task_rejects_unrelated_parent() {
        install_rustls();
        let mock = MockServer::start();
        let scope = Uuid::new_v4();
        let unrelated_parent = Uuid::new_v4();
        let intermediate_task_id = Uuid::new_v4();
        let unrelated_grand_parent = Uuid::new_v4();
        let project_id = Uuid::new_v4();

        // The unrelated parent workspace exists and belongs to a task whose
        // parent_workspace_id is not the scope — scope check climbs the chain,
        // finds the mismatch, and denies. The POST /api/tasks mutation must
        // never fire.
        mock.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/workspaces/{unrelated_parent}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": unrelated_parent.to_string(),
                    "task_id": intermediate_task_id.to_string(),
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
                .path(format!("/api/tasks/{intermediate_task_id}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": intermediate_task_id.to_string(),
                    "project_id": Uuid::new_v4().to_string(),
                    "title": "parent-task",
                    "description": null,
                    "status": "todo",
                    "parent_workspace_id": unrelated_grand_parent.to_string(),
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                }
            }));
        });
        // Any mutation would be a regression — fail the test loudly.
        let create_guard = mock.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/tasks");
            then.status(500);
        });

        let server = McpServer::new_workspace(&mock.base_url()).with_scope_for_test(scope);
        let req = McpCreateTaskRequest {
            project_id: Some(project_id),
            title: "child task".to_string(),
            description: None,
            parent_workspace_id: Some(unrelated_parent),
        };
        let result = server.create_task(Parameters(req)).await.unwrap();

        let body = error_json(&result);
        assert_eq!(
            body.get("error_kind").and_then(|v| v.as_str()),
            Some("scope_denied"),
            "expected scope_denied: {body}"
        );
        assert_eq!(
            create_guard.hits(),
            0,
            "POST /api/tasks must not fire when parent is out of scope",
        );
    }

    #[tokio::test]
    async fn workspace_mode_create_and_start_task_auto_defaults_parent_to_scope() {
        install_rustls();
        let mock = MockServer::start();
        let scope = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let repo_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let new_workspace_id = Uuid::new_v4();
        let execution_id = Uuid::new_v4();

        let start_mock = mock.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/tasks/start")
                .json_body_partial(
                    serde_json::json!({
                        "task": { "parent_workspace_id": scope.to_string() },
                    })
                    .to_string(),
                );
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "task_id": task_id.to_string(),
                    "workspace_id": new_workspace_id.to_string(),
                    "execution_id": execution_id.to_string(),
                }
            }));
        });

        let server = McpServer::new_workspace(&mock.base_url()).with_scope_for_test(scope);
        let req = McpCreateAndStartTaskRequest {
            project_id: Some(project_id),
            title: "child task".to_string(),
            description: None,
            parent_workspace_id: None,
            workspace_name: Some("child-ws".to_string()),
            repositories: vec![McpWorkspaceRepoInput {
                repo_id,
                branch: "main".to_string(),
            }],
            executor: "CODEX".to_string(),
            variant: None,
            prompt: "go".to_string(),
        };
        let result = server.create_and_start_task(Parameters(req)).await.unwrap();
        assert!(
            !result.is_error.unwrap_or(false),
            "create_and_start_task should succeed when parent defaults to scope: {result:?}"
        );
        start_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn workspace_mode_create_and_start_task_rejects_unrelated_parent() {
        install_rustls();
        let mock = MockServer::start();
        let scope = Uuid::new_v4();
        let unrelated_parent = Uuid::new_v4();
        let intermediate_task_id = Uuid::new_v4();
        let unrelated_grand_parent = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let repo_id = Uuid::new_v4();

        mock.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/workspaces/{unrelated_parent}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": unrelated_parent.to_string(),
                    "task_id": intermediate_task_id.to_string(),
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
                .path(format!("/api/tasks/{intermediate_task_id}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": intermediate_task_id.to_string(),
                    "project_id": Uuid::new_v4().to_string(),
                    "title": "parent-task",
                    "description": null,
                    "status": "todo",
                    "parent_workspace_id": unrelated_grand_parent.to_string(),
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                }
            }));
        });
        let start_guard = mock.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/tasks/start");
            then.status(500);
        });

        let server = McpServer::new_workspace(&mock.base_url()).with_scope_for_test(scope);
        let req = McpCreateAndStartTaskRequest {
            project_id: Some(project_id),
            title: "child task".to_string(),
            description: None,
            parent_workspace_id: Some(unrelated_parent),
            workspace_name: Some("child-ws".to_string()),
            repositories: vec![McpWorkspaceRepoInput {
                repo_id,
                branch: "main".to_string(),
            }],
            executor: "CODEX".to_string(),
            variant: None,
            prompt: "go".to_string(),
        };
        let result = server.create_and_start_task(Parameters(req)).await.unwrap();

        let body = error_json(&result);
        assert_eq!(
            body.get("error_kind").and_then(|v| v.as_str()),
            Some("scope_denied"),
            "expected scope_denied: {body}"
        );
        assert_eq!(
            start_guard.hits(),
            0,
            "POST /api/tasks/start must not fire when parent is out of scope",
        );
    }
}
