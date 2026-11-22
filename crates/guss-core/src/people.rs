//! Who is talking to Guss and what they may do. Mirrors Friday's chain of
//! command (owner > manager > staff) so a Friday-connected Guss can map
//! Friday roles onto these.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Staff,
    /// Friday's team leader: decides the void requests of the people who
    /// report to them, up to TEAM_LEADER_LIMIT_MAJOR (plan 12.3).
    #[serde(rename = "team_leader")]
    TeamLeader,
    Manager,
    Owner,
}

/// The most a team leader may approve per entry, in the business currency
/// (Friday's friday_common.access APPROVAL_LIMIT_CENTS: KES 2,000).
// shortcut: one fixed limit like Friday's; read it from Friday when limits become per business.
pub const TEAM_LEADER_LIMIT_MAJOR: f64 = 2_000.0;

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
            Action::DecideVoids => self >= Role::TeamLeader,
            Action::ReportLonger | Action::VoidDirectly => self >= Role::Manager,
        }
    }

    /// Whether this role may void or approve voiding an entry recorded by
    /// someone with `author` role. Managers can't overrule the owner.
    pub fn outranks_for_voids(self, author: Option<Role>) -> bool {
        match self {
            Role::Owner => true,
            Role::Manager => author.is_none_or(|a| a <= Role::Manager),
            // Only staff entries, and only for their team (checked by the caller).
            Role::TeamLeader => author == Some(Role::Staff),
            Role::Staff => false,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Manager => "manager",
            Role::TeamLeader => "team_leader",
            Role::Staff => "staff",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "owner" => Some(Role::Owner),
            "manager" => Some(Role::Manager),
            "team_leader" => Some(Role::TeamLeader),
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
    /// A team leader's team: the actor ids of the people who report to
    /// them ("telegram:12345", "friday:<user id>"). Empty for everyone else.
    #[serde(default)]
    pub team: Vec<String>,
}

impl Actor {
    pub fn new(id: impl Into<String>, name: impl Into<String>, role: Role) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            role,
            team: Vec::new(),
        }
    }

    pub fn with_team(mut self, team: Vec<String>) -> Self {
        self.team = team;
        self
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
        assert!(Role::TeamLeader.outranks_for_voids(Some(Role::Staff)));
        assert!(!Role::TeamLeader.outranks_for_voids(Some(Role::TeamLeader)));
        assert!(!Role::TeamLeader.outranks_for_voids(None));
    }

    #[test]
    fn team_leaders_decide_but_do_not_void_directly() {
        assert!(Role::TeamLeader.can(Action::DecideVoids));
        assert!(!Role::TeamLeader.can(Action::VoidDirectly));
        assert!(!Role::TeamLeader.can(Action::ReportLonger));
        assert_eq!(Role::parse("team_leader"), Some(Role::TeamLeader));
        assert_eq!(
            serde_json::to_string(&Role::TeamLeader).unwrap(),
            "\"team_leader\""
        );
    }
}
