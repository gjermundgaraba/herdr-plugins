use std::collections::HashSet;

use nucleo_matcher::{
    Config, Matcher, Utf32String,
    pattern::{CaseMatching, Normalization, Pattern},
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Muted,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub detail: String,
    pub badge: String,
    pub indicator: String,
    pub tone: Option<Tone>,
    pub spinning: bool,
    pub search: String,
    pub value: Value,
}

pub fn validate_items(items: &[Item]) -> Result<(), String> {
    let mut ids = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        if item.id.trim().is_empty() {
            return Err(format!("items[{index}].id must not be empty"));
        }
        if item.title.trim().is_empty() {
            return Err(format!("items[{index}].title must not be empty"));
        }
        if !ids.insert(&item.id) {
            return Err(format!("duplicate item id {:?}", item.id));
        }
    }
    Ok(())
}

pub struct Picker {
    pub items: Vec<Item>,
    pub query: String,
    pub selected: usize,
    pub scroll: usize,
    pub visible_rows: usize,
    filtered: Vec<usize>,
    haystacks: Vec<Utf32String>,
    matcher: Matcher,
    local_search: bool,
}

impl Picker {
    pub fn new(items: Vec<Item>, local_search: bool) -> Self {
        let haystacks = items.iter().map(haystack).collect();
        let mut picker = Self {
            items,
            query: String::new(),
            selected: 0,
            scroll: 0,
            visible_rows: 0,
            filtered: Vec::new(),
            haystacks,
            matcher: Matcher::new(Config::DEFAULT),
            local_search,
        };
        picker.refilter();
        picker
    }

    pub fn len(&self) -> usize {
        self.filtered.len()
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    pub fn row(&self, index: usize) -> Option<&Item> {
        self.items.get(*self.filtered.get(index)?)
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.row(self.selected)
    }

    pub fn needs_spinner(&self) -> bool {
        let end = self.len().min(self.scroll + self.visible_rows);
        (self.scroll..end).any(|index| self.row(index).is_some_and(|item| item.spinning))
    }

    pub fn refilter(&mut self) {
        let query = self.query.trim();
        let mut matches = Vec::new();

        if query.is_empty() || !self.local_search {
            matches.extend((0..self.items.len()).map(|index| (index, 0)));
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            for index in 0..self.items.len() {
                if let Some(score) =
                    pattern.score(self.haystacks[index].slice(..), &mut self.matcher)
                {
                    matches.push((index, score));
                }
            }
            matches.sort_unstable_by(|(left_index, left_score), (right_index, right_score)| {
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

    pub fn replace_items(&mut self, items: Vec<Item>) {
        let selected_id = self.selected_item().map(|item| item.id.clone());
        self.items = items;
        self.rebuild(selected_id.as_deref());
    }

    pub fn clear_items(&mut self) {
        self.items.clear();
        self.rebuild(None);
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

    fn rebuild(&mut self, selected_id: Option<&str>) {
        self.haystacks = self.items.iter().map(haystack).collect();
        self.refilter();
        if let Some(selected_id) = selected_id
            && let Some(index) = self
                .filtered
                .iter()
                .position(|index| self.items[*index].id == selected_id)
        {
            self.selected = index;
            self.ensure_selection_visible();
        }
    }
}

fn haystack(item: &Item) -> Utf32String {
    format!(
        "{} {} {} {} {} {}",
        item.id, item.title, item.subtitle, item.detail, item.badge, item.search
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str, title: &str, search: &str) -> Item {
        Item {
            id: id.into(),
            title: title.into(),
            search: search.into(),
            value: json!({ "original": id }),
            ..Item::default()
        }
    }

    #[test]
    fn fuzzy_search_uses_display_and_explicit_search_text() {
        let mut picker = Picker::new(
            vec![
                item("logs", "Split logs", ""),
                item("api", "Backend", "production service"),
                item("agent", "Claude API fixer", ""),
            ],
            true,
        );
        picker.query = "api fix".into();
        picker.refilter();
        assert_eq!(picker.row(0).unwrap().id, "agent");

        picker.query = "production".into();
        picker.refilter();
        assert_eq!(picker.row(0).unwrap().id, "api");
    }

    #[test]
    fn validates_item_identity() {
        assert!(validate_items(&[item("one", "One", "")]).is_ok());
        assert!(validate_items(&[]).is_ok());
        assert!(
            validate_items(&[item("same", "One", ""), item("same", "Two", "")])
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn provider_order_and_selection_survive_replacement() {
        let mut picker = Picker::new(vec![item("one", "One", ""), item("two", "Two", "")], false);
        picker.query = "not locally filtered".into();
        picker.refilter();
        picker.selected = 1;

        picker.replace_items(vec![item("two", "Updated", ""), item("three", "Three", "")]);
        assert_eq!(picker.len(), 2);
        assert_eq!(picker.selected_item().unwrap().id, "two");
    }
}
