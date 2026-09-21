use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

macro_rules! classification {
    ($name:ident { $($variant:ident => $wire:literal),+ }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $wire)] $variant),+ }
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $wire),+ } }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
classification!(Risk { Low => "low", Medium => "medium", High => "high", Critical => "critical", Unknown => "unknown" });
classification!(Authorization { High => "high", Medium => "medium", Low => "low", Unknown => "unknown" });
classification!(Policy { Permitted => "permitted", Prohibited => "prohibited", NeedsConfirmation => "needs_confirmation", Unknown => "unknown" });
classification!(Outcome { Allow => "allow", Deny => "deny", Ask => "ask", ForceAsk => "force_ask" });
classification!(RuleId { ProhibitedOrCritical => "prohibited_or_critical", IncompleteEvidence => "incomplete_evidence", ApprovalThresholdsMet => "approval_thresholds_met", ConfirmationOrUncertainty => "confirmation_or_uncertainty" });
classification!(CheckKind { Evidence => "evidence", Risk => "risk", Authorization => "authorization", Policy => "policy" });

pub trait Distribution<C: Copy> {
    fn probability(&self, choice: C) -> f64;
    fn values(&self) -> Vec<f64>;
    fn validate(&self, selected: C) -> Result<()> {
        let values = self.values();
        ensure!(
            values
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "Invalid probability value"
        );
        ensure!(
            values
                .iter()
                .all(|v| *v <= self.probability(selected) + 1e-9),
            "Choice does not match maximum probability"
        );
        // Preserve approximate sums, following the official SDK.
        Ok(())
    }
    fn sum(&self, choices: &[C]) -> f64 {
        choices.iter().map(|c| self.probability(*c)).sum()
    }
}
macro_rules! distribution {
    ($name:ident, $choice:ident { $($field:ident => $variant:ident),+ }) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        pub struct $name { $(pub $field: f64),+ }
        impl Distribution<$choice> for $name {
            fn probability(&self, choice: $choice) -> f64 {
                match choice { $($choice::$variant => self.$field),+ }
            }
            fn values(&self) -> Vec<f64> { vec![$(self.$field),+] }
        }
    };
}
distribution!(RiskProbabilities, Risk { low => Low, medium => Medium, high => High, critical => Critical, unknown => Unknown });
distribution!(AuthorizationProbabilities, Authorization { high => High, medium => Medium, low => Low, unknown => Unknown });
distribution!(PolicyProbabilities, Policy { permitted => Permitted, prohibited => Prohibited, needs_confirmation => NeedsConfirmation, unknown => Unknown });

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probabilities {
    pub risk: RiskProbabilities,
    pub authorization: AuthorizationProbabilities,
    pub policy: PolicyProbabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Judgment {
    pub risk: Risk,
    pub authorization: Authorization,
    pub policy: Policy,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probabilities: Option<Probabilities>,
}
impl Judgment {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.rationale.trim().is_empty(),
            "Missing judgment rationale"
        );
        if let Some(p) = &self.probabilities {
            p.risk.validate(self.risk)?;
            p.authorization.validate(self.authorization)?;
            p.policy.validate(self.policy)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationCheck<C> {
    pub passed: bool,
    pub accepted_choices: Vec<C>,
    pub choice: C,
    pub probability: Option<f64>,
    pub threshold: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizationCheck {
    #[serde(flatten)]
    pub classification: ClassificationCheck<Authorization>,
    pub required: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceCheck {
    pub passed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalChecks {
    pub evidence: EvidenceCheck,
    pub risk: ClassificationCheck<Risk>,
    pub authorization: AuthorizationCheck,
    pub policy: ClassificationCheck<Policy>,
}
impl ApprovalChecks {
    pub fn passed(&self) -> bool {
        self.failed().is_empty()
    }
    pub fn failed(&self) -> Vec<CheckKind> {
        [
            (CheckKind::Evidence, self.evidence.passed),
            (CheckKind::Risk, self.risk.passed),
            (
                CheckKind::Authorization,
                self.authorization.classification.passed,
            ),
            (CheckKind::Policy, self.policy.passed),
        ]
        .into_iter()
        .filter_map(|(kind, passed)| (!passed).then_some(kind))
        .collect()
    }
}
