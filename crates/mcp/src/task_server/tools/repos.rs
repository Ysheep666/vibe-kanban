use db::models::repo::Repo;
use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::McpServer;

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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetRepoRequest {
    #[schemars(description = "The ID of the repository to retrieve")]
    repo_id: Uuid,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RepoDetails {
    #[schemars(description = "The unique identifier of the repository")]
    id: String,
    #[schemars(description = "The name of the repository")]
    name: String,
    #[schemars(description = "The display name of the repository")]
    display_name: String,
    #[schemars(description = "The setup script that runs when initializing a workspace")]
    setup_script: Option<String>,
    #[schemars(description = "The cleanup script that runs when tearing down a workspace")]
    cleanup_script: Option<String>,
    #[schemars(description = "The dev server script that starts the development server")]
    dev_server_script: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateSetupScriptRequest {
    #[schemars(description = "The ID of the repository to update")]
    repo_id: Uuid,
    #[schemars(description = "The new setup script content (use empty string to clear)")]
    script: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateCleanupScriptRequest {
    #[schemars(description = "The ID of the repository to update")]
    repo_id: Uuid,
    #[schemars(description = "The new cleanup script content (use empty string to clear)")]
    script: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateDevServerScriptRequest {
    #[schemars(description = "The ID of the repository to update")]
    repo_id: Uuid,
    #[schemars(description = "The new dev server script content (use empty string to clear)")]
    script: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct UpdateRepoScriptResponse {
    #[schemars(description = "Whether the update was successful")]
    success: bool,
    #[schemars(description = "The repository ID that was updated")]
    repo_id: String,
    #[schemars(description = "The script field that was updated")]
    field: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ListReposResponse {
    repos: Vec<McpRepoSummary>,
    count: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct AddRepoRequest {
    #[schemars(
        description = "Absolute filesystem path to an existing git repository on this machine."
    )]
    pub path: String,
    #[schemars(description = "Optional human-readable display name. Defaults to the folder name.")]
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

#[tool_router(router = repos_tools_router, vis = "pub")]
impl McpServer {
    #[tool(description = "List all repositories.")]
    async fn list_repos(&self) -> Result<CallToolResult, ErrorData> {
        let url = self.url("/api/repos");
        let repos: Vec<Repo> = match self.send_json(self.client.get(&url)).await {
            Ok(rs) => rs,
            Err(e) => return Ok(Self::tool_error(e)),
        };

        let repo_summaries: Vec<McpRepoSummary> = repos
            .into_iter()
            .map(|r| McpRepoSummary {
                id: r.id.to_string(),
                name: r.name,
                display_name: r.display_name,
                path: r.path.to_string_lossy().into_owned(),
            })
            .collect();

        let response = ListReposResponse {
            count: repo_summaries.len(),
            repos: repo_summaries,
        };

        McpServer::success(&response)
    }

    #[tool(
        description = "Get detailed information about a repository including its scripts. Use `list_repos` to find available repo IDs."
    )]
    async fn get_repo(
        &self,
        Parameters(GetRepoRequest { repo_id }): Parameters<GetRepoRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/repos/{}", repo_id));
        let repo: Repo = match self.send_json(self.client.get(&url)).await {
            Ok(r) => r,
            Err(e) => return Ok(Self::tool_error(e)),
        };
        McpServer::success(&RepoDetails {
            id: repo.id.to_string(),
            name: repo.name,
            display_name: repo.display_name,
            setup_script: repo.setup_script,
            cleanup_script: repo.cleanup_script,
            dev_server_script: repo.dev_server_script,
        })
    }

    #[tool(
        description = "Update a repository's setup script. The setup script runs when initializing a workspace."
    )]
    async fn update_setup_script(
        &self,
        Parameters(UpdateSetupScriptRequest { repo_id, script }): Parameters<
            UpdateSetupScriptRequest,
        >,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/repos/{}", repo_id));
        let script_value = if script.is_empty() {
            None
        } else {
            Some(script)
        };
        let payload = serde_json::json!({
            "setup_script": script_value
        });
        let _repo: Repo = match self.send_json(self.client.put(&url).json(&payload)).await {
            Ok(r) => r,
            Err(e) => return Ok(Self::tool_error(e)),
        };
        McpServer::success(&UpdateRepoScriptResponse {
            success: true,
            repo_id: repo_id.to_string(),
            field: "setup_script".to_string(),
        })
    }

    #[tool(
        description = "Update a repository's cleanup script. The cleanup script runs when tearing down a workspace."
    )]
    async fn update_cleanup_script(
        &self,
        Parameters(UpdateCleanupScriptRequest { repo_id, script }): Parameters<
            UpdateCleanupScriptRequest,
        >,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/repos/{}", repo_id));
        let script_value = if script.is_empty() {
            None
        } else {
            Some(script)
        };
        let payload = serde_json::json!({
            "cleanup_script": script_value
        });
        let _repo: Repo = match self.send_json(self.client.put(&url).json(&payload)).await {
            Ok(r) => r,
            Err(e) => return Ok(Self::tool_error(e)),
        };
        McpServer::success(&UpdateRepoScriptResponse {
            success: true,
            repo_id: repo_id.to_string(),
            field: "cleanup_script".to_string(),
        })
    }

    #[tool(
        description = "Update a repository's dev server script. The dev server script starts the development server for the repository."
    )]
    async fn update_dev_server_script(
        &self,
        Parameters(UpdateDevServerScriptRequest { repo_id, script }): Parameters<
            UpdateDevServerScriptRequest,
        >,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/repos/{}", repo_id));
        let script_value = if script.is_empty() {
            None
        } else {
            Some(script)
        };
        let payload = serde_json::json!({
            "dev_server_script": script_value
        });
        let _repo: Repo = match self.send_json(self.client.put(&url).json(&payload)).await {
            Ok(r) => r,
            Err(e) => return Ok(Self::tool_error(e)),
        };
        McpServer::success(&UpdateRepoScriptResponse {
            success: true,
            repo_id: repo_id.to_string(),
            field: "dev_server_script".to_string(),
        })
    }

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
}

#[cfg(test)]
mod tests {
    use httpmock::MockServer;

    use super::*;
    use crate::task_server::McpServer;

    fn install_rustls() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

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
                    "archive_script": null,
                    "copy_files": null,
                    "parallel_setup_script": false,
                    "dev_server_script": null,
                    "default_target_branch": null,
                    "default_working_dir": null,
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
        let req = DeleteRepoRequest {
            repo_id: rid,
            force: None,
        };
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
        let req = DeleteRepoRequest {
            repo_id: rid,
            force: Some(true),
        };
        let result = server
            .delete_repo(rmcp::handler::server::wrapper::Parameters(req))
            .await
            .expect("must succeed");
        assert!(!result.is_error.unwrap_or(false));
        mock.assert_hits(1);
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
                    "archive_script": null,
                    "copy_files": null,
                    "parallel_setup_script": false,
                    "dev_server_script": null,
                    "default_target_branch": null,
                    "default_working_dir": null,
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
