use api_types::Tag;
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
    #[schemars(
        description = "Project to create the tag in. Optional when workspace context provides it."
    )]
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
        Parameters(CreateTagArgs {
            project_id,
            name,
            color,
        }): Parameters<CreateTagArgs>,
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

        let color = color.unwrap_or_else(|| DEFAULT_TAG_COLOR.to_string());
        let payload = serde_json::json!({
            "project_id": project_id,
            "name": name,
            "color": color,
        });

        let url = self.url("/api/v1/tags");
        let tag = match self
            .send_mutation_response::<Tag>(self.client.post(&url).json(&payload))
            .await
        {
            Ok(t) => t,
            Err(e) => return Ok(Self::tool_error(e)),
        };
        McpServer::success(&TagSummary {
            id: tag.id.to_string(),
            project_id: tag.project_id.to_string(),
            name: tag.name,
            color: tag.color,
        })
    }

    #[tool(
        description = "Update a tag's name and/or color. At least one field must be provided. Fails if the tag belongs to a different project than the caller's workspace context."
    )]
    async fn update_tag(
        &self,
        Parameters(UpdateTagArgs {
            tag_id,
            name,
            color,
        }): Parameters<UpdateTagArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if name.is_none() && color.is_none() {
            return Ok(Self::tool_error(ToolError::message(
                "No fields to update (provide at least one of `name`, `color`)",
            )));
        }
        if let Err(e) = self.require_tag_in_scope(tag_id, "update").await {
            return Ok(Self::tool_error(e));
        }
        // Only include fields the caller actually provided so the backend's
        // `#[serde(default, deserialize_with = "some_if_present")]` keeps
        // untouched fields at their current value.
        let mut payload = serde_json::Map::new();
        if let Some(name) = name {
            payload.insert("name".to_string(), serde_json::Value::String(name));
        }
        if let Some(color) = color {
            payload.insert("color".to_string(), serde_json::Value::String(color));
        }
        let payload = serde_json::Value::Object(payload);
        let url = self.url(&format!("/api/v1/tags/{}", tag_id));
        let tag = match self
            .send_mutation_response::<Tag>(self.client.patch(&url).json(&payload))
            .await
        {
            Ok(t) => t,
            Err(e) => return Ok(Self::tool_error(e)),
        };
        McpServer::success(&TagSummary {
            id: tag.id.to_string(),
            project_id: tag.project_id.to_string(),
            name: tag.name,
            color: tag.color,
        })
    }

    #[tool(
        description = "Delete a tag. When issue_tags reference this tag, rejects with a 409 envelope whose error_data = {message, issue_tag_count} lists how many references block the delete; pass force=true to cascade-delete the relation rows. Future server releases may also set error_kind on that envelope. Fails if the tag belongs to a different project than the caller's workspace context."
    )]
    async fn delete_tag(
        &self,
        Parameters(DeleteTagArgs { tag_id, force }): Parameters<DeleteTagArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(e) = self.require_tag_in_scope(tag_id, "delete").await {
            return Ok(Self::tool_error(e));
        }
        let url = self.url(&format!("/api/v1/tags/{}", tag_id));
        let mut req = self.client.delete(&url);
        if force.unwrap_or(false) {
            req = req.query(&[("force", "true")]);
        }
        if let Err(e) = self.send_delete_raw(req).await {
            return Ok(Self::tool_error(e));
        }
        McpServer::success(&DeleteTagResponse {
            success: true,
            tag_id: tag_id.to_string(),
        })
    }
}

impl McpServer {
    /// Cross-project scope guard for `update_tag` / `delete_tag`.
    ///
    /// Mirrors the guard `create_tag` applies up-front (lines 138-150):
    /// when the caller's workspace context pins a `project_id`, refuse
    /// to mutate a tag owned by a different project. Because these
    /// tools only receive a `tag_id`, we have to resolve the owning
    /// project first via the read-only `GET /api/remote/tags/{id}`
    /// endpoint. `Ok(())` means "safe to proceed" (no context, no
    /// scoped project, or the projects match); `Err` short-circuits
    /// the caller with a scope-denied `ToolError`.
    async fn require_tag_in_scope(&self, tag_id: Uuid, verb: &str) -> Result<(), ToolError> {
        let Some(ctx) = &self.context else {
            return Ok(());
        };
        let Some(scoped_project) = ctx.project_id else {
            return Ok(());
        };
        let url = self.url(&format!("/api/remote/tags/{}", tag_id));
        let tag: Tag = self.send_json(self.client.get(&url)).await?;
        if tag.project_id != scoped_project {
            return Err(ToolError::new(
                format!("Cannot {verb} tag outside the current workspace's project",),
                Some(format!(
                    "tag project_id={}, scoped project_id={}",
                    tag.project_id, scoped_project
                )),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use httpmock::MockServer;

    use super::*;

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

    /// `ApiResponse<Tag>` envelope — the shape `GET /api/remote/tags/{id}`
    /// returns, which `require_tag_in_scope` depends on.
    fn tag_read_envelope(id: Uuid, project_id: Uuid, name: &str, color: &str) -> serde_json::Value {
        serde_json::json!({
            "success": true,
            "data": {
                "id": id.to_string(),
                "project_id": project_id.to_string(),
                "name": name,
                "color": color,
            },
            "message": null
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
                .json_body_partial(serde_json::json!({ "color": DEFAULT_TAG_COLOR }).to_string());
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
                .json_body_partial(serde_json::json!({ "color": "#FF0000" }).to_string());
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

        let mut server =
            McpServer::new_workspace(&mock_server.base_url()).with_scope_for_test(Uuid::new_v4());
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
    async fn update_tag_rejects_cross_project_when_scoped() {
        install_rustls();
        let mock_server = MockServer::start();
        let tid = Uuid::new_v4();
        let scoped_project = Uuid::new_v4();
        let foreign_project = Uuid::new_v4();

        // GET /api/remote/tags/{id} — the scope guard's lookup. Returns a
        // tag whose project_id differs from the caller's scoped project.
        let lookup = mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/remote/tags/{tid}"));
            then.status(200).json_body(tag_read_envelope(
                tid,
                foreign_project,
                "foreign",
                "#ABCDEF",
            ));
        });
        // PATCH /api/v1/tags/{id} — the mutation endpoint. Must NOT be
        // called; guard short-circuits before this. Rigged to 500 so a
        // leaked call would flip the test red loudly.
        let mutation = mock_server.mock(|when, then| {
            when.method(httpmock::Method::PATCH)
                .path(format!("/api/v1/tags/{tid}"));
            then.status(500);
        });

        let mut server =
            McpServer::new_workspace(&mock_server.base_url()).with_scope_for_test(Uuid::new_v4());
        if let Some(ctx) = server.context.as_mut() {
            ctx.project_id = Some(scoped_project);
        }

        let req = UpdateTagArgs {
            tag_id: tid,
            name: Some("new-name".to_string()),
            color: None,
        };
        let result = server
            .update_tag(Parameters(req))
            .await
            .expect("must return error tool result");
        assert!(result.is_error.unwrap_or(false));
        lookup.assert_hits(1);
        assert_eq!(mutation.hits(), 0);
    }

    #[tokio::test]
    async fn delete_tag_rejects_in_use_without_force() {
        install_rustls();
        let mock_server = MockServer::start();
        let tid = Uuid::new_v4();
        mock_server.mock(|when, then| {
            when.method(httpmock::Method::DELETE)
                .path(format!("/api/v1/tags/{tid}"))
                .matches(|r| {
                    !r.query_params
                        .as_ref()
                        .map(|p| p.iter().any(|(k, _)| k == "force"))
                        .unwrap_or(false)
                });
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
            .delete_tag(Parameters(DeleteTagArgs {
                tag_id: tid,
                force: None,
            }))
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
            // Real backend returns raw `{"txid": 0}` on success (see
            // DeleteResponse + mutation_response in crates/server/src/routes/v1.rs).
            then.status(200).json_body(serde_json::json!({
                "txid": 0
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

    #[tokio::test]
    async fn delete_tag_rejects_cross_project_when_scoped() {
        install_rustls();
        let mock_server = MockServer::start();
        let tid = Uuid::new_v4();
        let scoped_project = Uuid::new_v4();
        let foreign_project = Uuid::new_v4();

        // GET /api/remote/tags/{id} — scope guard lookup. Tag belongs to a
        // different project than the caller's workspace scope.
        let lookup = mock_server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/remote/tags/{tid}"));
            then.status(200).json_body(tag_read_envelope(
                tid,
                foreign_project,
                "foreign",
                "#ABCDEF",
            ));
        });
        // DELETE /api/v1/tags/{id} — must NOT be called; guard short-
        // circuits before the mutation. Rigged to 500 so a leaked call
        // fails loudly.
        let mutation = mock_server.mock(|when, then| {
            when.method(httpmock::Method::DELETE)
                .path(format!("/api/v1/tags/{tid}"));
            then.status(500);
        });

        let mut server =
            McpServer::new_workspace(&mock_server.base_url()).with_scope_for_test(Uuid::new_v4());
        if let Some(ctx) = server.context.as_mut() {
            ctx.project_id = Some(scoped_project);
        }

        let result = server
            .delete_tag(Parameters(DeleteTagArgs {
                tag_id: tid,
                force: None,
            }))
            .await
            .expect("must return error tool result");
        assert!(result.is_error.unwrap_or(false));
        lookup.assert_hits(1);
        assert_eq!(mutation.hits(), 0);
    }
}
