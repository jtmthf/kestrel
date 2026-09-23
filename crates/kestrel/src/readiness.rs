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

impl Readiness {
    pub fn hold_reason(&self) -> Option<String> {
        let mut reasons = Vec::new();
        match self.state {
            WorkState::Terminal => reasons.push(format!("{} is closed", self.work_item)),
            WorkState::Unknown => reasons.push(format!("{} has unknown state", self.work_item)),
            WorkState::Open => {}
        }
        match self.delegation {
            Delegation::Absent => {
                reasons.push(format!("{} is no longer delegated", self.work_item))
            }
            Delegation::Unknown => {
                reasons.push(format!("{} has unknown delegation", self.work_item))
            }
            Delegation::Current => {}
        }
        if !self.unresolved_blockers.is_empty() {
            reasons.push(format!(
                "{} is blocked by {}",
                self.work_item,
                self.unresolved_blockers.join(", ")
            ));
        }
        (!reasons.is_empty()).then(|| reasons.join("; "))
    }
}
