//! The inspect overlay (dev): with `RC_DEBUG_HUD=1` (`show`: shown from the start), F3 shows or hides a column of live data over the HUD (the level,
//! tick and game mode; Ratchet's position, facing, state, speed and ground; the camera; the moby the camera looks at;
//! the scene). F9's dump (crate::gameplay) prints the same lines. Without the switch nothing is added.

use bevy::prelude::*;

/// Whether the overlay is shown (F3).
#[derive(Resource, Default)]
pub struct DebugHud {
    pub on: bool,
}

pub struct DebugHudPlugin;

impl Plugin for DebugHudPlugin {
    fn build(&self, app: &mut App) {
        match std::env::var("RC_DEBUG_HUD").as_deref().map(str::trim) {
            Ok("1") => { app.init_resource::<DebugHud>().add_systems(Update, toggle); }
            // Shown from the start (headless captures, which cannot press F3).
            Ok("show") => { app.insert_resource(DebugHud { on: true }).add_systems(Update, toggle); }
            _ => {}
        }
    }
}

fn toggle(keys: Res<ButtonInput<KeyCode>>, mut d: ResMut<DebugHud>) {
    if keys.just_pressed(KeyCode::F3) { d.on = !d.on; }
}

/// The moby nearest the camera's forward line: within 40 and 10° of it, the smallest angle first (Ratchet's own moby
/// left out). (id, distance).
fn looked_at(p: &crate::gameplay::Play) -> Option<(usize, f32)> {
    let cam = p.game.camera.out;
    let (eye, fwd) = (cam.pos_f32(), cam.rows_f32()[0]);
    let mut best: Option<(usize, f32, f32)> = None;
    for (id, m) in p.game.mobys.mobys.iter().enumerate() {
        if m.is_deleted() || id == p.hero_id { continue; }
        let d = [m.position[0] - eye[0], m.position[1] - eye[1], m.position[2] - eye[2]];
        let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if !(0.5..=40.0).contains(&dist) { continue; }
        let cos = (d[0] * fwd[0] + d[1] * fwd[1] + d[2] * fwd[2]) / dist;
        if cos < 10f32.to_radians().cos() { continue; }
        if best.is_none_or(|b| cos > b.2) { best = Some((id, dist, cos)); }
    }
    best.map(|(id, d, _)| (id, d))
}

/// The overlay's lines (ASCII: the game's fonts).
pub fn lines(p: &crate::gameplay::Play) -> Vec<String> {
    let h = &p.game.hero;
    let pos = h.pos.map(|v| v.to_f32());
    let vel = h.vel.map(|v| v.to_f32());
    let yaw = h.rot[2].to_f32();
    let moby = |id: usize| p.game.mobys.mobys.get(id).map_or_else(|| format!("{id}"), |m| format!("{id} class {} uid {} state {:#x}", m.o_class, m.spawn_id, m.state));
    let cam = p.game.camera.out;
    let cp = cam.pos_f32();
    let mut out = vec![
        format!("LEVEL {}  TICK {}  GAME MODE {}", p.svc.level, p.game.counter, p.svc.game_mode),
        format!("POS {:.2} {:.2} {:.2}", pos[0], pos[1], pos[2]),
        format!("YAW {:.3} RAD  {:.1} DEG", yaw, yaw.to_degrees()),
        format!("STATE {:#x}  GROUP {:#x}  BODY {}  AIR {}", h.state, h.group, h.mode, h.air_ticks),
        format!("VEL {:.3} {:.3} {:.3}", vel[0], vel[1], vel[2]),
        format!("GROUND {}", h.ground_moby.map_or_else(|| "world".to_string(), moby)),
        format!("CAM {:.2} {:.2} {:.2}  YAW {:.3}  PITCH {:.3}", cp[0], cp[1], cp[2], cam.euler[2].to_f32(), cam.euler[1].to_f32()),
        format!("LOOK {}", looked_at(p).map_or_else(|| "-".to_string(), |(id, d)| format!("{} dist {d:.1}", moby(id)))),
    ];
    if let Some(s) = p.svc.cinematic.scene.as_ref() { out.push(format!("SCENE {}  TICK {}", s.id, s.tick)); }
    out
}
