//! Weapon level bonuses and perks (port-only, after Ratchet & Clank 2; [`crate::weapon_level`] gives the levels).
//!
//! Every level raises the weapon's **damage** ([`damage_mult`], applied to the damage of Ratchet's hits on creatures in
//! `creature::damage::resolve`), its **fire rate** ([`fire_rate_mult`]: the held weapon's fire timers run faster,
//! `Weapons::speed_up_fire`) and its **ammo capacity** ([`max_ammo`]: pickups, crates, the HUD, the vendor).
//! At set levels a weapon gains **perks** ([`perks`]):
//!
//! * [`Perk::Ricochet`]: the Blaster's shots bounce off walls (the game's gold Blaster mechanism, `blaster_shot`).
//! * [`Perk::Burn`]: the hit sets the creature burning: a damage tick every half second for 3 seconds.
//! * [`Perk::Acid`]: the hit coats the creature in acid: damage ticks for 5 seconds, and it takes 20% more damage from
//!   Ratchet's weapons meanwhile.
//! * [`Perk::Shock`]: the hit arcs to the creatures around the target for half its damage.
//!
//! The damage of the effects is a fraction of the hit's own, delivered as ordinary hits for the creature (`hit_moby`,
//! `area_hit`) tagged with [`PERK_TAG`] in their flags, which the resolver does not credit with experience or perks
//! again (no feedback loops).

use crate::moby_runtime::MobyId;
use crate::moby_update::creature::{attack, V};
use crate::moby_update::services::{HitRecord, World};
use crate::weapon_level;

/// The flag bit of the hits the perks deliver (none of the game's receivers' masks has it).
pub const PERK_TAG: u32 = 0x4000_0000;

/// Damage: +8% a level (V10: ×1.72).
pub fn damage_mult(level: u8) -> f32 { 1.0 + 0.08 * (level.clamp(1, weapon_level::MAX_LEVEL) - 1) as f32 }
/// Fire rate: +4% a level (V10: ×1.36).
pub fn fire_rate_mult(level: u8) -> f32 { 1.0 + 0.04 * (level.clamp(1, weapon_level::MAX_LEVEL) - 1) as f32 }
/// Ammo capacity: +10% a level (V10: ×1.9), rounded up.
pub fn max_ammo(base: i32, level: u8) -> i32 { (base as f32 * (1.0 + 0.10 * (level.clamp(1, weapon_level::MAX_LEVEL) - 1) as f32)).ceil() as i32 }

/// A weapon perk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perk { Ricochet, Burn, Acid, Shock }

/// The perks of weapon `item` with the level they come at (item ids of level01's definitions: 10 Bomb Glove, 11
/// Devastator, 13 Visibomb, 15 Blaster, 16 Pyrocitor, 17 Mine Glove, 19 Tesla Claw, 20 Glove of Doom, 23 R.Y.N.O.,
/// 24 Drone Device, 25 Decoy Glove). The Suck Cannon, the Taunter and the Morph-o-Ray do no damage: none.
pub fn perks(item: usize) -> &'static [(u8, Perk)] {
    use Perk::*;
    match item {
        15 => &[(3, Ricochet), (4, Shock), (6, Burn), (8, Acid)],
        16 => &[(3, Acid), (6, Shock), (9, Burn)],
        11 => &[(3, Burn), (6, Shock), (9, Acid)],
        10 => &[(3, Burn), (6, Shock), (9, Acid)],
        17 => &[(3, Acid), (6, Burn), (9, Shock)],
        19 => &[(3, Acid), (6, Burn), (9, Shock)],
        20 => &[(3, Shock), (6, Acid), (9, Burn)],
        13 => &[(3, Burn), (6, Shock), (9, Acid)],
        23 => &[(3, Burn), (6, Acid), (9, Shock)],
        24 => &[(3, Shock), (6, Burn), (9, Acid)],
        25 => &[(3, Acid), (6, Burn), (9, Shock)],
        _ => &[],
    }
}

/// The effects the weapon menu lets the player switch on and off, by cursor index (bit `1 << index` of
/// `GameState::weapon_fx_off` / `Weapons::fx_off` set: that effect is off for the weapon). Every damaging weapon has
/// all three, each unlocked at its own level ([`unlock_level`]); the Blaster's ricochet is not switchable.
pub const EFFECTS: [Perk; 3] = [Perk::Acid, Perk::Burn, Perk::Shock];

/// The switchable effects weapon `item` has (empty: it does no damage).
pub fn effect_perks(item: usize) -> Vec<Perk> { EFFECTS.iter().copied().filter(|&e| unlock_level(item, e).is_some()).collect() }

/// The level weapon `item` gets `perk` at.
pub fn unlock_level(item: usize, perk: Perk) -> Option<u8> { perks(item).iter().find(|&&(_, p)| p == perk).map(|&(l, _)| l) }

/// The bit of `perk` in a weapon's off-mask (0 for the ones that cannot be switched).
pub fn off_bit(perk: Perk) -> u8 { EFFECTS.iter().position(|&e| e == perk).map_or(0, |i| 1 << i) }

/// Whether weapon `item` at `level` has `perk`.
pub fn has(item: usize, level: u8, perk: Perk) -> bool { perks(item).iter().any(|&(l, p)| p == perk && l <= level) }

/// The perks `item` has gained at `level` that it had not at `level − 1` (for the level-up popup).
pub fn gained(item: usize, level: u8) -> Vec<(u8, Perk)> { perks(item).iter().copied().filter(|&(l, _)| l == level).collect() }

/// The perk's name for the popup.
pub fn name(p: Perk) -> &'static str {
    match p {
        Perk::Ricochet => "Ricochet",
        Perk::Burn => "Burn",
        Perk::Acid => "Acid",
        Perk::Shock => "Shock",
    }
}

/// The Blaster's shot bounces at `level` (the gold Blaster's own count is gold + 5): 2 from V3, 3 from V6, 4 from V9.
pub fn ricochet_bounces(level: u8) -> i32 { if has(15, level, Perk::Ricochet) { 2 + ((level as i32 - 3) / 3).max(0) } else { 0 } }

/// A damage-over-time effect on a creature.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Status {
    target: MobyId,
    perk: Perk,
    /// Ticks left, and ticks between damage ticks.
    left: i32,
    every: i32,
    /// The damage of one tick and the flags of the hit that started it.
    dmg: f32,
    flags: u32,
}

/// The effects running ([`crate::moby_update::services::Services::weapon_fx`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusTable {
    list: Vec<Status>,
}

impl StatusTable {
    /// Starts (or renews, keeping the stronger damage) `s`.
    fn add(&mut self, s: Status) {
        if let Some(e) = self.list.iter_mut().find(|e| e.target == s.target && e.perk == s.perk) {
            e.left = s.left;
            e.dmg = e.dmg.max(s.dmg);
            return;
        }
        if self.list.len() < 64 { self.list.push(s); }
    }

    /// Whether `target` is coated in acid.
    pub fn acid_on(&self, target: MobyId) -> bool { self.list.iter().any(|e| e.target == target && e.perk == Perk::Acid) }
}

/// Whether a moby of class `o_class` is a creature the perks apply to: not one of the crates (classes 500..540, the
/// split `set_death_bits` makes between crates and enemies).
fn is_enemy(o_class: i16) -> bool { !(500..540).contains(&o_class) }

/// One of Ratchet's hits (`rec`, `damage` after the game's own scaling) lands on creature `target`: the level's
/// damage bonus, the acid's bonus, and the weapon's perks. The weapon is the one in hand, else the last fired.
pub fn on_hit(w: &mut World, target: MobyId, rec: &HitRecord, damage: &mut f32) {
    let held = w.hero.items.slot.id;
    let Some(item) = w.hero.weapons.credited_item(held) else { return };
    let level = weapon_level::display_level(w.hero.weapons.xp[item], w.hero.weapons.gold[item] != 0);
    *damage *= damage_mult(level);
    if w.svc.weapon_fx.acid_on(target) { *damage *= 1.2; }
    let Some(attacker) = w.hero_moby else { return };
    if !is_enemy(w.m(target).o_class) { return; }
    let flags = rec.flags | PERK_TAG;
    for &(at, perk) in perks(item) {
        if level < at { continue; }
        // Switched off in the weapon menu (the effects only; the ricochet has no bit).
        if w.hero.weapons.fx_off[item] & off_bit(perk) != 0 { continue; }
        match perk {
            Perk::Ricochet => {}
            Perk::Burn => w.svc.weapon_fx.add(Status { target, perk, left: 180, every: 30, dmg: 0.15 * *damage, flags }),
            Perk::Acid => w.svc.weapon_fx.add(Status { target, perk, left: 300, every: 30, dmg: 0.08 * *damage, flags }),
            Perk::Shock => {
                let centre: V = w.m(target).position;
                attack::area_hit(w, 2.5, centre, attacker, 0.5 * *damage, 0.0, 0.0, Some(target), flags, rec.b28, rec.b29);
            }
        }
    }
}

/// Once a tick before the creatures update: the damage ticks of the running effects.
pub fn tick_status(w: &mut World) {
    if w.svc.weapon_fx.list.is_empty() { return; }
    let Some(attacker) = w.hero_moby else {
        w.svc.weapon_fx.list.clear();
        return;
    };
    let mut list = std::mem::take(&mut w.svc.weapon_fx.list);
    for s in &mut list {
        s.left -= 1;
        // The creature is gone (deleted or its slot reused by something that is no creature).
        let alive = w.table.mobys.get(s.target).is_some_and(|m| m.state < 0xfd && is_enemy(m.o_class));
        if !alive { s.left = 0; continue; }
        if s.left % s.every == 0 && 0 < s.left {
            let pos: V = w.m(s.target).position;
            attack::hit_moby(w, s.target, attacker, s.dmg, s.flags, pos, [0.0; 4]);
        }
    }
    list.retain(|s| 0 < s.left);
    // Effects started while ticking (none today) are kept.
    list.extend(std::mem::take(&mut w.svc.weapon_fx.list));
    w.svc.weapon_fx.list = list;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_scale_the_stats() {
        assert_eq!(damage_mult(1), 1.0);
        assert!((damage_mult(10) - 1.72).abs() < 1e-5);
        assert!((fire_rate_mult(10) - 1.36).abs() < 1e-5);
        assert_eq!(max_ammo(40, 1), 40);
        assert_eq!(max_ammo(40, 10), 76);
        assert!(damage_mult(3) < damage_mult(4));
    }

    #[test]
    fn perks_come_at_their_levels() {
        assert!(!has(15, 2, Perk::Ricochet));
        assert!(has(15, 3, Perk::Ricochet));
        assert!(has(15, 10, Perk::Burn));
        assert_eq!(ricochet_bounces(2), 0);
        assert_eq!((ricochet_bounces(3), ricochet_bounces(6), ricochet_bounces(9)), (2, 3, 4));
        assert_eq!(gained(10, 3), vec![(3, Perk::Burn)]);
        assert!(perks(9).is_empty());
        // Every damaging weapon has the three switchable effects, each with its own bit.
        assert_eq!(effect_perks(16), vec![Perk::Acid, Perk::Burn, Perk::Shock]);
        assert!(effect_perks(21).is_empty());
        assert_eq!((off_bit(Perk::Acid), off_bit(Perk::Burn), off_bit(Perk::Shock), off_bit(Perk::Ricochet)), (1, 2, 4, 0));
        assert_eq!(unlock_level(25, Perk::Shock), Some(9));
    }

    #[test]
    fn enemies_are_not_crates() {
        assert!(!is_enemy(500));
        assert!(!is_enemy(539));
        assert!(is_enemy(540));
        assert!(is_enemy(0x2a0));
    }
}
