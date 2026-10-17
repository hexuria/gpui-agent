//! Thin wrapper over `CandidateSet` + `Scores` + `arbitrate`, shaped like
//! `aui-policy/src/instinct_policy.rs`'s `choose_*` heads minus
//! habituation history (no history exists in gpui-agent yet).
//!
//! Callers supply `(id, evidence-millis)` pairs; [`choose`] returns a
//! [`Choice`] — `Chosen` names exactly one offered id, `Abstained` can
//! never be dispatched. Default profile [`Profile::Standard`]
//! (min_confidence 750 / min_margin 150); never lower it to pass a test.

use instinct_core::{
    Affinity, Answer, CandidateId, CandidateSet, Confidence, DriveIndex, Drives, Profile, Scores,
    arbitrate,
};

/// One ranked candidate as reported by the arbitration trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedCandidate {
    pub id: String,
    /// Post-arbitration urge in millis (0..=1000).
    pub confidence_millis: i16,
}

/// The gated outcome of [`choose`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Exactly one offered id cleared the profile's thresholds.
    Chosen {
        id: String,
        ranked: Vec<RankedCandidate>,
    },
    /// Nothing cleared the gate — the ranked list is for diagnostics /
    /// escalation only and may not be dispatched.
    Abstained {
        why: String,
        ranked: Vec<RankedCandidate>,
    },
}

impl Choice {
    /// The chosen id, or `None` on abstain.
    pub fn chosen_id(&self) -> Option<&str> {
        match self {
            Self::Chosen { id, .. } => Some(id),
            Self::Abstained { .. } => None,
        }
    }

    /// Ranked candidates (descending urge) for diagnostics/escalation.
    pub fn ranked(&self) -> &[RankedCandidate] {
        match self {
            Self::Chosen { ranked, .. } | Self::Abstained { ranked, .. } => ranked,
        }
    }
}

/// Errors are internal invariants only (bad ids, mismatched lengths) —
/// abstain is a value, not an error.
#[derive(Debug, thiserror::Error)]
pub enum ArbitrationError {
    #[error("candidate ids must be unique and non-empty: {0}")]
    Candidates(String),
    #[error("instinct arbitration failed: {0}")]
    Instinct(String),
}

/// `choose(candidates, profile)` — gate `candidates` through Instinct and
/// return the gated [`Choice`]. `None` profile means [`Profile::Standard`].
pub fn choose(
    candidates: &[(String, Confidence)],
    profile: Option<Profile>,
) -> Result<Choice, ArbitrationError> {
    let profile = profile.unwrap_or_default();
    match candidates.len() {
        0 => {
            return Ok(Choice::Abstained {
                why: "no candidates".into(),
                ranked: Vec::new(),
            });
        }
        1 => {
            let (id, evidence) = &candidates[0];
            let ranked = vec![RankedCandidate {
                id: id.clone(),
                confidence_millis: evidence.get(),
            }];
            if *evidence < profile.thresholds().min_confidence {
                return Ok(Choice::Abstained {
                    why: "below min_confidence".into(),
                    ranked,
                });
            }
            return Ok(Choice::Chosen {
                id: id.clone(),
                ranked,
            });
        }
        _ => {}
    }

    let ids: Vec<CandidateId> = candidates
        .iter()
        .map(|(id, _)| {
            CandidateId::new(id.as_str()).map_err(|e| ArbitrationError::Candidates(e.to_string()))
        })
        .collect::<Result<_, _>>()?;
    let set = CandidateSet::new(ids).map_err(|e| ArbitrationError::Candidates(e.to_string()))?;
    let question = set
        .question("candidate")
        .map_err(|e| ArbitrationError::Instinct(e.to_string()))?;
    // Uniform drive + neutral affinity: no history yet, so urges equal
    // evidence and `arbitrate` reduces to the threshold+margin gate.
    let drives = Drives::new(&[("goal", Confidence::MAX)])
        .map_err(|e| ArbitrationError::Instinct(e.to_string()))?;
    let mut scores = Scores::new(&question);
    let mut affinity = Affinity::new(&question, &drives);
    for (id, conf) in candidates {
        let idx = set
            .index_of(id.as_str())
            .ok_or_else(|| ArbitrationError::Candidates(format!("missing option {id}")))?;
        scores
            .set(idx, *conf)
            .map_err(|e| ArbitrationError::Instinct(e.to_string()))?;
        affinity
            .set(DriveIndex::new(0), idx, Confidence::MAX)
            .map_err(|e| ArbitrationError::Instinct(e.to_string()))?;
    }
    let arbitration = arbitrate(&scores, &drives, &affinity, None, profile)
        .map_err(|e| ArbitrationError::Instinct(e.to_string()))?;
    let ranked = ranked_from_answer(&set, arbitration.answer(), arbitration.urges())?;

    match arbitration.answer() {
        Answer::Choice { option, .. } => {
            let id = set
                .id(*option)
                .ok_or_else(|| ArbitrationError::Instinct("chosen option missing".into()))?;
            Ok(Choice::Chosen {
                id: id.as_str().to_string(),
                ranked,
            })
        }
        Answer::Abstain { why, .. } => Ok(Choice::Abstained {
            why: format!("{why:?}"),
            ranked,
        }),
        other => Err(ArbitrationError::Instinct(format!(
            "unexpected answer {other:?}"
        ))),
    }
}

fn ranked_from_answer(
    set: &CandidateSet,
    answer: &Answer,
    urges: &Scores<'_>,
) -> Result<Vec<RankedCandidate>, ArbitrationError> {
    let entries = match answer {
        Answer::Choice { ranked, .. } | Answer::Abstain { ranked, .. } => ranked.entries(),
        _ => return Ok(Vec::new()),
    };
    entries
        .iter()
        .map(|(idx, _)| {
            let id = set
                .id(*idx)
                .ok_or_else(|| ArbitrationError::Instinct("missing ranked option".into()))?;
            let urge = urges
                .get(*idx)
                .ok_or_else(|| ArbitrationError::Instinct("missing urge".into()))?;
            Ok(RankedCandidate {
                id: id.as_str().to_string(),
                confidence_millis: urge.get(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(v: i16) -> Confidence {
        Confidence::new(v).unwrap()
    }

    #[test]
    fn empty_abstains() {
        let choice = choose(&[], None).unwrap();
        assert!(matches!(choice, Choice::Abstained { .. }));
        assert!(choice.chosen_id().is_none());
    }

    #[test]
    fn single_above_threshold_chooses() {
        let choice = choose(&[("a".into(), c(800))], None).unwrap();
        assert_eq!(choice.chosen_id(), Some("a"));
    }

    #[test]
    fn single_below_threshold_abstains() {
        let choice = choose(&[("a".into(), c(500))], None).unwrap();
        assert!(choice.chosen_id().is_none());
    }

    #[test]
    fn single_at_exactly_min_confidence_chooses() {
        // Boundary: min_confidence is inclusive — 750 chooses, 749
        // abstains (Standard profile 750/150).
        let at = choose(&[("a".into(), c(750))], None).unwrap();
        assert_eq!(at.chosen_id(), Some("a"));
        let under = choose(&[("a".into(), c(749))], None).unwrap();
        assert!(under.chosen_id().is_none());
    }

    #[test]
    fn clear_winner_chooses() {
        let choice = choose(
            &[
                ("a".into(), c(900)),
                ("b".into(), c(400)),
                ("c".into(), c(200)),
            ],
            None,
        )
        .unwrap();
        assert_eq!(choice.chosen_id(), Some("a"));
    }

    #[test]
    fn thin_margin_abstains() {
        // 800 vs 780: margin 20 < 150 → abstain, never a pick.
        let choice = choose(&[("a".into(), c(800)), ("b".into(), c(780))], None).unwrap();
        assert!(choice.chosen_id().is_none());
        assert_eq!(choice.ranked().len(), 2);
    }

    #[test]
    fn all_below_confidence_abstains() {
        let choice = choose(&[("a".into(), c(400)), ("b".into(), c(300))], None).unwrap();
        assert!(choice.chosen_id().is_none());
    }

    #[test]
    fn deterministic_on_input_order() {
        let a = choose(&[("x".into(), c(900)), ("y".into(), c(400))], None).unwrap();
        let b = choose(&[("y".into(), c(400)), ("x".into(), c(900))], None).unwrap();
        assert_eq!(a, b);
    }
}
