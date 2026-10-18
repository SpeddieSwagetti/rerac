//! Weapon level bonuses (port-only, after Ratchet & Clank 2; [`crate::weapon_level`] gives the levels).
//!
//! Every level raises the weapon's **damage** ([`damage_mult`], applied to the damage of Ratchet's hits on creatures in
//! `creature::damage::resolve`), its **fire rate** ([`fire_rate_mult`]: the held weapon's fire timers run faster,
//! `Weapons::speed_up_fire`) and its **ammo capacity** ([`max_ammo`]: pickups, crates, the HUD, the vendor).

use crate::moby_update::services::World;
use crate::weapon_level;

/// Damage: +8% a level (V10: ×1.72).
pub fn damage_mult(level: u8) -> f32 { 1.0 + 0.08 * (level.clamp(1, weapon_level::MAX_LEVEL) - 1) as f32 }
/// Fire rate: +4% a level (V10: ×1.36).
pub fn fire_rate_mult(level: u8) -> f32 { 1.0 + 0.04 * (level.clamp(1, weapon_level::MAX_LEVEL) - 1) as f32 }
/// Ammo capacity: +10% a level (V10: ×1.9), rounded up.
pub fn max_ammo(base: i32, level: u8) -> i32 { (base as f32 * (1.0 + 0.10 * (level.clamp(1, weapon_level::MAX_LEVEL) - 1) as f32)).ceil() as i32 }

/// The damage factor of Ratchet's hit now: the level of the weapon in hand, else the last fired ([`damage_mult`]);
/// 1 when no ammo weapon is credited.
pub fn damage_factor(w: &World) -> f32 {
    let held = w.hero.items.slot.id;
    let Some(item) = w.hero.weapons.credited_item(held) else { return 1.0 };
    damage_mult(weapon_level::display_level(w.hero.weapons.xp[item], w.hero.weapons.gold[item] != 0))
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
}
