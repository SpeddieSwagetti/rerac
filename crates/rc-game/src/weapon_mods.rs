//! Weapon mods (port-only, after Ratchet: Deadlocked's Omega mods): a weapon carries at most one mod, chosen on the
//! pause menu's Weapons page (`menus::pause::pages::ammo_update`, stored in `GameState::weapon_mod` by item id).
//!
//! * [`Mod::Acid`]: the hit coats the creature in acid: damage ticks for 5 seconds, and it takes 20% more damage from
//!   Ratchet's weapons meanwhile.
//! * [`Mod::Burn`]: the hit sets the creature burning: a damage tick every half second for 3 seconds.
//! * [`Mod::Shock`]: the hit arcs to the creatures around the target for half its damage.
//!
//! The damage of the effects is a fraction of the hit's own (so it grows with the weapon's level, [`crate::weapon_stats`]),
//! delivered as ordinary hits for the creature (`hit_moby`, `area_hit`) tagged with [`MOD_TAG`] in their flags, which
//! the resolver does not credit with experience or mods again (no feedback loops).

use crate::moby_runtime::MobyId;
use crate::moby_update::creature::{attack, V};
use crate::moby_update::services::{HitRecord, World};

/// The flag bit of the hits the mods deliver (none of the game's receivers' masks has it).
pub const MOD_TAG: u32 = 0x4000_0000;

/// A weapon mod (the value stored in `GameState::weapon_mod`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mod { None, Acid, Burn, Shock }

/// The mods in the menu's order (the index is the stored value).
pub const MODS: [Mod; 4] = [Mod::None, Mod::Acid, Mod::Burn, Mod::Shock];

impl Mod {
    /// The mod stored as `v` (anything unknown: none).
    pub fn from_u8(v: u8) -> Mod { MODS.get(v as usize).copied().unwrap_or(Mod::None) }

    pub fn name(self) -> &'static str {
        match self {
            Mod::None => "None",
            Mod::Acid => "Acid",
            Mod::Burn => "Burn",
            Mod::Shock => "Shock",
        }
    }
}

/// Whether weapon `item` can carry a mod: the ones that damage creatures (item ids of level01's definitions: 10 Bomb
/// Glove, 11 Devastator, 13 Visibomb, 15 Blaster, 16 Pyrocitor, 17 Mine Glove, 19 Tesla Claw, 20 Glove of Doom, 23
/// R.Y.N.O., 24 Drone Device, 25 Decoy Glove). The Suck Cannon, the Taunter and the Morph-o-Ray do no damage.
pub fn can_mod(item: usize) -> bool { matches!(item, 10 | 11 | 13 | 15 | 16 | 17 | 19 | 20 | 23 | 24 | 25) }

/// A damage-over-time effect on a creature.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Status {
    target: MobyId,
    mod_: Mod,
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
        if let Some(e) = self.list.iter_mut().find(|e| e.target == s.target && e.mod_ == s.mod_) {
            e.left = s.left;
            e.dmg = e.dmg.max(s.dmg);
            return;
        }
        if self.list.len() < 64 { self.list.push(s); }
    }

    /// Whether `target` is coated in acid.
    pub fn acid_on(&self, target: MobyId) -> bool { self.list.iter().any(|e| e.target == target && e.mod_ == Mod::Acid) }
}

/// Whether a moby of class `o_class` is a creature the mods apply to: not one of the crates (classes 500..540, the
/// split `set_death_bits` makes between crates and enemies).
fn is_enemy(o_class: i16) -> bool { !(500..540).contains(&o_class) }

/// One of Ratchet's hits (`rec`, `damage` after the game's own scaling and the level's bonus) lands on creature
/// `target`: the acid's damage bonus, and the mod of the weapon in hand (else the last fired).
pub fn on_hit(w: &mut World, target: MobyId, rec: &HitRecord, damage: &mut f32) {
    if w.svc.weapon_fx.acid_on(target) { *damage *= 1.2; }
    let held = w.hero.items.slot.id;
    let Some(item) = w.hero.weapons.credited_item(held) else { return };
    let selected = Mod::from_u8(w.hero.weapons.mod_sel[item]);
    if selected == Mod::None { return; }
    let Some(attacker) = w.hero_moby else { return };
    if !is_enemy(w.m(target).o_class) { return; }
    let flags = rec.flags | MOD_TAG;
    match selected {
        Mod::None => {}
        Mod::Burn => w.svc.weapon_fx.add(Status { target, mod_: selected, left: 180, every: 30, dmg: 0.15 * *damage, flags }),
        Mod::Acid => w.svc.weapon_fx.add(Status { target, mod_: selected, left: 300, every: 30, dmg: 0.08 * *damage, flags }),
        Mod::Shock => {
            let centre: V = w.m(target).position;
            attack::area_hit(w, 2.5, centre, attacker, 0.5 * *damage, 0.0, 0.0, Some(target), flags, rec.b28, rec.b29);
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
    list.extend(std::mem::take(&mut w.svc.weapon_fx.list));
    w.svc.weapon_fx.list = list;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mods_round_trip_and_unknown_is_none() {
        for (i, m) in MODS.iter().enumerate() { assert_eq!(Mod::from_u8(i as u8), *m); }
        assert_eq!(Mod::from_u8(200), Mod::None);
        assert_eq!(MODS[0], Mod::None);
    }

    #[test]
    fn only_damaging_weapons_take_mods() {
        assert!(can_mod(15) && can_mod(16) && can_mod(10));
        assert!(!can_mod(9) && !can_mod(14) && !can_mod(21) && !can_mod(0));
    }

    #[test]
    fn enemies_are_not_crates() {
        assert!(!is_enemy(500));
        assert!(!is_enemy(539));
        assert!(is_enemy(540));
        assert!(is_enemy(0x2a0));
    }
}
