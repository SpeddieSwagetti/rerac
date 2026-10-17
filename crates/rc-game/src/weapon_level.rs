//! Weapon leveling (port-only, after Ratchet & Clank 2): every ammo weapon starts at V1 and gains experience
//! whenever one of its attacks damages an enemy (`crate::moby_update::creature::damage::resolve` →
//! [`crate::hero::weapons::Weapons::credit_hit`]). At [`MAX_LEVEL`] the weapon turns gold: the game's own gold flag
//! (`GameState::global.gold_weapons`, 0x13e520 + item) is set, so every gold effect the game already has applies.
//!
//! The experience lives in `GameState::weapon_xp` (by item id; not part of the memory-card format, so only the gold
//! flag survives a save / load: a gold weapon is always shown at [`MAX_LEVEL`]).

/// Whether weapon leveling is on (the port option "Weapon experience"; the engine keeps it in step with the setting).
/// Read where no weapon or HUD state has it at hand (the vendor's ammo capacity).
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub fn enabled() -> bool { ENABLED.load(std::sync::atomic::Ordering::Relaxed) }
pub fn set_enabled(on: bool) { ENABLED.store(on, std::sync::atomic::Ordering::Relaxed) }

/// The last level; reaching it makes the weapon gold.
pub const MAX_LEVEL: u8 = 10;
/// Experience of one damaging hit.
pub const HIT_XP: i32 = 1;
/// Experience needed to go from level `n` to `n + 1` (index `n − 1`).
const STEP: [i32; 9] = [20, 30, 40, 50, 60, 70, 80, 90, 100];

/// Total experience at which `level` starts (level 1: 0).
pub fn xp_at(level: u8) -> i32 { STEP.iter().take((level.clamp(1, MAX_LEVEL) - 1) as usize).sum() }

/// The experience of the last level (what the meter stops at).
pub fn xp_cap() -> i32 { xp_at(MAX_LEVEL) }

/// The level `xp` gives (1..=[`MAX_LEVEL`]).
pub fn level(xp: i32) -> u8 { (1..=MAX_LEVEL).rev().find(|&l| xp >= xp_at(l)).unwrap_or(1) }

/// The level shown for a weapon: gold weapons are always at the maximum.
pub fn display_level(xp: i32, gold: bool) -> u8 { if gold { MAX_LEVEL } else { level(xp) } }

/// The meter's fill as (experience into the level, experience the level takes); full at the maximum level.
pub fn progress(xp: i32, gold: bool) -> (i32, i32) {
    let l = display_level(xp, gold);
    if l >= MAX_LEVEL { return (1, 1); }
    let base = xp_at(l);
    ((xp - base).max(0), xp_at(l + 1) - base)
}

/// The name the level-up popups use (item ids of level01's definitions).
pub fn weapon_name(item: usize) -> &'static str {
    match item {
        9 => "Suck Cannon",
        10 => "Bomb Glove",
        11 => "Devastator",
        13 => "Visibomb",
        14 => "Taunter",
        15 => "Blaster",
        16 => "Pyrocitor",
        17 => "Mine Glove",
        18 => "Walloper",
        19 => "Tesla Claw",
        20 => "Glove of Doom",
        21 => "Morph-o-Ray",
        23 => "R.Y.N.O.",
        24 => "Drone Device",
        25 => "Decoy Glove",
        _ => "Weapon",
    }
}

/// The popup text of a level gained.
pub fn popup_text(item: usize, level: u8) -> String {
    if level >= MAX_LEVEL { return format!("{} upgraded to Gold!", weapon_name(item)); }
    let mut text = format!("{} reached V{}", weapon_name(item), level);
    // A perk gained at this level ([`crate::weapon_perks`]).
    for (_, perk) in crate::weapon_perks::gained(item, level) { text.push_str(&format!(" - {} unlocked", crate::weapon_perks::name(perk))); }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_steps() {
        assert_eq!(level(0), 1);
        assert_eq!(level(19), 1);
        assert_eq!(level(20), 2);
        assert_eq!(level(49), 2);
        assert_eq!(level(50), 3);
        assert_eq!(level(xp_cap() - 1), MAX_LEVEL - 1);
        assert_eq!(level(xp_cap()), MAX_LEVEL);
        assert_eq!(level(i32::MAX), MAX_LEVEL);
    }

    #[test]
    fn hits_level_the_held_weapon_and_the_last_fired_one() {
        use crate::hero::weapons::Weapons;
        let mut w = Weapons::default();
        w.uses_ammo[15] = true; // the Blaster
        w.uses_ammo[10] = true; // the Bomb Glove
        // The wrench (item 8, no ammo) in hand and nothing fired: no experience.
        w.credit_hit(8);
        assert_eq!(w.xp, [0; 37]);
        // The held Blaster gains, and levels up on the 20th hit.
        for _ in 0..19 { w.credit_hit(15); }
        assert_eq!((w.xp[15], w.level_ups.len()), (19, 0));
        w.credit_hit(15);
        assert_eq!(w.level_ups, vec![(15, 2)]);
        // A thrown bomb still counts after the hand holds the wrench: the glove last used ammo.
        assert!(w.use_ammo(10, 0) || w.ammo[10] == 0);
        w.last_fired = 10;
        w.credit_hit(8);
        assert_eq!(w.xp[10], 1);
        // A gold weapon gains nothing; the experience stops at the cap.
        w.gold[15] = 1;
        w.credit_hit(15);
        assert_eq!(w.xp[15], 20);
        w.xp[10] = xp_cap() - 1;
        w.credit_hit(10);
        w.credit_hit(10);
        assert_eq!(w.xp[10], xp_cap());
        assert_eq!(w.level_ups.last(), Some(&(10, MAX_LEVEL)));
    }

    #[test]
    fn progress_and_gold() {
        assert_eq!(progress(0, false), (0, 20));
        assert_eq!(progress(35, false), (15, 30));
        assert_eq!(progress(0, true), (1, 1));
        assert_eq!(display_level(0, true), MAX_LEVEL);
        assert_eq!(popup_text(15, 2), "Blaster reached V2");
        assert_eq!(popup_text(15, 3), "Blaster reached V3 - Ricochet unlocked");
        assert_eq!(popup_text(15, MAX_LEVEL), "Blaster upgraded to Gold!");
    }
}
