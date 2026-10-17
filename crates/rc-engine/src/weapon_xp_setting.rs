//! The port option "Weapon experience" (Port Options; rc_game::weapon_level): weapon leveling on or off.
//!
//! On (the default): damaging hits give the weapon in hand experience, the weapon HUD and the quick select ring show
//! the level and the experience meter, and level-ups are announced. Off: the game's own weapons.
//!
//! Read once from `RC_WEAPON_XP=0|1`, else the port settings file's `weapon_xp = on|off`, else on; the menu writes
//! changes back to the file. A process-wide flag (like the other port settings the tick reads) because the gameplay
//! tick, the HUD feed and the menu all need it and none of them owns it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

fn flag() -> &'static AtomicBool {
    static FLAG: OnceLock<AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| {
        let (on, source) = if let Ok(v) = std::env::var("RC_WEAPON_XP") {
            (v.trim() != "0", "RC_WEAPON_XP")
        } else if let Some(v) = crate::render_settings::load_key("weapon_xp") {
            (v.trim() != "off", "settings file")
        } else {
            (true, "default")
        };
        println!("weapon experience: {} ({source})", if on { "on" } else { "off" });
        AtomicBool::new(on)
    })
}

/// Whether weapon leveling is on.
pub fn enabled() -> bool { flag().load(Ordering::Relaxed) }

/// Sets it (the Port Options row) and writes it into the port settings file.
pub fn set_and_save(on: bool) {
    flag().store(on, Ordering::Relaxed);
    crate::render_settings::save_key("weapon_xp", if on { "on" } else { "off" });
}
