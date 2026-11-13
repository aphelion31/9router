//! Combo member plan: configured order minus members in first-response grace.
use super::cooldown::Cooldowns;

pub fn grace_key(combo: &str, member: &str) -> String {
    format!("{combo}\0{member}")
}

/// Keeps configured order. Drops members still in grace for this combo.
// ponytail: grace applies only to multi-member combos (as combo.js). No streaming
// first-response timeout yet, so nothing sets grace from here.
pub fn plan(cd: &Cooldowns, combo: &str, members: &[String]) -> Vec<String> {
    if members.len() <= 1 {
        return members.to_vec();
    }
    members
        .iter()
        .filter(|m| !cd.is_cooling(&grace_key(combo, m)))
        .cloned()
        .collect()
}
