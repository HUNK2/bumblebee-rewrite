//! Bumblebee's battle chatter table and the original single-speaker selection rules.
//! [game] 007cc400/007cc720: match action/category/either subcategory, choose uniformly,
//! then test the chosen line's frequency. 007cd880 history compares the three hashes,
//! not the waveform, and 007cd430 applies a randomized speak delay.
use std::{collections::HashMap, path::Path};
use crate::formats::{hash::crc32, lxb::{DataFile, Node}, pack::{self, Pack}};

pub const ATTACK: u32 = crc32(b"CHATTER_ACTION_ATTACK");
pub const DIED: u32 = crc32(b"CHATTER_ACTION_DIED");
pub const INJURED: u32 = crc32(b"CHATTER_ACTION_INJURED");
pub const CHARACTER: u32 = crc32(b"CHATTER_CATEGORY_CHARACTER");
pub const GENERIC: u32 = crc32(b"CHATTER_CATEGORY_GENERIC");
pub const SELF: u32 = crc32(b"CHATTER_SUBCATEGORY_SELF");
pub const GENERIC_SUB: u32 = crc32(b"CHATTER_SUBCATEGORY_GENERIC");
pub const ENEMY: u32 = crc32(b"CHATTER_SUBCATEGORY_ENEMY");

#[derive(Clone, Copy, Debug)]
pub struct Line { pub action: u32, pub category: u32, pub subcategory: u32, pub event: u32, pub frequency: f32 }
pub fn read(node: Node) -> Vec<Line> {
    node.fields().into_iter().filter_map(|(_, n)| {
        let hash = |field| n.get(field).and_then(Node::int).map(|v| v as u32);
        Some(Line { action: hash("action")?, category: hash("category")?, subcategory: hash("subCategory")?,
            event: hash("eventID")?, frequency: n.get("frequency").and_then(Node::float)? })
    }).collect()
}

pub struct Tuning {
    pub enabled: bool,
    pub low_health: f32,
    pub delay: [f32; 2],
    history: HashMap<u32, f32>,
}
impl Tuning {
    pub fn load(game: &Path) -> Result<Self, String> {
        let pack = Pack::open(&game.join("bnxglobal.str"))?;
        for chunk in pack.of_type(pack::DATA) {
            let Ok(file) = DataFile::parse(pack.data(chunk).to_vec()) else { continue };
            let Some(node) = file.root().get("BattleChatter") else { continue };
            let value = |field| node.get(field).and_then(Node::float).ok_or_else(|| format!("BattleChatter.{field} missing"));
            let mut history = HashMap::new();
            for (action, field) in [
                (ATTACK, "attackHistoryTime"), (DIED, "diedHistoryTime"), (INJURED, "injuredHistoryTime"),
                (crc32(b"CHATTER_ACTION_MOVETO"), "moveToHistoryTime"), (crc32(b"CHATTER_ACTION_SPOT"), "spotHistoryTime"),
                (crc32(b"CHATTER_ACTION_TAUNT"), "tauntHistoryTime"), (crc32(b"CHATTER_ACTION_TACTICS"), "tacticsHistoryTime"),
            ] { history.insert(action, value(field)?); }
            return Ok(Self { enabled: node.get("startEnabled").and_then(Node::int) == Some(1),
                low_health: value("charLowHealthPct")?, delay: [value("minSpeakDelay")?, value("maxSpeakDelay")?], history });
        }
        Err("bnxglobal.str has no BattleChatter tuning".into())
    }
}

pub struct Speaker {
    pub lines: Vec<Line>,
    pub tuning: Tuning,
    history: HashMap<(u32, u32, u32), f32>,
}
impl Speaker {
    pub fn new(lines: Vec<Line>, tuning: Tuning) -> Self { Self { lines, tuning, history: HashMap::new() } }
    pub fn step(&mut self, dt: f32) { self.history.retain(|_, left| { *left -= dt; *left >= 0.0 }); }
    pub fn clear(&mut self) { self.history.clear(); }
    pub fn choose(&mut self, action: u32, category: u32, subcategories: [u32; 2], random: &mut impl FnMut() -> f32) -> Option<Line> {
        if !self.tuning.enabled { return None; }
        let candidates: Vec<_> = self.lines.iter().filter(|l| l.action == action && l.category == category
            && subcategories.contains(&l.subcategory)).collect();
        if candidates.is_empty() { return None; }
        let chosen = *candidates[(random().clamp(0.0, 0.999999) * candidates.len() as f32) as usize];
        if random() > chosen.frequency { return None; }
        let key = (chosen.action, chosen.category, chosen.subcategory);
        if self.history.contains_key(&key) { return None; }
        self.history.insert(key, self.tuning.history.get(&action).copied().unwrap_or(0.0));
        Some(chosen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frequency_and_group_history_apply_to_the_selected_line() {
        let mut speaker = Speaker::new(vec![
            Line { action: ATTACK, category: GENERIC, subcategory: GENERIC_SUB, event: 1, frequency: 0.333 },
            Line { action: ATTACK, category: GENERIC, subcategory: GENERIC_SUB, event: 2, frequency: 0.333 },
            Line { action: INJURED, category: CHARACTER, subcategory: SELF, event: 3, frequency: 1.0 },
        ], Tuning { enabled: true, low_health: 0.333, delay: [0.3, 1.5], history: HashMap::from([(ATTACK, 25.0), (INJURED, 25.0)]) });
        assert!(speaker.choose(ATTACK, GENERIC, [GENERIC_SUB, 0], &mut || 0.5).is_none());
        assert_eq!(speaker.choose(ATTACK, GENERIC, [GENERIC_SUB, 0], &mut || 0.1).unwrap().event, 1);
        assert!(speaker.choose(ATTACK, GENERIC, [GENERIC_SUB, 0], &mut || 0.2).is_none(), "history blocks the whole group");
        assert!(speaker.choose(INJURED, CHARACTER, [SELF, 0], &mut || 0.1).is_some());
        speaker.step(25.01);
        assert!(speaker.choose(ATTACK, GENERIC, [GENERIC_SUB, 0], &mut || 0.1).is_some());
    }
    #[test]
    fn installed_chatter_tuning_is_read_without_fitting() {
        let t = Tuning::load(Path::new(r"C:\Games2")).unwrap();
        assert!(t.enabled);
        assert_eq!((t.low_health, t.delay, t.history[&ATTACK]), (0.333, [0.3, 1.5], 25.0));
    }
}
