//! **Port-only: frames between two logic ticks.** The game's logic ticks at 60 Hz and the port keeps that tick whatever
//! the frame rate (crate::frame_pace; nothing in the engine is written for another rate, so a faster tick would speed
//! up physics, animation and every timer). At a frame rate above 60 most frames therefore run no tick, and drawing the
//! state again would show the same picture twice. This module draws those frames from poses *between* the last two
//! ticks instead, the usual fixed-timestep rule: with `alpha` = the part of a tick that has passed since the last one
//! ran, a frame shows `lerp(previous tick, last tick, alpha)`. The picture is thereby one tick (1/60 s) behind the
//! logic, and moves smoothly at any frame rate.
//!
//! What is blended (everything the player sees move with the world):
//! * **The camera** ([`interp_camera`], `Update`, after the game camera has been written in `RunFixedMainLoop`): the
//!   main camera's `Transform`. The true transform is put back before the next fixed loop ([`restore_camera`]), so the
//!   logic reads exactly what it did at 60 Hz.
//! * **Moby draw data**: the instance records (model matrix) and the joint palettes of every moby that is placed or
//!   posed on the CPU and uploaded as a storage buffer: the gameplay instances, Ratchet and the dynamic slots, attached
//!   items, afterimages, scene actors and the ship ([`set_buffer`], used where those are uploaded). The buffers keep
//!   their last two ticks; the model matrix is blended with its axis lengths kept, a palette matrix component-wise.
//!   A record that is new, or that moved farther than [`SNAP_DISTANCE`] in one tick (a spawn, a teleport, a camera cut
//!   in a scene), and a joint that appears or disappears (to or from the identity), are shown at once.
//!
//! * **What is rebuilt every frame from the logic's latest state** keeps its last two ticks in a [`Pair`] and is blended
//!   where it is built: the particles' positions (crate::particle_render; a slot whose particle was born since the
//!   tick before is shown where it is), the shadow volumes' vertices (crate::shadow_render) and the HUD's primitives
//!   (crate::hud_render: positions and colours of the primitives that are the same sprite in both ticks, which
//!   includes the crosshair and target markers).
//!
//! Not blended (drawn as of the last tick, at 60 Hz steps): the effect-callback quads (lasers, rings, the bomb
//! reticle: crate::fx_draw), water and sea animation, shrub sway, sprite animation frames (a HUD sprite that changes
//! its picture between ticks changes at the tick) and the menus.
//!
//! Inactive at 60 (`FrameRate::Fps60`) and in frame-exact runs ([`active`] is false): [`set_buffer`] is then exactly
//! [`crate::asset_write::set_buffer`], so the picture is what it always was.

use crate::asset_write::{buffer_is, set_buffer_id};
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{LazyLock, Mutex};

/// A record or a camera that moved farther than this (game units) in one tick is not blended: it was placed, not moved.
pub const SNAP_DISTANCE: f32 = 12.0;

/// One `MobyInst` record (moby.wgsl), whose first 64 bytes are the model matrix.
const RECORD: usize = 208;
const MODEL: usize = 64;
/// One palette matrix: 16 f32.
const MATRIX: usize = 64;

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Ticks scheduled so far (the tick index of the state the logic holds now).
static TICK: AtomicU64 = AtomicU64::new(0);
/// Frames started so far.
static FRAME: AtomicU64 = AtomicU64::new(0);
/// The part of a tick that has passed since the last one ran, as f32 bits.
static ALPHA: AtomicU32 = AtomicU32::new(0x3f80_0000);

/// Whether frames are blended between ticks.
pub fn active() -> bool { ACTIVE.load(Relaxed) }
pub fn tick_index() -> u64 { TICK.load(Relaxed) }
fn frame_index() -> u64 { FRAME.load(Relaxed) }
/// The part of a tick that has passed since the last one ran (the blend factor of this frame's picture).
pub fn alpha() -> f32 { f32::from_bits(ALPHA.load(Relaxed)) }

/// A value's state at the last two ticks, for what is rebuilt on the CPU every frame from the logic's latest state
/// (particles, shadow volumes, the HUD's primitives). [`Pair::feed`] it that latest state every frame; [`Pair::view`]
/// gives the tick before's, the latest and the blend factor. A value that did not change in a tick is the same in both.
#[derive(Default)]
pub struct Pair<T> {
    prev: T,
    curr: T,
    tick: u64,
}

impl<T> Pair<T> {
    /// This frame's state of the logic (as of the last tick that ran). Only while [`active`].
    pub fn feed(&mut self, state: T) {
        let now = tick_index();
        if now > self.tick {
            self.prev = std::mem::replace(&mut self.curr, state);
            self.tick = now;
        } else {
            self.curr = state;
        }
    }

    /// (the tick before's state, the last tick's, the blend factor).
    pub fn view(&self) -> (&T, &T, f32) { (&self.prev, &self.curr, alpha()) }
}

/// `a` to `b` by `t`, unless they are farther apart than [`SNAP_DISTANCE`] (then `b`: it was placed, not moved).
pub fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let d: f32 = (0..3).map(|k| (b[k] - a[k]).powi(2)).sum();
    if d > SNAP_DISTANCE * SNAP_DISTANCE { return b; }
    std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t)
}

/// Starts a frame (crate::frame_pace, in `First`): the ticks it will run, the part of a tick left over after them
/// (the blend factor of the picture), and whether blending is on.
pub fn begin_frame(ticks: u32, alpha: f32, on: bool) {
    TICK.fetch_add(ticks as u64, Relaxed);
    FRAME.fetch_add(1, Relaxed);
    ALPHA.store(alpha.clamp(0.0, 1.0).to_bits(), Relaxed);
    ACTIVE.store(on, Relaxed);
}

/// What a buffer holds, for blending its contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 208-byte `MobyInst` records: the model matrix is blended, the rest (lights, palette base, mode) is the last tick's.
    Record,
    /// 64-byte joint matrices.
    Palette,
}

/// A buffer's last two ticks.
struct Slot {
    kind: Kind,
    /// The buffer as of the tick before `tick`'s.
    prev: Vec<u8>,
    /// The buffer as of the last write, which was tick `tick`.
    curr: Vec<u8>,
    tick: u64,
    /// What was last shown.
    out: Vec<u8>,
    /// The frame it was shown for.
    shown: u64,
}

impl Slot {
    fn new(kind: Kind, bytes: &[u8], tick: u64) -> Self {
        Slot { kind, prev: bytes.to_vec(), curr: bytes.to_vec(), tick, out: bytes.to_vec(), shown: u64::MAX }
    }

    /// The buffer's contents as of tick `now`. A buffer is only written when it changes, so a write for a later tick
    /// makes the old contents the tick before's; a second write in the same tick replaces the contents, the tick before
    /// is kept.
    fn put(&mut self, bytes: &[u8], now: u64) {
        if self.curr.len() != bytes.len() {
            *self = Slot::new(self.kind, bytes, now);
        } else if now > self.tick {
            std::mem::swap(&mut self.prev, &mut self.curr);
            self.curr.copy_from_slice(bytes);
            self.tick = now;
        } else {
            self.curr.copy_from_slice(bytes);
        }
    }

    /// `out` for frame `frame`: blended while the last write is of the tick the logic is at, else (nothing changed
    /// in the last tick) the contents as they are.
    fn show(&mut self, now: u64, alpha: f32, frame: u64) {
        self.shown = frame;
        let a = if self.tick == now { alpha } else { 1.0 };
        self.out.resize(self.curr.len(), 0);
        self.out.copy_from_slice(&self.curr);
        if a >= 1.0 || self.prev.len() != self.curr.len() { return; }
        match self.kind {
            Kind::Palette => {
                for ((p, c), o) in self.prev.chunks_exact(MATRIX).zip(self.curr.chunks_exact(MATRIX)).zip(self.out.chunks_exact_mut(MATRIX)) {
                    blend_matrix(p, c, o, a);
                }
            }
            Kind::Record => {
                for ((p, c), o) in self.prev.chunks_exact(RECORD).zip(self.curr.chunks_exact(RECORD)).zip(self.out.chunks_exact_mut(RECORD)) {
                    blend_model(&p[..MODEL], &c[..MODEL], &mut o[..MODEL], a);
                }
            }
        }
    }
}

static SLOTS: LazyLock<Mutex<HashMap<AssetId<ShaderBuffer>, Slot>>> = LazyLock::new(Default::default);

fn slots() -> std::sync::MutexGuard<'static, HashMap<AssetId<ShaderBuffer>, Slot>> { SLOTS.lock().unwrap_or_else(|e| e.into_inner()) }

/// The f32 at index `i` of a little-endian byte slice.
fn f(b: &[u8], i: usize) -> f32 { f32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]) }

fn put_f32s(o: &mut [u8], v: &[f32]) {
    for (dst, x) in o.chunks_exact_mut(4).zip(v) { dst.copy_from_slice(&x.to_le_bytes()); }
}

fn is_identity(m: &[u8]) -> bool { (0..16).all(|i| f(m, i) == if i % 5 == 0 { 1.0 } else { 0.0 }) }

/// One joint matrix: component-wise, unless it did not move, or it comes from / goes to the identity (the joint is
/// not posed on one side: show the pose).
fn blend_matrix(p: &[u8], c: &[u8], o: &mut [u8], a: f32) {
    if p == c || is_identity(p) || is_identity(c) { return; }
    let v: [f32; 16] = std::array::from_fn(|i| f(p, i) + (f(c, i) - f(p, i)) * a);
    put_f32s(o, &v);
}

/// One model matrix (16 f32, column-major: axes 0..3, translation): translation and axes blended, each axis keeping
/// the length it blends to (a plain blend of two rotations shrinks the axes in between). Not blended when it did not
/// change, when either side is not placed (all zero) or when the move is longer than [`SNAP_DISTANCE`].
fn blend_model(p: &[u8], c: &[u8], o: &mut [u8], a: f32) {
    if p == c { return; }
    let pm: [f32; 16] = std::array::from_fn(|i| f(p, i));
    let cm: [f32; 16] = std::array::from_fn(|i| f(c, i));
    if pm.iter().all(|&v| v == 0.0) || cm.iter().all(|&v| v == 0.0) { return; }
    let d = Vec3::new(pm[12] - cm[12], pm[13] - cm[13], pm[14] - cm[14]);
    if d.length_squared() > SNAP_DISTANCE * SNAP_DISTANCE { return; }
    let mut r: [f32; 16] = std::array::from_fn(|i| pm[i] + (cm[i] - pm[i]) * a);
    for col in 0..3 {
        let at = |m: &[f32; 16]| Vec3::new(m[4 * col], m[4 * col + 1], m[4 * col + 2]);
        let (pa, ca, v) = (at(&pm), at(&cm), at(&r));
        let want = pa.length() + (ca.length() - pa.length()) * a;
        let len = v.length();
        if len > 1e-6 && want > 1e-6 {
            let v = v * (want / len);
            (r[4 * col], r[4 * col + 1], r[4 * col + 2]) = (v.x, v.y, v.z);
        }
    }
    put_f32s(o, &r);
}

/// [`crate::asset_write::set_buffer`] for a buffer of blended poses: with blending on, `bytes` become the buffer's
/// contents as of this tick and the buffer is set to what this frame shows (between the last two ticks); with it off,
/// exactly `set_buffer`. Call it where the buffer is uploaded, as often as that is (a buffer that is written only when
/// it changes is fine: it keeps its last two states). True when the buffer's bytes changed.
pub fn set_buffer(buffers: &mut Assets<ShaderBuffer>, h: &Handle<ShaderBuffer>, bytes: &[u8], kind: Kind) -> bool {
    if !active() { return crate::asset_write::set_buffer(buffers, h, bytes); }
    let (now, a, frame, id) = (tick_index(), alpha(), frame_index(), h.id());
    let mut reg = slots();
    let slot = reg.entry(id).or_insert_with(|| Slot::new(kind, bytes, now));
    slot.put(bytes, now);
    slot.show(now, a, frame);
    set_buffer_id(buffers, id, &slot.out)
}

/// Frames the buffers that were not written this frame (their tick's blend moves on with `alpha`), forgets those whose
/// asset is gone, and, when blending was switched off, puts every buffer back to its last tick's contents.
fn blend_buffers(mut buffers: ResMut<Assets<ShaderBuffer>>) {
    let mut reg = slots();
    if reg.is_empty() { return; }
    if !active() {
        for (id, s) in reg.drain() { set_buffer_id(&mut buffers, id, &s.curr); }
        return;
    }
    let (now, a, frame) = (tick_index(), alpha(), frame_index());
    reg.retain(|id, _| buffers.contains(*id));
    for (id, s) in reg.iter_mut() {
        if s.shown == frame { continue; }
        s.show(now, a, frame);
        if !buffer_is(&buffers, *id, &s.out) { set_buffer_id(&mut buffers, *id, &s.out); }
    }
}

/// A camera's last two ticks, and the transform the logic wrote (put back by [`restore_camera`]).
#[derive(Component)]
struct CamInterp {
    prev: Transform,
    curr: Transform,
    tick: u64,
    truth: Option<Transform>,
}

fn lerp_transform(a: &Transform, b: &Transform, t: f32) -> Transform {
    Transform { translation: a.translation.lerp(b.translation, t), rotation: a.rotation.slerp(b.rotation, t), scale: a.scale.lerp(b.scale, t) }
}

/// A camera that moved farther than [`SNAP_DISTANCE`], or turned by more than about 75°, in one tick was cut, not moved.
fn jumped(a: &Transform, b: &Transform) -> bool {
    a.translation.distance_squared(b.translation) > SNAP_DISTANCE * SNAP_DISTANCE || a.rotation.dot(b.rotation).abs() < 0.8
}

/// The main camera between its last two ticks. Only while the game drives the view: the free camera moves with the
/// frame (it is written every frame, so it is shown as it is).
fn interp_camera(
    source: Option<Res<crate::game_camera::CameraSource>>,
    mut cams: Query<(Entity, &mut Transform, Option<&mut CamInterp>), With<crate::fly_cam::FlyCam>>,
    mut commands: Commands,
) {
    if !active() || source.is_some_and(|s| *s == crate::game_camera::CameraSource::Fly) { return; }
    let (now, a) = (tick_index(), alpha());
    for (e, mut tf, st) in &mut cams {
        let t = *tf;
        let Some(mut st) = st else {
            commands.entity(e).insert(CamInterp { prev: t, curr: t, tick: now, truth: Some(t) });
            continue;
        };
        if now > st.tick {
            st.prev = st.curr;
            st.curr = t;
            st.tick = now;
        } else if st.curr != t {
            // Changed without a tick (a scene's camera, a switch of source): shown as it is.
            st.prev = t;
            st.curr = t;
        }
        if jumped(&st.prev, &st.curr) { st.prev = st.curr; }
        let shown = if st.tick == now { lerp_transform(&st.prev, &st.curr, a) } else { st.curr };
        st.truth = Some(t);
        if *tf != shown { *tf = shown; }
    }
}

/// Puts the logic's own camera transform back before the fixed loop (the logic and every system in it read the camera
/// as at 60 Hz, not the blended one).
fn restore_camera(mut cams: Query<(&mut Transform, &mut CamInterp)>) {
    for (mut tf, mut st) in &mut cams {
        if let Some(t) = st.truth.take() {
            if *tf != t { *tf = t; }
        }
    }
}

pub struct InterpPlugin;

impl Plugin for InterpPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(RunFixedMainLoop, restore_camera.in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop))
            .add_systems(Update, interp_camera)
            .add_systems(PostUpdate, blend_buffers.before(bevy::asset::AssetEventSystems));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(v: &[f32]) -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() }
    fn floats(b: &[u8]) -> Vec<f32> { (0..b.len() / 4).map(|i| f(b, i)).collect() }

    /// A model matrix with `scale` on the axes and `x` as translation x.
    fn model(scale: f32, x: f32) -> Vec<u8> {
        bytes(&[scale, 0.0, 0.0, 0.0, 0.0, scale, 0.0, 0.0, 0.0, 0.0, scale, 0.0, x, 0.0, 0.0, 1.0])
    }

    #[test]
    fn a_slot_keeps_its_last_two_ticks() {
        let mut s = Slot::new(Kind::Record, &model(1.0, 0.0), 5);
        // Tick 6 moves it to x = 2; the blend runs from the old place.
        let mut rec = model(1.0, 2.0);
        rec.resize(RECORD, 0);
        let mut old = model(1.0, 0.0);
        old.resize(RECORD, 0);
        let mut s2 = Slot::new(Kind::Record, &old, 5);
        s2.put(&rec, 6);
        s2.show(6, 0.25, 1);
        assert!((f(&s2.out, 12) - 0.5).abs() < 1e-6);
        s2.show(6, 1.0, 2);
        assert_eq!(f(&s2.out, 12), 2.0);
        // Tick 7 changes nothing (no write): the tick's blend is over, the contents stay.
        s2.show(7, 0.5, 3);
        assert_eq!(f(&s2.out, 12), 2.0);
        // Writes of a size the slot does not have start it again.
        s.put(&[0u8; 16], 9);
        assert_eq!((s.prev.len(), s.curr.len(), s.tick), (16, 16, 9));
    }

    #[test]
    fn a_second_write_in_a_tick_keeps_the_tick_before() {
        let mut old = model(1.0, 0.0);
        old.resize(RECORD, 0);
        let mut s = Slot::new(Kind::Record, &old, 1);
        let mut a = model(1.0, 4.0);
        a.resize(RECORD, 0);
        let mut b = model(1.0, 6.0);
        b.resize(RECORD, 0);
        s.put(&a, 2);
        s.put(&b, 2);
        s.show(2, 0.5, 1);
        assert!((f(&s.out, 12) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn models_keep_their_axis_lengths_and_snap_on_teleports() {
        // 90° about z between two ticks, scale 2: the middle keeps the length 2 (a plain blend would give √2).
        let p = bytes(&[2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        let c = bytes(&[0.0, 2.0, 0.0, 0.0, -2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        let mut o = c.clone();
        blend_model(&p, &c, &mut o, 0.5);
        let v = floats(&o);
        assert!((Vec3::new(v[0], v[1], v[2]).length() - 2.0).abs() < 1e-5);
        // Farther than SNAP_DISTANCE: the new place at once. Never placed (all zero): the same.
        let (far, mut o) = (model(1.0, 100.0), model(1.0, 100.0));
        blend_model(&model(1.0, 0.0), &far, &mut o, 0.5);
        assert_eq!(o, far);
        let mut o = model(1.0, 3.0);
        blend_model(&bytes(&[0.0; 16]), &model(1.0, 3.0), &mut o, 0.5);
        assert_eq!(o, model(1.0, 3.0));
    }

    #[test]
    fn joints_blend_unless_they_appear_or_vanish() {
        let ident = bytes(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        let a = bytes(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 2.0, 0.0, 0.0, 1.0]);
        let b = bytes(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 4.0, 0.0, 0.0, 1.0]);
        let mut o = b.clone();
        blend_matrix(&a, &b, &mut o, 0.5);
        assert_eq!(f(&o, 12), 3.0);
        let mut o = b.clone();
        blend_matrix(&ident, &b, &mut o, 0.5);
        assert_eq!(o, b, "from the identity: the pose");
        let mut o = ident.clone();
        blend_matrix(&b, &ident, &mut o, 0.5);
        assert_eq!(o, ident, "to the identity: the identity");
    }

    #[test]
    fn cameras_blend_and_cut() {
        let a = Transform::from_xyz(0.0, 0.0, 0.0);
        let b = Transform::from_xyz(2.0, 0.0, 0.0);
        assert_eq!(lerp_transform(&a, &b, 0.5).translation, Vec3::new(1.0, 0.0, 0.0));
        assert!(!jumped(&a, &b));
        assert!(jumped(&a, &Transform::from_xyz(50.0, 0.0, 0.0)));
        assert!(jumped(&a, &Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI * 0.9))));
    }
}
