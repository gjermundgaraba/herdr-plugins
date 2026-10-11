//! What a key says about the agent in its slot.

use std::path::Path;

use herdr_client::ClientReportAgent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLabel {
    pub title: String,
    pub subtitle: String,
}

pub fn agent_label(row: &ClientReportAgent) -> AgentLabel {
    let agent = &row.agent;
    let title = agent
        .name
        .as_ref()
        .or(agent.display_agent.as_ref())
        .or(agent.agent.as_ref())
        .cloned()
        .unwrap_or_else(|| "agent".into());
    let repo = row
        .foreground_cwd
        .as_ref()
        .or(row.cwd.as_ref())
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned());
    let subtitle = Some(row.workspace_label.clone())
        .filter(|label| !label.is_empty())
        .or(repo)
        .or_else(|| agent.terminal_title_stripped.clone())
        .or_else(|| agent.terminal_title.clone())
        .unwrap_or_default();
    AgentLabel { title, subtitle }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::tests::fixture_report;

    #[test]
    fn labels_follow_name_and_location_fallbacks() {
        let mut row = fixture_report().agents[0].clone();
        assert_eq!(
            agent_label(&row),
            AgentLabel {
                title: "Claude".into(),
                subtitle: "api".into(),
            }
        );

        row.agent.name = Some("builder".into());
        assert_eq!(agent_label(&row).title, "builder");
        row.agent.name = None;
        row.agent.display_agent = None;
        assert_eq!(agent_label(&row).title, "claude");
        row.agent.agent = None;
        assert_eq!(agent_label(&row).title, "agent");

        // Without a space label the directory names the agent's place.
        row.workspace_label.clear();
        assert_eq!(agent_label(&row).subtitle, "api");
        row.foreground_cwd = Some("/Users/gg/ws/pers/herdr-deck".into());
        assert_eq!(agent_label(&row).subtitle, "herdr-deck");
        row.cwd = None;
        row.foreground_cwd = None;
        assert_eq!(agent_label(&row).subtitle, "Claude Review");
        row.agent.terminal_title_stripped = None;
        row.agent.terminal_title = None;
        assert_eq!(agent_label(&row).subtitle, "");
    }
}
