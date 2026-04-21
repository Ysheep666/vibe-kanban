//! Thin wrapper over reqwest::Client for MCP → server HTTP calls.
//! Centralises envelope decoding for the handful of routes MCP consumes today.

use api_types::{MutationResponse, Tag};
use db::models::{repo::Repo, task::Task, workspace::Workspace};
use reqwest::Client;
use utils::response::ApiResponse;
use uuid::Uuid;

#[derive(thiserror::Error, Debug)]
pub enum ApiClientError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("server error: {0}")]
    Server(String),
    #[error("invalid response shape")]
    BadShape,
}

pub type ApiResult<T> = Result<T, ApiClientError>;

#[derive(Debug, Clone)]
pub struct ApiClient {
    client: Client,
    base_url: String,
}

impl ApiClient {
    pub fn new(client: Client, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into(),
        }
    }

    pub async fn get_workspace(&self, id: Uuid) -> ApiResult<Workspace> {
        self.get_json(&format!("/api/workspaces/{id}")).await
    }

    pub async fn get_task(&self, id: Uuid) -> ApiResult<Task> {
        self.get_json(&format!("/api/tasks/{id}")).await
    }

    /// Register a repo via `POST /api/repos`.
    ///
    /// NOTE: The MCP `add_repo` tool does NOT use this helper. The tool issues
    /// the raw HTTP call via `send_json` so the server's full error envelope —
    /// including any `error_kind` the server starts populating — reaches the
    /// AI caller untouched. This helper is for non-MCP consumers (e.g. future
    /// background maintenance code) that are happy to collapse non-success
    /// responses into `ApiClientError::Server(message)`.
    pub async fn register_repo(&self, path: &str, display_name: Option<&str>) -> ApiResult<Repo> {
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

    /// Delete a repo via `DELETE /api/repos/:id[?force=true]`.
    ///
    /// NOTE: The MCP `delete_repo` tool does NOT use this helper. The tool
    /// issues the raw HTTP call via `send_empty_json` so the 409 conflict
    /// body — specifically `error_data.workspaces: Vec<String>` that lists
    /// the active workspaces blocking the delete — flows through to the AI
    /// caller intact. This helper deliberately collapses any non-success
    /// response into `ApiClientError::Server(message)`, dropping `error_data`.
    /// Use it only when you don't need the structured conflict payload.
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

    /// Create a tag via `POST /api/v1/tags`.
    ///
    /// NOTE: The MCP `create_tag` tool does NOT use this helper. The tool
    /// issues the raw HTTP call via `send_mutation_response` so the server's
    /// full error envelope — including any `error_kind` the server starts
    /// populating — reaches the AI caller untouched. This helper is for
    /// non-MCP consumers that are happy to collapse non-success responses
    /// into `ApiClientError::Server(message)`.
    pub async fn create_tag(&self, project_id: Uuid, name: &str, color: &str) -> ApiResult<Tag> {
        let url = format!("{}/api/v1/tags", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "name": name,
            "color": color,
        });
        let resp = self.client.post(url).json(&body).send().await?;
        let envelope: MutationResponse<Tag> = resp.json().await?;
        Ok(envelope.data)
    }

    /// Update a tag via `PATCH /api/v1/tags/:id`.
    ///
    /// NOTE: The MCP `update_tag` tool does NOT use this helper. The tool
    /// issues the raw HTTP call via `send_mutation_response` so the server's
    /// full error envelope flows to the AI caller unchanged. This helper is
    /// for non-MCP consumers that are happy to collapse non-success responses
    /// into `ApiClientError::Server(message)`.
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
        let envelope: MutationResponse<Tag> = resp.json().await?;
        Ok(envelope.data)
    }

    /// Delete a tag via `DELETE /api/v1/tags/:id[?force=true]`.
    ///
    /// NOTE: The MCP `delete_tag` tool does NOT use this helper. The tool
    /// issues the raw HTTP call via `send_delete_raw` so the 409 conflict
    /// body — specifically `error_data.issue_tag_count: i64` that lists
    /// how many issue_tags block the delete — flows through to the AI
    /// caller intact. This helper deliberately uses `error_for_status()`
    /// and drops the structured conflict payload. Use it only when you
    /// don't need the reference-count information.
    pub async fn delete_tag(&self, tag_id: Uuid, force: bool) -> ApiResult<()> {
        let url = format!("{}/api/v1/tags/{tag_id}", self.base_url);
        let mut req = self.client.delete(url);
        if force {
            req = req.query(&[("force", "true")]);
        }
        let _resp = req.send().await?.error_for_status()?;
        Ok(())
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.get(url).send().await?;
        let envelope: ApiResponse<T> = resp.json().await?;
        if !envelope.is_success() {
            return Err(ApiClientError::Server(
                envelope.message().unwrap_or("").to_string(),
            ));
        }
        envelope.into_data().ok_or(ApiClientError::BadShape)
    }
}

#[cfg(test)]
mod api_client_tests {
    use super::*;

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
                .matches(|req| {
                    !req.query_params
                        .as_ref()
                        .map(|p| p.iter().any(|(k, _)| k == "force"))
                        .unwrap_or(false)
                });
            then.status(200).json_body(serde_json::json!({
                "success": true, "data": null
            }));
        });
        let client = ApiClient::new(reqwest::Client::new(), server.base_url());
        client.delete_repo(rid, false).await.expect("must succeed");
        mock.assert_hits(1);
    }

    #[tokio::test]
    async fn get_workspace_decodes_envelope() {
        let server = httpmock::MockServer::start();
        let wid = uuid::Uuid::new_v4();
        server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/workspaces/{wid}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": wid.to_string(),
                    "task_id": null,
                    "container_ref": null,
                    "branch": "main",
                    "setup_completed_at": null,
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                    "archived": false,
                    "pinned": false,
                    "name": null,
                    "worktree_deleted": false
                }
            }));
        });
        let client = ApiClient::new(reqwest::Client::new(), server.base_url());
        let ws = client.get_workspace(wid).await.unwrap();
        assert_eq!(ws.id, wid);
    }

    #[tokio::test]
    async fn get_task_decodes_envelope() {
        let server = httpmock::MockServer::start();
        let tid = uuid::Uuid::new_v4();
        let pid = uuid::Uuid::new_v4();
        server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/tasks/{tid}"));
            then.status(200).json_body(serde_json::json!({
                "success": true,
                "data": {
                    "id": tid.to_string(),
                    "project_id": pid.to_string(),
                    "title": "t",
                    "description": null,
                    "status": "todo",
                    "parent_workspace_id": null,
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z"
                }
            }));
        });
        let client = ApiClient::new(reqwest::Client::new(), server.base_url());
        let task = client.get_task(tid).await.unwrap();
        assert_eq!(task.id, tid);
        assert_eq!(task.project_id, pid);
        assert_eq!(task.title, "t");
    }

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
                    "color": "#6B7280"
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
}
