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
