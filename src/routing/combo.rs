//! Combo member plan: configured order minus members in first-response grace.
use super::cooldown::Cooldowns;

pub fn grace_key(combo: &str, member: &str) -> String {
    format!("{combo}\0{member}")
}

/// Keeps configured order. Drops members still in grace for this combo.
// Grace is set by chat.rs when a member sends no first byte within
// COMBO_FIRST_RESPONSE_TIMEOUT_MS. It applies only to multi-member combos (as combo.js).
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
