use herdr_client::AgentStatus;
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Actions,
    Workspaces,
    Tabs,
    Panes,
    Agents,
}

impl Filter {
    pub const ALL: [Self; 6] = [
        Self::All,
        Self::Actions,
        Self::Workspaces,
        Self::Tabs,
        Self::Panes,
        Self::Agents,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Actions => "actions",
            Self::Workspaces => "workspaces",
            Self::Tabs => "tabs",
            Self::Panes => "panes",
            Self::Agents => "agents",
        }
    }

    pub fn next(self, delta: isize) -> Self {
        let index = Self::ALL.iter().position(|item| *item == self).unwrap_or(0);
        let len = Self::ALL.len() as isize;
        Self::ALL[(index as isize + delta).rem_euclid(len) as usize]
    }
}

impl std::str::FromStr for Filter {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "all" => Ok(Self::All),
            "actions" => Ok(Self::Actions),
            "workspaces" => Ok(Self::Workspaces),
            "tabs" => Ok(Self::Tabs),
            "panes" => Ok(Self::Panes),
            "agents" => Ok(Self::Agents),
            _ => Err("expected all, actions, workspaces, tabs, panes, or agents"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NativeAction,
    PluginAction,
    Workspace,
    Tab,
    Pane,
    Agent,
}

impl Kind {
    pub const fn filter(self) -> Filter {
        match self {
            Self::NativeAction | Self::PluginAction => Filter::Actions,
            Self::Workspace => Filter::Workspaces,
            Self::Tab => Filter::Tabs,
            Self::Pane => Filter::Panes,
            Self::Agent => Filter::Agents,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::NativeAction => "native",
            Self::PluginAction => "plugin",
            Self::Workspace => "workspace",
            Self::Tab => "tab",
            Self::Pane => "pane",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Dispatch {
    pub method: String,
    pub params: Value,
}

impl Dispatch {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub kind: Kind,
    pub agent_status: Option<AgentStatus>,
    pub title: String,
    pub subtitle: String,
    pub detail: String,
    pub keys: Vec<String>,
    pub dispatch: Dispatch,
}

pub struct Picker {
    pub items: Vec<Item>,
    pub query: String,
    pub filter: Filter,
    pub selected: usize,
    pub scroll: usize,
    pub visible_rows: usize,
    filtered: Vec<usize>,
}

impl Picker {
    pub fn new(items: Vec<Item>, filter: Filter) -> Self {
        let mut picker = Self {
            items,
            query: String::new(),
            filter,
            selected: 0,
            scroll: 0,
            visible_rows: 0,
            filtered: Vec::new(),
        };
        picker.refilter();
        picker
    }

    pub fn rows(&self) -> Vec<&Item> {
        self.filtered
            .iter()
            .filter_map(|index| self.items.get(*index))
            .collect()
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.filtered
            .get(self.selected)
            .and_then(|index| self.items.get(*index))
    }

    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.refilter();
    }

    pub fn cycle_filter(&mut self, delta: isize) {
        self.set_filter(self.filter.next(delta));
    }

    pub fn refilter(&mut self) {
        let query = self.query.trim();
        let mut matches = Vec::new();

        if query.is_empty() {
            matches.extend(
                self.items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| {
                        self.filter == Filter::All || item.kind.filter() == self.filter
                    })
                    .map(|(index, _)| (index, 0)),
            );
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut matcher = Matcher::new(Config::DEFAULT);
            for (index, item) in self.items.iter().enumerate() {
                if self.filter != Filter::All && item.kind.filter() != self.filter {
                    continue;
                }
                let haystack = format!(
                    "{} {} {} {} {}",
                    item.title,
                    item.subtitle,
                    item.detail,
                    item.kind.label(),
                    item.keys.join(" ")
                );
                let mut buf = Vec::new();
                if let Some(score) = pattern.score(Utf32Str::new(&haystack, &mut buf), &mut matcher)
                {
                    matches.push((index, score));
                }
            }
            matches.sort_by(|(left_index, left_score), (right_index, right_score)| {
                right_score.cmp(left_score).then_with(|| {
                    self.items[*left_index]
                        .title
                        .cmp(&self.items[*right_index].title)
                })
            });
        }

        self.filtered = matches.into_iter().map(|(index, _)| index).collect();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected =
            (self.selected as isize + delta).rem_euclid(self.filtered.len() as isize) as usize;
        self.ensure_selection_visible();
    }

    pub fn ensure_selection_visible(&mut self) {
        let rows = self.visible_rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(kind: Kind, title: &str) -> Item {
        Item {
            kind,
            agent_status: None,
            title: title.into(),
            subtitle: String::new(),
            detail: String::new(),
            keys: Vec::new(),
            dispatch: Dispatch::new("test", json!({})),
        }
    }

    #[test]
    fn fuzzy_search_and_source_filter_compose() {
        let mut picker = Picker::new(
            vec![
                item(Kind::NativeAction, "Split pane"),
                item(Kind::Workspace, "api"),
                item(Kind::Agent, "Claude API fixer"),
            ],
            Filter::All,
        );
        picker.query = "api fix".into();
        picker.refilter();
        assert_eq!(picker.rows()[0].title, "Claude API fixer");

        picker.set_filter(Filter::Workspaces);
        assert!(picker.rows().is_empty());
        picker.query = "api".into();
        picker.refilter();
        assert_eq!(picker.rows()[0].title, "api");
    }

    #[test]
    fn parses_filter_labels() {
        assert_eq!("workspaces".parse(), Ok(Filter::Workspaces));
        assert!("workspace".parse::<Filter>().is_err());
    }
}
