//! Who is talking to Guss and what they may do. Mirrors Friday's chain of
//! command (owner > manager > staff) so a Friday-connected Guss can map
//! Friday roles onto these.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Staff,
    Manager,
    Owner,
}

/// Things a person can ask Guss to do that not everyone may.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Sales, expenses, stock.
    Record,
    /// Today's report.
    ReportToday,
    /// Week and month reports.
    ReportLonger,
    /// The recent entries list.
    History,
    /// Void an entry outright (others: a request instead).
    VoidDirectly,
    /// Approve or reject someone's void request.
    DecideVoids,
}

impl Role {
    pub fn can(self, action: Action) -> bool {
        match action {
            Action::Record | Action::ReportToday | Action::History => true,
            Action::ReportLonger | Action::VoidDirectly | Action::DecideVoids => {
                self >= Role::Manager
            }
        }
    }

    /// Whether this role may void or approve voiding an entry recorded by
    /// someone with `author` role. Managers can't overrule the owner.
    pub fn outranks_for_voids(self, author: Option<Role>) -> bool {
        match self {
            Role::Owner => true,
            Role::Manager => author.is_none_or(|a| a <= Role::Manager),
            Role::Staff => false,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Manager => "manager",
            Role::Staff => "staff",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "owner" => Some(Role::Owner),
            "manager" => Some(Role::Manager),
            "staff" => Some(Role::Staff),
            _ => None,
        }
    }
}

/// The person behind a message: "telegram:12345" / "Mary" / Staff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    /// Stable id with its source, e.g. "telegram:12345", "web", "cli".
    pub id: String,
    pub name: String,
    pub role: Role,
}

impl Actor {
    pub fn new(id: impl Into<String>, name: impl Into<String>, role: Role) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            role,
        }
    }

    /// The single local user of the CLI, or a channel without its own
    /// people (until Friday links its users).
    pub fn owner(id: impl Into<String>) -> Self {
        let id = id.into();
        Self::new(id.clone(), id, Role::Owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staff_record_but_do_not_void_or_decide() {
        assert!(Role::Staff.can(Action::Record));
        assert!(Role::Staff.can(Action::ReportToday));
        assert!(!Role::Staff.can(Action::ReportLonger));
        assert!(!Role::Staff.can(Action::VoidDirectly));
        assert!(!Role::Staff.can(Action::DecideVoids));
        assert!(Role::Manager.can(Action::DecideVoids));
    }

    #[test]
    fn managers_cannot_overrule_the_owner() {
        assert!(Role::Manager.outranks_for_voids(Some(Role::Staff)));
        assert!(Role::Manager.outranks_for_voids(Some(Role::Manager)));
        assert!(!Role::Manager.outranks_for_voids(Some(Role::Owner)));
        assert!(Role::Owner.outranks_for_voids(Some(Role::Owner)));
        assert!(!Role::Staff.outranks_for_voids(Some(Role::Staff)));
    }
}
