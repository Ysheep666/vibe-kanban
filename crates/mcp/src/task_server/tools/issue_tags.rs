use api_types::{CreateIssueTagRequest, IssueTag, ListIssueTagsResponse, MutationResponse};
use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::McpServer;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpListIssueTagsRequest {
    #[schemars(description = "Issue ID to list tags for")]
    issue_id: Uuid,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct IssueTagSummary {
    #[schemars(description = "Issue-tag relation ID")]
    id: String,
    #[schemars(description = "Issue ID")]
    issue_id: String,
    #[schemars(description = "Tag ID")]
    tag_id: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct McpListIssueTagsResponse {
    issue_id: String,
    issue_tags: Vec<IssueTagSummary>,
    count: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpAddIssueTagRequest {
    #[schemars(description = "Issue ID to attach the tag to")]
    issue_id: Uuid,
    #[schemars(description = "Tag ID to attach")]
    tag_id: Uuid,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct McpAddIssueTagResponse {
    issue_tag_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct McpRemoveIssueTagRequest {
    #[schemars(description = "Issue-tag relation ID to remove")]
    issue_tag_id: Uuid,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct McpRemoveIssueTagResponse {
    success: bool,
    issue_tag_id: String,
}

#[tool_router(router = issue_tags_tools_router, vis = "pub")]
impl McpServer {
    #[tool(description = "List tags attached to an issue.")]
    async fn list_issue_tags(
        &self,
        Parameters(McpListIssueTagsRequest { issue_id }): Parameters<McpListIssueTagsRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/remote/issue-tags?issue_id={}", issue_id));
        let response: ListIssueTagsResponse = match self.send_json(self.client.get(&url)).await {
            Ok(r) => r,
            Err(e) => return Ok(Self::tool_error(e)),
        };

        let issue_tags = response
            .issue_tags
            .into_iter()
            .map(|issue_tag| IssueTagSummary {
                id: issue_tag.id.to_string(),
                issue_id: issue_tag.issue_id.to_string(),
                tag_id: issue_tag.tag_id.to_string(),
            })
            .collect::<Vec<_>>();

        McpServer::success(&McpListIssueTagsResponse {
            issue_id: issue_id.to_string(),
            count: issue_tags.len(),
            issue_tags,
        })
    }

    #[tool(description = "Attach a tag to an issue.")]
    async fn add_issue_tag(
        &self,
        Parameters(McpAddIssueTagRequest { issue_id, tag_id }): Parameters<McpAddIssueTagRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let payload = CreateIssueTagRequest {
            id: None,
            issue_id,
            tag_id,
        };

        let url = self.url("/api/remote/issue-tags");
        let response: MutationResponse<IssueTag> =
            match self.send_json(self.client.post(&url).json(&payload)).await {
                Ok(r) => r,
                Err(e) => return Ok(Self::tool_error(e)),
            };

        McpServer::success(&McpAddIssueTagResponse {
            issue_tag_id: response.data.id.to_string(),
        })
    }

    #[tool(description = "Remove a tag from an issue using issue_tag_id.")]
    async fn remove_issue_tag(
        &self,
        Parameters(McpRemoveIssueTagRequest { issue_tag_id }): Parameters<McpRemoveIssueTagRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = self.url(&format!("/api/remote/issue-tags/{}", issue_tag_id));
        if let Err(e) = self.send_empty_json(self.client.delete(&url)).await {
            return Ok(Self::tool_error(e));
        }

        McpServer::success(&McpRemoveIssueTagResponse {
            success: true,
            issue_tag_id: issue_tag_id.to_string(),
        })
    }
}
