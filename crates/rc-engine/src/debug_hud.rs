//! The inspect overlay (dev): with `RC_DEBUG_HUD=1` (`show` / `mobys` / `sound`: shown from the start on that page), F3 shows or hides a column of live
//! data over the HUD, F4 cycles its pages, F8 makes a repro line. F9's dump (crate::gameplay) prints the player page.
//!
//! * **Player**: the level, tick and game mode; Ratchet's position, facing, state, speed and ground; the camera; the moby
//!   the camera looks at; the scene.
//! * **Mobys**: a label over every moby within [`LABEL_RANGE`] of Ratchet (id, class, state) and, for the one the camera
//!   looks at, its uid, mode, update / draw distances, health record and first pvar words.
//! * **Sound**: every busy sound slot (owner, sound, loop, volume, slot state, sound instance).
//! * **F8**: `RC_LEVEL=… RC_SCENE=0 RC_HERO_AT=x,y,z,yaw RC_GIVE_ITEMS=…` (the owned items, the hand item last so it is
//!   requested into the hand): printed, copied to the clipboard where a copier exists, and shown for a few seconds.
//!
//! Without the switch nothing is added.

use bevy::prelude::*;

/// The overlay's state.
#[derive(Resource, Default)]
pub struct DebugHud {
    pub on: bool,
    pub page: Page,
    /// The last repro line and the frame it stops showing at.
    pub note: Option<(String, u64)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Player,
    Mobys,
    Sound,
}

impl Page {
    fn next(self) -> Page {
        match self {
            Page::Player => Page::Mobys,
            Page::Mobys => Page::Sound,
            Page::Sound => Page::Player,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Page::Player => "PLAYER",
            Page::Mobys => "MOBYS",
            Page::Sound => "SOUND",
        }
    }
}

/// Mobys this close to Ratchet get a label on the mobys page.
pub const LABEL_RANGE: f32 = 15.0;

pub struct DebugHudPlugin;

impl Plugin for DebugHudPlugin {
    fn build(&self, app: &mut App) {
        match std::env::var("RC_DEBUG_HUD").as_deref().map(str::trim) {
            Ok("1") => { app.init_resource::<DebugHud>().add_systems(Update, keys); }
            // Shown from the start on a page (headless captures, which cannot press F3 / F4).
            Ok(v @ ("show" | "mobys" | "sound")) => {
                let page = match v { "mobys" => Page::Mobys, "sound" => Page::Sound, _ => Page::Player };
                app.insert_resource(DebugHud { on: true, page, ..default() }).add_systems(Update, keys);
            }
            _ => {}
        }
    }
}

fn keys(keys: Res<ButtonInput<KeyCode>>, mut d: ResMut<DebugHud>, play: Option<Res<crate::gameplay::Play>>, frame: Res<crate::determinism::FrameNumber>) {
    if keys.just_pressed(KeyCode::F3) { d.on = !d.on; }
    if keys.just_pressed(KeyCode::F4) { d.page = d.page.next(); }
    if keys.just_pressed(KeyCode::F8) {
        let Some(p) = play else { return };
        let line = repro_line(&p);
        println!("repro: {line}");
        let copied = copy_to_clipboard(&line);
        d.note = Some((if copied { format!("COPIED: {line}") } else { line }, frame.0 + 300));
    }
}

/// The repro line for this moment (module doc).
pub fn repro_line(p: &crate::gameplay::Play) -> String {
    let h = &p.game.hero;
    let pos = h.pos.map(|v| v.to_f32());
    let hand = h.items.slot.id;
    let mut items: Vec<usize> = (0..h.owned.0.len()).filter(|&i| h.owned.has(i) && i as i32 != hand).collect();
    if hand > 0 && h.owned.has(hand as usize) { items.push(hand as usize); }
    let items: Vec<String> = items.iter().map(|i| i.to_string()).collect();
    let mut s = format!("RC_LEVEL={} RC_SCENE=0 RC_HERO_AT={:.2},{:.2},{:.2},{:.3}", p.svc.level, pos[0], pos[1], pos[2], h.rot[2].to_f32());
    if !items.is_empty() { s.push_str(&format!(" RC_GIVE_ITEMS={}", items.join(","))); }
    s
}

/// The system's clipboard copier (`pbcopy`, `clip`, `wl-copy` or `xclip`); false when none ran.
fn copy_to_clipboard(s: &str) -> bool {
    use std::io::Write;
    let tools: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(windows) {
        &[("clip", &[])]
    } else {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])]
    };
    for (cmd, args) in tools {
        let Ok(mut child) = std::process::Command::new(cmd).args(*args).stdin(std::process::Stdio::piped()).spawn() else { continue };
        let ok = child.stdin.take().is_some_and(|mut i| i.write_all(s.as_bytes()).is_ok());
        if ok && child.wait().is_ok_and(|st| st.success()) { return true; }
    }
    false
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

fn moby_text(p: &crate::gameplay::Play, id: usize) -> String {
    p.game.mobys.mobys.get(id).map_or_else(|| format!("{id}"), |m| format!("{id} class {} uid {} state {:#x}", m.o_class, m.spawn_id, m.state))
}

/// The player page (and F9's dump).
pub fn lines(p: &crate::gameplay::Play) -> Vec<String> {
    let h = &p.game.hero;
    let pos = h.pos.map(|v| v.to_f32());
    let vel = h.vel.map(|v| v.to_f32());
    let yaw = h.rot[2].to_f32();
    let d = h.disp.map(|v| v.to_f32());
    let moved = [(d[0] * d[0] + d[1] * d[1]).sqrt() * 60.0, (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() * 60.0];
    let cam = p.game.camera.out;
    let cp = cam.pos_f32();
    let mut out = vec![
        format!("LEVEL {}  TICK {}  GAME MODE {}", p.svc.level, p.game.counter, p.svc.game_mode),
        format!("POS {:.2} {:.2} {:.2}", pos[0], pos[1], pos[2]),
        format!("YAW {:.3} RAD  {:.1} DEG", yaw, yaw.to_degrees()),
        format!("STATE {:#x}  GROUP {:#x}  BODY {}  AIR {}", h.state, h.group, h.mode, h.air_ticks),
        format!("VEL {:.3} {:.3} {:.3}", vel[0], vel[1], vel[2]),
        // 0x13f4e4 / 0x13f4e0 (u/tick), and what the tick's move did (0x13f450, the platform's carry included).
        format!("SPEED {:.3}/TICK {:.2} U/S  TARGET {:.2} U/S", h.speed.to_f32(), h.speed.to_f32() * 60.0, h.target_speed.to_f32() * 60.0),
        format!("MOVED {:.2} U/S  3D {:.2} U/S", moved[0], moved[1]),
        format!("GROUND {}", h.ground_moby.map_or_else(|| "world".to_string(), |id| moby_text(p, id))),
        format!("CAM {:.2} {:.2} {:.2}  YAW {:.3}  PITCH {:.3}", cp[0], cp[1], cp[2], cam.euler[2].to_f32(), cam.euler[1].to_f32()),
        format!("LOOK {}", looked_at(p).map_or_else(|| "-".to_string(), |(id, d)| format!("{} dist {d:.1}", moby_text(p, id)))),
    ];
    if let Some(s) = p.svc.cinematic.scene.as_ref() { out.push(format!("SCENE {}  TICK {}", s.id, s.tick)); }
    out
}

/// The mobys page's column: the count labelled and the looked-at moby's details.
pub fn moby_lines(p: &crate::gameplay::Play, labelled: usize) -> Vec<String> {
    let mut out = vec![format!("{labelled} MOBYS WITHIN {LABEL_RANGE:.0}  (id class state)")];
    let Some((id, d)) = looked_at(p) else {
        out.push("LOOK -".into());
        return out;
    };
    let m = &p.game.mobys.mobys[id];
    out.push(format!("LOOK {} dist {d:.1}", moby_text(p, id)));
    out.push(format!("POS {:.2} {:.2} {:.2}  ROT Z {:.3}  SCALE {:.3}", m.position[0], m.position[1], m.position[2], m.rotation[2], m.scale));
    out.push(format!("MODE {:#06x}  CMD {:#x}  COLL {}  UPDATE {}  DRAW {}  VISIBLE {}", m.mode, m.cmd, m.has_collision, m.update_dist, m.draw_dist, m.visible));
    if let Some(hp) = rc_game::targeting::record_health(m) { out.push(format!("HEALTH {hp:.2}")); }
    for (k, row) in m.pvars.chunks(4).take(16).collect::<Vec<_>>().chunks(4).enumerate() {
        let words: Vec<String> = row.iter().map(|c| format!("{:08x}", u32::from_le_bytes([c[0], c.get(1).copied().unwrap_or(0), c.get(2).copied().unwrap_or(0), c.get(3).copied().unwrap_or(0)]))).collect();
        out.push(format!("+{:02x} {}", k * 16, words.join(" ")));
    }
    out
}

/// The mobys page's labels: (screen x, y in the 512 × 416 frame, text) over every moby within [`LABEL_RANGE`] of
/// Ratchet in front of the camera, 1 above its position.
pub fn moby_labels(p: &crate::gameplay::Play) -> Vec<(f32, f32, String)> {
    let pos = p.game.hero.pos.map(|v| v.to_f32());
    let cam = p.game.camera.out;
    let (eye, rows) = (cam.pos_f32(), cam.rows_f32());
    let mut out = Vec::new();
    for (id, m) in p.game.mobys.mobys.iter().enumerate() {
        if m.is_deleted() || id == p.hero_id { continue; }
        let d = ((m.position[0] - pos[0]).powi(2) + (m.position[1] - pos[1]).powi(2) + (m.position[2] - pos[2]).powi(2)).sqrt();
        if d > LABEL_RANGE { continue; }
        let at = [m.position[0], m.position[1], m.position[2] + 1.0];
        let Some(c) = rc_game::targeting::project_tan(eye, rows, at, p.svc.view_tan_x) else { continue };
        out.push((c[0], c[1], format!("{id} c{} s{:x}", m.o_class, m.state)));
    }
    // Labels that would overlap one placed before (about 8 pixels a character, rows of 10) move down a row until free.
    out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.total_cmp(&b.0)));
    let mut placed: Vec<(f32, f32, f32)> = Vec::new();
    for l in &mut out {
        let w = 8.0 * l.2.len() as f32;
        while placed.iter().any(|&(x0, x1, y)| (l.1 - y).abs() < 10.0 && l.0 < x1 && x0 < l.0 + w) { l.1 += 10.0; }
        placed.push((l.0, l.0 + w, l.1));
    }
    out
}

/// The sound page: every busy slot.
pub fn sound_lines(a: &rc_game::audio::AudioSystem, p: &crate::gameplay::Play) -> Vec<String> {
    use rc_game::audio::voices::{flags, state};
    let mut out = Vec::new();
    for (k, s) in a.slots.slots.iter().enumerate() {
        if s.state == state::FREE { continue; }
        let owner = match s.owner {
            Some(o) if o.id as usize == p.hero_id => "Ratchet".to_string(),
            Some(o) => p.game.mobys.mobys.get(o.id as usize).map_or_else(|| format!("moby {}", o.id), |m| format!("{} c{}", o.id, m.o_class)),
            None => "-".to_string(),
        };
        let inst = if s.sndinst >= 0 { format!("  inst {}", s.sndinst) } else { String::new() };
        let lp = if s.flags & flags::LOOP != 0 { " loop" } else { "" };
        out.push(format!("{k:2}: {owner}  snd {}{lp}  vol {:#x}  st {}{inst}", s.class_index, s.vol, s.state));
    }
    if out.is_empty() { out.push("no sound slot busy".into()); }
    out.insert(0, format!("{} SOUND SLOTS BUSY", out.len() - usize::from(out[0].starts_with("no "))));
    out
}
