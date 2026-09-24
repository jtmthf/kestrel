#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkState {
    Open,
    Terminal,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delegation {
    Current,
    Absent,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub work_item: String,
    pub state: WorkState,
    pub delegation: Delegation,
    pub unresolved_blockers: Vec<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request<'a> {
    Automatic,
    /// Works ahead of blockers, but not of its own withdrawal.
    Commanded {
        by: &'a str,
    },
    /// Its own Delegation, so it works ahead of everything.
    Dispatched,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Start { worked_ahead: Option<String> },
    Hold { because: String },
    Cancel { because: String },
}

impl Readiness {
    pub fn decide(&self, request: Request<'_>) -> Decision {
        let blocked = (!self.unresolved_blockers.is_empty()).then(|| {
            format!(
                "{} is blocked by {}",
                self.work_item,
                self.unresolved_blockers.join(", ")
            )
        });
        let worked_ahead = |by: &str| {
            blocked
                .as_ref()
                .map(|blocked| format!("{blocked}, and {by} asked to work ahead"))
        };
        let commanded_by = match request {
            Request::Dispatched => {
                return Decision::Start {
                    worked_ahead: worked_ahead("an operator's dispatch"),
                };
            }
            Request::Commanded { by } => Some(by),
            Request::Automatic => None,
        };
        if self.delegation == Delegation::Absent {
            return Decision::Cancel {
                because: format!("{} is no longer delegated", self.work_item),
            };
        }

        let mut reasons = Vec::new();
        match self.state {
            WorkState::Terminal => reasons.push(format!("{} is closed", self.work_item)),
            WorkState::Unknown => reasons.push(format!("{} has unknown state", self.work_item)),
            WorkState::Open => {}
        }
        if self.delegation == Delegation::Unknown {
            reasons.push(format!("{} has unknown delegation", self.work_item));
        }
        let worked_ahead = match commanded_by {
            Some(by) => worked_ahead(by),
            None => {
                reasons.extend(blocked);
                None
            }
        };
        if reasons.is_empty() {
            Decision::Start { worked_ahead }
        } else {
            Decision::Hold {
                because: reasons.join("; "),
            }
        }
    }
}
