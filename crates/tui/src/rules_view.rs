//! `/rules` (`docs/tui.md`, "Swapped views"): every standing rule by scope,
//! global and then this project, each with the prefix it allows, when and
//! from which session it was added, and a ✕ that revokes it.

use contract::rules::RuleDecision;

use crate::configure::{Revoked, RuleRow, RulesScope, RulesSection};
use crate::keys::{Edit, Key};
use crate::local_time::utc_minute;
use crate::settings_view::{Act, Ctx};
use crate::swapped::{Frame, Ink, List, Spot, rows_height};

/// One row of the view: a section's heading, one of its rules, or the
/// note in place of its rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    /// A section's heading.
    Heading(RulesScope),
    /// One rule: its section and its index in that section's rows.
    Rule(RulesScope, usize),
    /// The note in place of a section's rules.
    Note(RulesScope),
}

impl Item {
    /// The section the row belongs to.
    fn scope(self) -> RulesScope {
        match self {
            Item::Heading(scope) | Item::Rule(scope, _) | Item::Note(scope) => scope,
        }
    }
}

/// The `/rules` view's state.
#[derive(Debug)]
pub(crate) struct Rules {
    sections: Option<(RulesSection, RulesSection)>,
    items: Vec<Item>,
    list: List,
    /// What the last action said, shown below the rows.
    said: Vec<String>,
}

impl Rules {
    /// The view over `ctx`'s workspace, its rows read.
    pub(crate) fn open(ctx: &Ctx<'_>) -> Self {
        let mut rules = Self {
            sections: None,
            items: Vec::new(),
            list: List::default(),
            said: Vec::new(),
        };
        rules.reread(ctx);
        rules
    }

    /// Reads the rows again, keeping the selection where it was; a failed
    /// read says why.
    pub(crate) fn reread(&mut self, ctx: &Ctx<'_>) {
        match ctx.seam.rules(ctx.workspace) {
            Ok(sections) => self.sections = Some(sections),
            Err(error) => {
                self.sections = None;
                self.said = vec![error.message];
            }
        }
        self.items = build(self.sections.as_ref());
        let shown = self.shown(ctx);
        self.list
            .select(self.list.selected(), self.items.len(), shown);
    }

    /// The rows the view shows at `ctx`'s height.
    fn shown(&self, ctx: &Ctx<'_>) -> usize {
        rows_height(&self.frame(), ctx.height)
    }

    /// The selected row.
    fn selected(&self) -> Option<Item> {
        self.items.get(self.list.selected()).copied()
    }

    /// `scope`'s section.
    fn section(&self, scope: RulesScope) -> Option<&RulesSection> {
        self.sections.as_ref().map(|sections| pick(sections, scope))
    }

    /// Handles one key.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            return Act::Close;
        }
        if *key == Key::CtrlG {
            return self.open_file();
        }
        if *key == Key::Backspace {
            return self.revoke_selected(ctx);
        }
        let shown = self.shown(ctx);
        if self.list.key(key, self.items.len(), shown) {
            self.said.clear();
        }
        Act::Stay
    }

    /// Handles one editing key: Delete revokes the selected rule.
    pub(crate) fn edit_key(&mut self, edit: &Edit, ctx: &Ctx<'_>) -> Act {
        if *edit == Edit::Delete {
            self.revoke_selected(ctx)
        } else {
            Act::Stay
        }
    }

    /// A click: the ✕ closes, a row is selected, a rule's ✕ revokes it.
    pub(crate) fn click(&mut self, spot: Spot, ctx: &Ctx<'_>) -> Act {
        let shown = self.shown(ctx);
        match spot {
            Spot::Close => Act::Close,
            Spot::Switch { .. } => Act::Stay,
            Spot::Row(at) => {
                self.list.select(at, self.items.len(), shown);
                self.said.clear();
                Act::Stay
            }
            Spot::Revoke(at) => {
                self.list.select(at, self.items.len(), shown);
                self.revoke_selected(ctx)
            }
            // Only the model picker draws cells with targets of their own.
            Spot::Cell(_, _) => Act::Stay,
        }
    }

    /// Opens the selected section's file; with the rules unread there is
    /// no section, so nothing opens.
    fn open_file(&self) -> Act {
        match self.selected().and_then(|item| self.section(item.scope())) {
            Some(section) => Act::Open(section.file.clone()),
            None => Act::Stay,
        }
    }

    /// Revokes the selected rule. On a heading or note row nothing
    /// happens.
    fn revoke_selected(&mut self, ctx: &Ctx<'_>) -> Act {
        let Some(Item::Rule(scope, index)) = self.selected() else {
            return Act::Stay;
        };
        let Some(row) = self
            .section(scope)
            .and_then(|section| section.rows.as_ref().ok())
            .and_then(|rows| rows.get(index))
        else {
            return Act::Stay;
        };
        let (line, text) = (row.line, row.text.clone());
        match ctx.seam.revoke(ctx.workspace, scope, line, &text) {
            Ok(Revoked::Removed) => {
                self.reread(ctx);
                self.said = vec!["Revoked; applies to the next call judged.".to_owned()];
            }
            Ok(Revoked::Stale) => {
                self.reread(ctx);
                self.said = vec!["The rules file changed; nothing was revoked.".to_owned()];
            }
            Err(error) => self.said = vec![error.message],
        }
        Act::Stay
    }

    /// The frame to draw.
    pub(crate) fn frame(&self) -> Frame {
        Frame {
            title: "Rules".to_owned(),
            rows: self
                .items
                .iter()
                .enumerate()
                .map(|(at, item)| self.line(*item, at))
                .collect(),
            list: self.list,
            below: self.said.clone(),
            field: None,
            footer: "↑↓ move · Delete revoke · Ctrl+G open the file · Esc close".to_owned(),
        }
    }

    /// One row's cells.
    fn line(&self, item: Item, at: usize) -> Vec<(String, Option<Spot>, Ink)> {
        match item {
            Item::Heading(scope) => {
                let name = match scope {
                    RulesScope::Global => "Global",
                    RulesScope::Project => "Project",
                };
                let file = self
                    .section(scope)
                    .map(|section| section.file.to_string_lossy().into_owned())
                    .unwrap_or_default();
                vec![(format!("{name} rules  {file}"), None, Ink::Heading)]
            }
            Item::Rule(_, _) => {
                let row = self.rule_row(item);
                match row {
                    Some(row) => vec![
                        ("✕ ".to_owned(), Some(Spot::Revoke(at)), Ink::Plain),
                        (fields(row), None, Ink::Plain),
                    ],
                    None => vec![("No rules.".to_owned(), None, Ink::Muted)],
                }
            }
            Item::Note(scope) => vec![(note(self.section(scope)), None, Ink::Muted)],
        }
    }

    /// The rule `item` names.
    fn rule_row(&self, item: Item) -> Option<&RuleRow> {
        let Item::Rule(scope, index) = item else {
            return None;
        };
        self.section(scope)
            .and_then(|section| section.rows.as_ref().ok())
            .and_then(|rows| rows.get(index))
    }
}

/// `scope`'s section: the only match on a rules scope, which `section`,
/// `open_file` and `build` share.
fn pick(sections: &(RulesSection, RulesSection), scope: RulesScope) -> &RulesSection {
    match scope {
        RulesScope::Global => &sections.0,
        RulesScope::Project => &sections.1,
    }
}

/// The view's rows for `sections`: each section's heading, then its rules
/// in line order, or one note in place of them. With the rules unread
/// there are no rows.
fn build(sections: Option<&(RulesSection, RulesSection)>) -> Vec<Item> {
    let Some(sections) = sections else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for scope in [RulesScope::Global, RulesScope::Project] {
        items.push(Item::Heading(scope));
        let section = pick(sections, scope);
        match section.rows.as_ref() {
            Ok(rows) if !rows.is_empty() => {
                for (index, _) in rows.iter().enumerate() {
                    items.push(Item::Rule(scope, index));
                }
            }
            _ => items.push(Item::Note(scope)),
        }
    }
    items
}

/// One rule's fields, joined by two spaces: decision, tool, prefix
/// (`(any)` when empty), `added` as a UTC minute, and the session. An
/// absent `added` or session is left out.
fn fields(row: &RuleRow) -> String {
    let decision = match row.rule.decision {
        RuleDecision::Allow => "allow",
        RuleDecision::Ask => "ask",
        RuleDecision::Deny => "deny",
    };
    let mut fields = vec![decision.to_owned(), row.rule.tool.clone()];
    if row.rule.prefix.is_empty() {
        fields.push("(any)".to_owned());
    } else {
        fields.push(row.rule.prefix.clone());
    }
    if let Some(added) = row.rule.added.and_then(utc_minute) {
        fields.push(added);
    }
    if let Some(session) = &row.rule.session_id {
        fields.push(session.0.clone());
    }
    fields.join("  ")
}

/// The note in place of `section`'s rules: the error and how to fix it,
/// or that the file holds no rules.
fn note(section: Option<&RulesSection>) -> String {
    match section.and_then(|section| section.rows.as_ref().err()) {
        Some(error) => format!("{error}  Ctrl+G opens it to fix it."),
        None => "No rules.".to_owned(),
    }
}

#[cfg(test)]
#[path = "rules_view_tests.rs"]
mod tests;
