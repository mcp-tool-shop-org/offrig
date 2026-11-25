//! Static procedure-only MCP skill (`io.modelcontextprotocol/skills`, SEP-2640).
//!
//! Serves `skill://offrig/offrig-session/SKILL.md` with a pinned SHA-256 manifest
//! and no `resources: "dynamic"`. Policy rules (budget caps, lane ownership,
//! deadlines, and shutdown guards) stay in server code.

use rmcp::ErrorData as McpError;
use rmcp::model::{
    GetSkillResult, ListResourcesResult, ListSkillsResult, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, Skill, SkillFrontmatter, SkillResource,
    SkillResources,
};

pub const SKILL_NAME: &str = "offrig-session";
pub const SKILL_URI: &str = "skill://offrig/offrig-session/SKILL.md";
pub const SKILL_DESCRIPTION: &str =
    "Plan, launch, follow, run work on, and shut down rented GPU sessions through offrig-mcp.";
pub const SKILL_MD: &str = include_str!("../skills/offrig-session/SKILL.md");
pub const SKILL_DIGEST: &str =
    "sha256:56b2081b40c07beac9d1a76fee38511ef8016f41078db0fddc65ddabc75b54e0";
pub const SKILL_SIZE: u64 = 3311;

/// Construct the static skill entry for `skills/list` and `skills/get`.
pub fn offrig_session_skill() -> Skill {
    Skill::new(
        SKILL_URI,
        SkillFrontmatter::new(SKILL_NAME, SKILL_DESCRIPTION),
        SkillResources::Files(vec![SkillResource::new(
            SKILL_URI,
            SKILL_DIGEST,
            SKILL_SIZE,
        )]),
    )
}

pub fn list_skills() -> ListSkillsResult {
    ListSkillsResult::with_all_items(vec![offrig_session_skill()])
}

pub fn get_skill(uri: &str) -> Result<GetSkillResult, McpError> {
    if uri != SKILL_URI {
        return Err(McpError::invalid_params(
            format!("unknown skill: {uri}"),
            None,
        ));
    }
    Ok(GetSkillResult::new(offrig_session_skill()))
}

pub fn list_resources() -> ListResourcesResult {
    let mut resource = Resource::new(SKILL_URI, SKILL_NAME)
        .with_description(SKILL_DESCRIPTION)
        .with_mime_type("text/markdown")
        .with_size(SKILL_SIZE);
    let _ = &mut resource;
    ListResourcesResult::with_all_items(vec![resource])
}

pub fn read_resource(uri: &str) -> Result<ReadResourceResponse, McpError> {
    if uri != SKILL_URI {
        return Err(McpError::resource_not_found(
            format!("unknown resource: {uri}"),
            None,
        ));
    }
    let contents = ResourceContents::text(SKILL_MD, SKILL_URI).with_mime_type("text/markdown");
    Ok(ReadResourceResult::new(vec![contents]).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_manifest_matches_embedded_file_bytes() {
        let skill = offrig_session_skill();
        skill.validate().expect("skill manifest valid");
        let SkillResources::Files(files) = &skill.resources else {
            panic!("skill resources must be a static file manifest, never dynamic");
        };
        assert_eq!(files.len(), 1);
        files[0]
            .verify(SKILL_MD.as_bytes())
            .expect("pinned digest and size match SKILL.md");
    }
}
