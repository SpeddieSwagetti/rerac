//! Hero model replacement: Lauma in place of Ratchet (moby class 0), through the game's own data structures.
//!
//! `RC_LAUMA=1` swaps class 0 of every level at load ([`inject`], called by `level_load::load_level` once the level's
//! textures and mobys exist). Nothing else in the engine knows: the replacement is an ordinary [`LevelMobyClass`] and
//! its textures are ordinary moby-table entries, so every system that draws or moves Ratchet takes her instead:
//!
//! * **Placement**: the hero's instance (gameplay, the pause and vendor 3D previews, scenes, afterimages) is class 0;
//!   it is positioned, rotated and scaled by the game exactly as before. Ratchet's header, skeleton, sequences,
//!   joint lists and collision stay (the game logic reads them), only the packets and the texture slots change.
//! * **Lighting**: her vertices carry the game's packed normals (azimuth / elevation bytes), so `moby.wgsl` lights
//!   her with the instance's light block like any moby; the per-vertex RGBA multipliers default to 0x80 (1.0).
//! * **Skinning (version 2 model, the default)**: her bones are mapped onto Ratchet's 111 joints by
//!   `tools/lauma_rig.py`, and the file carries each vertex's skin (up to three of his joints, weights summing to 256)
//!   and the positions his joints take in her T pose. His animations are rotations about his joints in the frame of
//!   his own T-pose bind pose, so the mesh is posed into a T pose first and his joints are moved to her proportions:
//!   [`skeleton_plan`] / [`apply_skeleton`] rewrite the class's rest translations and inverse-bind rows (the engine's
//!   copy, the class header's copy and everything made from them: worn items, snapshots, attachments), and
//!   [`rescale_translation_keys`] re-anchors his translation keys (the pelvis bob, face joints, the arm layers of
//!   classes 1 and 2) on her rest translations, scaled by how much longer or shorter her segment is. Rotation keys,
//!   scale keys, timing and every other sequence are untouched, so she walks, runs, jumps and fights on his clips.
//! * **Skinning (version 1 model, or `RC_LAUMA_SKIN=back|root|static`)**: every vertex rides one joint
//!   (`MobySkin::joint`) and she does not deform. The joint is the one his back attachment list (joint list 5:
//!   where Clank and the packs ride) resolves to, so she turns with his body in flips, bobs with his run and lowers
//!   in his crouch. `RC_LAUMA_SKIN=root` binds to the root joint instead, `RC_LAUMA_SKIN=static` to no joint at all
//!   (joint count 0: the identity). These leave his skeleton alone.
//! * **Data structure**: one packet of packed `i16` positions (in Ratchet's own class scale, so his animation
//!   translations still apply), 4.12 ST, the game's triangle list with texture slots 0..15, drawn as the high and
//!   the low LOD alike (a far hero must not vanish), no metal packets (his chrome would hang in the air) and no
//!   glow packets.
//!
//! The model is `assets/models/lauma.rcm`, written by `tools/lauma_convert.py` from the glTF binary (axes, textures
//! and alpha already in the form the engine takes). Her size is fitted to Ratchet's: his bind-pose height from his own
//! packets, feet on his feet.
//!
//! Environment: `RC_LAUMA=1` enables; `RC_LAUMA_MODEL=<path>` another file; `RC_LAUMA_SCALE=<f>` (default 1) scales her relative to
//! her baseline size, [`LAUMA_BASE`] times Ratchet's height (1 = Ratchet's height; up to about 2.5 holds, but cutscenes are placed for his height); `RC_LAUMA_YAW=<degrees>` (default 0) turns her about the vertical axis (the game's
//! model forward is +x; the converter faces her that way); `RC_LAUMA_SKIN=rig|back|root|static` as above (default
//! `rig` for a version 2 model, `back` for version 1). `RC_LAUMA_BOB=<f>` (default [`LAUMA_BOB`]) scales the pelvis's small
//! translation motion (the bob of a walk or run; big moves keep their size, scaled to her legs) and `RC_LAUMA_SQUASH=<f>` (default
//! [`LAUMA_SQUASH`]) how much of his squash and stretch scale keys she takes: 1 is his full motion, 0 none.
//! `RC_LAUMA_JIGGLE=<f>` (default 1, 0 to 2) is how much her cape, back train, hair and pendants swing on their spring strands
//! (0: rigid), `RC_LAUMA_JIGGLE_STIFF=<f>` (default 1) speeds the springs; `RC_LAUMA_FLOOR=0` turns the floor guard off,
//! `RC_LAUMA_FLOOR_SNAP=<metres>` (default 0.05) is how far above the floor a hovering foot is lowered onto it, and
//! `RC_LAUMA_PIN=neck|all|off` (default `neck`) is which joints may not stretch (see `rc_formats::moby_anim::FloorGuard`).
//! With `RC_MOBY_CPU_LIGHT` the class colours are baked at load before this runs, so Ratchet stays.
//!
//! `RC_LAUMA_DUMP=1` (with or without `RC_LAUMA`) writes `lauma_dump.txt` into the working directory once per
//! process: Ratchet's skeleton as the class data has it (the hierarchy, each joint's position in the rest pose, where
//! his own mesh weights put each joint, every joint list with its chain, every sequence's frame count). It is what
//! reshaping his skeleton to her proportions needs, and it only reads.

use crate::moby_render::LevelMobys;
use anyhow::{ensure, Context, Result};
use rc_formats::gadget::{self, RATCHET_O_CLASS};
use rc_formats::level::{LevelCore, TextureEntry};
use rc_formats::moby::{MobySkin, MobySubmesh, MobyTriangle, MobyVertex, MobyVertexKind};
use rc_formats::moby_anim::{FloorGuard, JiggleSetup, JiggleStrand, MobyAnimClass, MobySequence};
use rc_formats::texture::{LevelTexture, Texture, TextureSource, TextureTable};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Ratchet's root joint (`RC_LAUMA_SKIN=root`, and the fallback when the back joint cannot be read).
const ROOT_JOINT: u8 = 0;

/// The joint list the back items hang on (Clank, the packs: docs/formats/moby_rac1.md, `moby_attach`): its attachment
/// joint is part of Ratchet's body in every animation, so a vertex skinned to it moves with his flips, run and crouch.
const BACK_LIST: usize = 5;

/// The converted model (`tools/lauma_convert.py`): game axes (x forward, y left, z up), metres, her own size.
struct Asset {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    /// a, b, c, material (0..15).
    tris: Vec<[u32; 4]>,
    /// Material -> index into `textures`, 0xff for none.
    mat_tex: [u8; 16],
    textures: Vec<Texture>,
    /// Version 2 only: the retargeting block.
    rig: Option<RigData>,
}

/// The rig block of a version 2 model (`tools/lauma_rig.py`).
struct RigData {
    /// Per vertex: the skin on Ratchet's joints.
    skin: Vec<MobySkin>,
    /// Where each of Ratchet's joints sits in her T pose: game axes, the model's own frame (before the fit).
    joints: Vec<[f32; 3]>,
    /// Version 3: the spring strands (cape, train, hair, pendants), pivot and tip in the model's own frame.
    jiggle: Vec<JiggleStrand>,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.data.len()).context("the file is truncated")?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32> { Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap())) }

    fn u16(&mut self) -> Result<u16> { Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap())) }

    fn u8(&mut self) -> Result<u8> { Ok(self.take(1)?[0]) }

    fn floats<const N: usize>(&mut self, count: usize) -> Result<Vec<[f32; N]>> {
        let bytes = count.checked_mul(N * 4).context("the vertex count is absurd")?;
        let raw = self.take(bytes)?;
        Ok(raw.chunks_exact(N * 4).map(|c| std::array::from_fn(|i| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap()))).collect())
    }
}

fn parse(data: &[u8]) -> Result<Asset> {
    let mut r = Reader { data, at: 0 };
    ensure!(r.take(4)? == b"RCHM", "not a hero model file (magic)");
    let version = r.u32()?;
    ensure!((1..=3).contains(&version), "unsupported hero model version {version}");
    let (nv, nt, ntex) = (r.u32()? as usize, r.u32()? as usize, r.u32()? as usize);
    let mat_tex: [u8; 16] = r.take(16)?.try_into().unwrap();
    let pos = r.floats::<3>(nv)?;
    let nrm = r.floats::<3>(nv)?;
    let uv = r.floats::<2>(nv)?;
    let mut tris = Vec::new();
    for _ in 0..nt {
        tris.push([r.u32()?, r.u32()?, r.u32()?, r.u32()?]);
    }
    let mut textures = Vec::new();
    for _ in 0..ntex {
        let (width, height) = (r.u32()?, r.u32()?);
        let bytes = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4)).context("the texture size is absurd")?;
        textures.push(Texture { width, height, rgba: r.take(bytes)?.to_vec() });
    }
    ensure!(tris.iter().all(|t| t[..3].iter().all(|&i| (i as usize) < nv) && t[3] < 16), "a triangle is out of range");
    ensure!(mat_tex.iter().all(|&t| t == 0xff || (t as usize) < ntex), "a material names a missing texture");
    let rig = if version >= 2 {
        let nj = r.u32()? as usize;
        ensure!((1..=0x70).contains(&nj), "the rig has {nj} joints");
        let mut skin = Vec::with_capacity(nv);
        for _ in 0..nv {
            let joints = [r.u8()?, r.u8()?, r.u8()?];
            let count = r.u8()?;
            let weights = [r.u16()?, r.u16()?, r.u16()?];
            ensure!((1..=3).contains(&count), "a vertex has {count} skin influences");
            let used = count as usize;
            ensure!(weights[..used].iter().map(|&w| w as u32).sum::<u32>() == 256, "a vertex's skin weights do not sum to 256");
            ensure!(joints[..used].iter().all(|&j| (j as usize) < nj), "a vertex skins to a joint past the rig");
            skin.push(MobySkin { count, joints, weights });
        }
        let joints = r.floats::<3>(nj)?;
        let mut jiggle = Vec::new();
        if version >= 3 {
            let ns = r.u32()? as usize;
            ensure!(ns <= 32, "the rig has {ns} jiggle strands");
            for _ in 0..ns {
                let (joint, anchor, _pad) = (r.u8()?, r.u8()?, r.u16()?);
                ensure!((joint as usize) < nj && (anchor as usize) < nj, "a jiggle strand names a joint past the rig");
                let [freq, damp, max_deg] = r.floats::<3>(1)?[0];
                ensure!(freq > 0.0 && freq < 60.0 && (0.0..=2.0).contains(&damp) && max_deg > 0.0 && max_deg <= 90.0, "a jiggle strand's spring is out of range");
                let (pivot, tip) = (r.floats::<3>(1)?[0], r.floats::<3>(1)?[0]);
                jiggle.push(JiggleStrand { joint, anchor, freq, damp, max_deg, pivot, tip });
            }
        }
        Some(RigData { skin, joints, jiggle })
    } else {
        None
    };
    Ok(Asset { pos, nrm, uv, tris, mat_tex, textures, rig })
}

/// The model, read once per process. A missing or broken file leaves Ratchet in place.
fn asset() -> Option<&'static Asset> {
    static ASSET: OnceLock<Option<Asset>> = OnceLock::new();
    ASSET
        .get_or_init(|| {
            let path = std::env::var("RC_LAUMA_MODEL").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(crate::asset_dir()).join("models/lauma.rcm"));
            match std::fs::read(&path).context("reading the file").and_then(|d| parse(&d)) {
                Ok(a) => Some(a),
                Err(e) => {
                    eprintln!("hero model: {}: {e:#}; Ratchet stays (run tools/lauma_convert.py to make the file)", path.display());
                    None
                }
            }
        })
        .as_ref()
}

/// `RC_LAUMA=1`.
pub fn enabled() -> bool { std::env::var("RC_LAUMA").is_ok_and(|v| v.trim() == "1") }

/// Her baseline size in Ratchet's heights (headdress included): what `RC_LAUMA_SCALE=1` means. Larger sizes broke the
/// cutscenes (their actors are placed for his height), so the baseline is his own height and a bigger one is opt-in.
const LAUMA_BASE: f32 = 1.0;
/// The largest total size (in his heights) the packed positions hold, with a margin.
const LAUMA_MAX: f32 = 2.5;
/// Share of the pelvis's small translation motion she keeps by default (his bob of a walk or run). Motion beyond
/// [`BOB_KNEE`] (a crouch, sitting, a landing's dip) is not softened: her legs shorten by their own ratio there and the
/// pelvis has to follow them or her feet leave the ground.
const LAUMA_BOB: f32 = 0.6;
/// How far the pelvis may move off its rest height, in metres (his), before the bob factor stops applying.
const BOB_KNEE: f32 = 0.04;
/// Share of his squash and stretch she keeps by default.
const LAUMA_SQUASH: f32 = 0.4;

fn env_f32(name: &str, default: f32) -> f32 { std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default) }

/// `RC_LAUMA_DUMP=1`.
fn dump_requested() -> bool { std::env::var("RC_LAUMA_DUMP").is_ok_and(|v| v.trim() == "1") }

/// Ratchet's class block (the joint lists live in it) in the level's core data.
fn ratchet_blob<'a>(core: &LevelCore, core_data: &'a [u8]) -> Option<&'a [u8]> {
    let blk = core.blocks.iter().find(|b| b.name == "moby_class/0000")?;
    core_data.get(blk.offset..blk.offset + blk.size)
}

/// Ratchet's skeleton as text (`lauma_dump.txt`): one line per joint with its parent, its rest-pose position (the sum of
/// the rest translations up the chain, in metres), the translation row of its inverse bind matrix (metres, for the
/// cross-check), how much of his own mesh's skin weight it carries and where that weight is centred; then every joint
/// list (the root-to-joint chain and the second byte list) and every sequence's frame count.
fn dump_ratchet(blob: Option<&[u8]>, mobys: &LevelMobys, ci: usize, back: Option<u8>) -> Result<String> {
    use std::fmt::Write as _;
    let (lc, anim) = (&mobys.classes[ci], &mobys.anim[ci]);
    let unit = lc.class.header.scale / 1024.0;
    let n = anim.joint_count;
    let mut mass = vec![0.0f32; n];
    let mut sum = vec![[0.0f32; 3]; n];
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in lc.class.high_lod.iter().flat_map(|s| &s.vertices) {
        let p = [v.x as f32 * unit, v.y as f32 * unit, v.z as f32 * unit];
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
        for k in 0..(v.skin.count as usize).min(3) {
            let (j, w) = (v.skin.joints[k] as usize, v.skin.weights[k] as f32 / 256.0);
            if j < n {
                mass[j] += w;
                for a in 0..3 {
                    sum[j][a] += w * p[a];
                }
            }
        }
    }
    let rest_position = |j: usize| -> [f32; 3] {
        let mut p = [0.0f32; 3];
        let mut at = Some(j);
        for _ in 0..64 {
            let Some(k) = at else { break };
            if k >= n {
                break;
            }
            for a in 0..3 {
                p[a] += anim.rest[k][a] * unit;
            }
            at = anim.parent(k);
        }
        p
    };
    let mut s = String::new();
    writeln!(
        s,
        "# Ratchet class {RATCHET_O_CLASS} (RC_LAUMA_DUMP): {n} joints, class scale {:.4} (1 packed unit = {:.6} m), low-LOD joint count {}",
        lc.class.header.scale, unit, lc.class.header.low_lod_joint_count
    )?;
    writeln!(s, "# bind-pose box (m): x {:.3}..{:.3}  y {:.3}..{:.3}  z {:.3}..{:.3}", lo[0], hi[0], lo[1], hi[1], lo[2], hi[2])?;
    writeln!(s, "# back attachment joint (joint list {BACK_LIST}): {back:?}")?;
    writeln!(s, "# joint parent | rest position x y z | inverse-bind row 3 x y z | skin weight mass | weighted centroid x y z")?;
    for j in 0..n {
        let p = rest_position(j);
        let t = anim.skeleton[j][3];
        let m = mass[j];
        let c = if m > 0.0 { [sum[j][0] / m, sum[j][1] / m, sum[j][2] / m] } else { [0.0; 3] };
        let parent = anim.parent(j).map_or("-".to_string(), |q| q.to_string());
        writeln!(
            s,
            "{j:3} {parent:>4} | {:+.3} {:+.3} {:+.3} | {:+.3} {:+.3} {:+.3} | {m:7.2} | {:+.3} {:+.3} {:+.3}",
            p[0], p[1], p[2], t[0] * unit, t[1] * unit, t[2] * unit, c[0], c[1], c[2]
        )?;
    }
    if let Some(b) = blob {
        writeln!(s, "# joint lists: chain (root to the attachment joint) | second byte list")?;
        for l in 0..64 {
            if let Ok((chain, second)) = gadget::joint_list(b, &lc.class.header, l) {
                writeln!(s, "list {l:2}: {chain:?} | {second:?}")?;
            }
        }
    }
    writeln!(s, "# sequences (index: frame count)")?;
    for (i, q) in anim.sequences.iter().enumerate() {
        if let Some(q) = q {
            writeln!(s, "seq {i:3}: {} frames", q.header.frame_count)?;
        }
    }
    Ok(s)
}

/// The packed normal (azimuth, elevation) the game stores: `x = cos a cos e, y = sin a cos e, z = sin e`, both angles
/// in 2pi/256 steps (`rc_formats::moby::moby_normal` is the inverse).
fn normal_bytes(n: [f32; 3]) -> (u8, u8) {
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
    let (x, y, z) = (n[0] / len, n[1] / len, n[2] / len);
    let step = std::f32::consts::PI / 128.0;
    let azimuth = (y.atan2(x) / step).round() as i32;
    let elevation = (z.clamp(-1.0, 1.0).asin() / step).round() as i32;
    ((azimuth & 0xff) as u8, (elevation & 0xff) as u8)
}

/// The fitted placement: yaw about the vertical axis, uniform scale and the height shift, in world units.
struct Fit {
    cos: f32,
    sin: f32,
    scale: f32,
    dz: f32,
}

/// Her packet in Ratchet's packed units (`inv` = 1024 / class scale): positions fitted and rounded to `i16`
/// (returns how many vertices had to be clamped), normals as bytes, ST in 4.12, `skin_of(i)` the skin of vertex `i`.
fn build_submesh(a: &Asset, fit: &Fit, inv: f32, skin_of: &dyn Fn(usize) -> MobySkin) -> (MobySubmesh, usize) {
    let mut clamped = 0;
    let mut vertices = Vec::with_capacity(a.pos.len());
    for (i, ((p, n), t)) in a.pos.iter().zip(&a.nrm).zip(&a.uv).enumerate() {
        let (x, y) = (p[0] * fit.cos - p[1] * fit.sin, p[0] * fit.sin + p[1] * fit.cos);
        let packed = [x * fit.scale * inv, y * fit.scale * inv, (p[2] * fit.scale + fit.dz) * inv].map(f32::round);
        if packed.iter().any(|c| !(-32768.0f32..=32767.0).contains(c)) {
            clamped += 1;
        }
        let (normal_azimuth, normal_elevation) = normal_bytes([n[0] * fit.cos - n[1] * fit.sin, n[0] * fit.sin + n[1] * fit.cos, n[2]]);
        let skin = skin_of(i);
        vertices.push(MobyVertex {
            raw: [0; 8],
            normal_azimuth,
            normal_elevation,
            x: packed[0] as i16,
            y: packed[1] as i16,
            z: packed[2] as i16,
            id: (i & 0x1ff) as u16,
            kind: match skin.count {
                2 => MobyVertexKind::TwoWay,
                3 => MobyVertexKind::ThreeWay,
                _ => MobyVertexKind::Single,
            },
            duplicate: false,
            skin,
            st: [(t[0] * 4096.0).round() as i16, (t[1] * 4096.0).round() as i16],
        });
    }
    let triangles = a.tris.iter().map(|t| MobyTriangle { a: t[0], b: t[1], c: t[2], texture: t[3] as i32 }).collect();
    let sub = MobySubmesh {
        entry: Default::default(),
        vertex_table: Default::default(),
        metal_header: Default::default(),
        transfers: Vec::new(),
        duplicates: Vec::new(),
        raw_vertices: Vec::new(),
        // Empty: every vertex's RGBA multiplier is the default 0x80 (1.0), as on the disc.
        rgba_multipliers: Vec::new(),
        vertices,
        st: Vec::new(),
        index_bytes: Vec::new(),
        secret_indices: Vec::new(),
        texture_indices: Vec::new(),
        initial_texture: -1,
        unresolved_duplicates: 0,
        triangles,
        is_metal: false,
    };
    (sub, clamped)
}

/// The new skeleton, worked out before anything is changed (so a bad model leaves Ratchet whole).
#[derive(Clone)]
struct SkeletonPlan {
    /// New rest local translations, packed units.
    rest: Vec<[f32; 3]>,
    /// His rest local translations, packed units (what his translation keys are measured from).
    old_rest: Vec<[f32; 3]>,
    /// How far each joint's world position moves, packed units: the inverse-bind translation row follows.
    shift: Vec<[f32; 3]>,
    /// Scale of a translation key's offset from the rest translation, per joint: her segment over his.
    ratio: Vec<f32>,
    /// Share of his squash and stretch scale keys kept (1: all of it).
    squash: f32,
    /// Share of the pelvis's motion kept within `knee` of its rest (1: all of it), and the knee, packed units.
    bob: f32,
    knee: f32,
}

/// The plan in force, for the sequences that arrive after the load (the cutscene player swaps its own streamed
/// sequences into the actor's class animation: [`retarget_scene_sequence`]).
static PLAN: Mutex<Option<SkeletonPlan>> = Mutex::new(None);

fn length(v: [f32; 3]) -> f32 { (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() }

/// Each joint's position in the rest pose: the sum of the rest translations up the parent chain.
fn world_positions(anim: &MobyAnimClass, rest: &[[f32; 3]]) -> Vec<[f32; 3]> {
    (0..anim.joint_count)
        .map(|j| {
            let mut p = [0.0f32; 3];
            let mut at = Some(j);
            for _ in 0..64 {
                let Some(k) = at else { break };
                if k >= rest.len() {
                    break;
                }
                for a in 0..3 {
                    p[a] += rest[k][a];
                }
                at = anim.parent(k);
            }
            p
        })
        .collect()
}

/// Moves Ratchet's joints to `joints` (her T pose, the model's frame) through the same fit as the mesh. The root stays
/// where it is (the floor under the instance), and a joint with no parent stays too.
fn skeleton_plan(anim: &MobyAnimClass, joints: &[[f32; 3]], fit: &Fit, unit: f32, bob: f32, squash: f32) -> Result<SkeletonPlan> {
    let n = anim.joint_count;
    ensure!(joints.len() == n && anim.rest.len() == n && anim.skeleton.len() == n, "the model's rig has {} joints, Ratchet has {n}", joints.len());
    let old_world = world_positions(anim, &anim.rest);
    let new_world: Vec<[f32; 3]> = (0..n)
        .map(|j| {
            if anim.parent(j).is_none() {
                return old_world[j];
            }
            let p = joints[j];
            let (x, y) = (p[0] * fit.cos - p[1] * fit.sin, p[0] * fit.sin + p[1] * fit.cos);
            [x * fit.scale / unit, y * fit.scale / unit, (p[2] * fit.scale + fit.dz) / unit]
        })
        .collect();
    ensure!(new_world.iter().all(|p| p.iter().all(|c| c.is_finite() && c.abs() < 30000.0)), "a joint of the rig is out of the packed range");
    let rest: Vec<[f32; 3]> = (0..n)
        .map(|j| match anim.parent(j) {
            Some(p) if p < n => [0, 1, 2].map(|a| new_world[j][a] - new_world[p][a]),
            _ => anim.rest[j],
        })
        .collect();
    let shift = (0..n).map(|j| [0, 1, 2].map(|a| new_world[j][a] - old_world[j][a])).collect();
    // A joint whose rest offset is tiny keeps its keys as they are; the root follows the pelvis (a bob in height).
    let min_len = 0.02 / unit;
    let mut ratio: Vec<f32> = (0..n)
        .map(|j| {
            let old = length(anim.rest[j]);
            if old > min_len { (length(rest[j]) / old).clamp(0.25, 4.0) } else { 1.0 }
        })
        .collect();
    // The root's keys are the world motion of the actor (travel, stairs, a ramp in a cutscene), not a body proportion:
    // they stay as they are. (Scaling them sank her into slopes and slid her along them.)
    ratio[0] = 1.0;
    Ok(SkeletonPlan { rest, old_rest: anim.rest.clone(), shift, ratio, squash, bob, knee: BOB_KNEE / unit })
}

/// The inverse bind matrix of a joint that moved by `d` (packed units): its translation row is `-b * R` for the bind
/// position `b` and the matrix's rotation rows `R`, so it changes by `-d * R` (the rotation is the identity for nearly
/// every joint of his).
fn shift_inverse_bind(m: &mut [[f32; 4]; 4], d: [f32; 3]) {
    for a in 0..3 {
        let delta = d[0] * m[0][a] + d[1] * m[1][a] + d[2] * m[2][a];
        m[3][a] -= delta;
    }
}

/// The pelvis's offset from its rest, softened: within `knee` it is multiplied by `bob`, beyond it the extra is kept in
/// full (continuous at the knee). A walk's bob is small and gets calmer; a crouch is large and keeps its depth.
fn soft_bob(off: f32, bob: f32, knee: f32) -> f32 {
    let a = off.abs();
    if a <= knee { off * bob } else { off.signum() * (knee * bob + (a - knee)) }
}

/// One translation key of joint `j` re-anchored: a key replaces the joint's rest translation, so the new key is
/// `rest' + ratio * (key - rest)` (her rest, plus his offset from his rest scaled by her segment over his).
fn rescale_key(plan: &SkeletonPlan, j: usize, key: [i16; 3]) -> [i16; 3] {
    std::array::from_fn(|a| {
        let mut off = key[a] as f32 - plan.old_rest[j][a];
        if j == 1 {
            off = soft_bob(off, plan.bob, plan.knee);
        }
        let v = plan.rest[j][a] + plan.ratio[j] * off;
        v.round().clamp(-32768.0, 32767.0) as i16
    })
}

/// A scale key (4.12) pulled toward 1 by `1 - k`: his squash and stretch, softened. Keys far from 1 (0.4 to 2.5 is the
/// band) are not squash but a joint hidden or enlarged on purpose and stay as they are.
fn soften_scale(s: [u16; 3], k: f32) -> [u16; 3] {
    if s.iter().any(|&c| !(0.4..=2.5).contains(&(c as f32 / 4096.0))) {
        return s;
    }
    s.map(|c| (4096.0 + (c as f32 - 4096.0) * k).round().clamp(0.0, 65535.0) as u16)
}

/// Re-anchors every translation key of the sequences ([`rescale_key`]) and softens the scale keys ([`soften_scale`]),
/// in the frame's records and in the payload bytes the chain evaluation reads. Returns the translation records changed.
fn rescale_translation_keys(seqs: &mut [Option<MobySequence>], plan: &SkeletonPlan) -> usize {
    let n = plan.rest.len();
    let mut changed = 0;
    for seq in seqs.iter_mut().flatten() {
        for f in seq.frames.iter_mut() {
            let at = f.header.trans_offset as usize;
            for (i, r) in f.trans.iter_mut().enumerate() {
                let j = r.joint as usize;
                if (r.joint as i8) < 0 || j >= n {
                    continue;
                }
                let t = rescale_key(plan, j, r.trans);
                r.trans = t;
                let o = at + 8 * i;
                if o + 6 <= f.payload.len() {
                    for a in 0..3 {
                        f.payload[o + 2 * a..o + 2 * a + 2].copy_from_slice(&t[a].to_le_bytes());
                    }
                }
                changed += 1;
            }
            if plan.squash < 1.0 {
                let qb = f.header.quat_bytes as usize;
                for (i, r) in f.scales.iter_mut().enumerate() {
                    r.scale = soften_scale(r.scale, plan.squash);
                    let o = qb + 8 * i;
                    if o + 6 <= f.payload.len() {
                        for a in 0..3 {
                            f.payload[o + 2 * a..o + 2 * a + 2].copy_from_slice(&r.scale[a].to_le_bytes());
                        }
                    }
                }
            }
        }
    }
    changed
}

/// What the floor guard may do, in metres (the floor is the height of his soles; see [`FloorGuard`]).
struct GuardLimits {
    floor: f32,
    max_lift: f32,
    snap: f32,
    band: f32,
    floor_on: bool,
}

/// The floor guard ([`FloorGuard`]): up to eight points per foot, the lowest vertex of each slice of the foot along its
/// length, among the vertices whose strongest joint is one of the ankle, foot, ball or toe joints.
fn floor_guard(asset: &Asset, rig: &RigData, fit: &Fit, unit: f32, limits: GuardLimits, pins: Vec<u8>, anim: &MobyAnimClass) -> Option<FloorGuard> {
    const FEET: [u8; 8] = [94, 95, 96, 97, 100, 101, 102, 103];
    const BINS: usize = 8;
    let mut points = Vec::new();
    for left in [false, true] {
        let mut cand: Vec<(u8, [f32; 3])> = Vec::new();
        for (i, sk) in rig.skin.iter().enumerate() {
            let (mut best, mut best_w) = (0u8, 0u16);
            for k in 0..sk.count as usize {
                if sk.weights[k] > best_w {
                    (best, best_w) = (sk.joints[k], sk.weights[k]);
                }
            }
            if !FEET.contains(&best) || (best >= 98) != left {
                continue;
            }
            let p = asset.pos[i];
            let (x, y) = (p[0] * fit.cos - p[1] * fit.sin, p[0] * fit.sin + p[1] * fit.cos);
            cand.push((best, [x * fit.scale / unit, y * fit.scale / unit, (p[2] * fit.scale + fit.dz) / unit]));
        }
        if cand.is_empty() {
            continue;
        }
        let (lo, hi) = cand.iter().fold((f32::MAX, f32::MIN), |(lo, hi), c| (lo.min(c.1[0]), hi.max(c.1[0])));
        let mut bins: [Option<(u8, [f32; 3])>; BINS] = [None; BINS];
        for c in &cand {
            let b = (if hi > lo { ((c.1[0] - lo) / (hi - lo) * BINS as f32) as usize } else { 0 }).min(BINS - 1);
            if bins[b].map_or(true, |o| c.1[2] < o.1[2]) {
                bins[b] = Some(*c);
            }
        }
        points.extend(bins.into_iter().flatten());
    }
    if anim.rest.len() < 2 {
        return None;
    }
    let GuardLimits { floor, max_lift, snap, band, floor_on } = limits;
    Some(FloorGuard { joint_count: anim.joint_count, rest1: anim.rest[1].map(f32::to_bits), points, floor: floor / unit, max_lift: max_lift / unit, snap: snap / unit, band: band / unit, floor_on, pins })
}

/// A sequence the cutscene player streams into an actor's slot (its own frames, never part of the level's class data):
/// Ratchet's keys in it get the same treatment as his own sequences, or his neck and pelvis offsets would return in
/// every cutscene. Other classes are left alone.
pub(crate) fn retarget_scene_sequence(o_class: i32, seq: &mut Option<MobySequence>) {
    if o_class != RATCHET_O_CLASS {
        return;
    }
    let guard = PLAN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(plan) = guard.as_ref() {
        rescale_translation_keys(std::slice::from_mut(seq), plan);
    }
}

/// Writes the plan into class `ci` (the engine's animation data and the class header's skeleton) and into the
/// sequences that carry his joint numbers: his own, and the animation-only classes 1 and 2 (the arm layers).
/// Returns (translation keys rescaled, animation-only classes touched).
fn apply_skeleton(mobys: &mut LevelMobys, ci: usize, plan: &SkeletonPlan) -> (usize, usize) {
    let n = plan.rest.len();
    let anim = &mut mobys.anim[ci];
    for j in 0..n {
        anim.rest[j] = plan.rest[j];
        shift_inverse_bind(&mut anim.skeleton[j], plan.shift[j]);
    }
    let sk = &mut mobys.classes[ci].class.skeleton;
    for j in 0..n {
        if let Some(t) = sk.trans.get_mut(j) {
            t.vector = plan.rest[j];
        }
        if let Some(m) = sk.matrices.get_mut(j) {
            shift_inverse_bind(m, plan.shift[j]);
        }
    }
    let mut keys = rescale_translation_keys(&mut mobys.anim[ci].sequences, plan);
    let layers: Vec<usize> = mobys.classes.iter().enumerate().filter(|(_, c)| c.o_class == 1 || c.o_class == 2).map(|(i, _)| i).collect();
    for &k in &layers {
        keys += rescale_translation_keys(&mut mobys.anim[k].sequences, plan);
    }
    (keys, layers.len())
}

/// Replaces Ratchet's class 0 with Lauma when `RC_LAUMA=1`: her textures are appended to the moby texture table (the
/// next free indices, like `moby_spawn::load_spaceship`), the class keeps Ratchet's header, skeleton and sequences and
/// gets her packet as both LODs. Nothing is changed unless every step succeeds. Levels without a class 0 are left alone.
pub fn inject(core: &LevelCore, core_data: &[u8], textures: &mut Vec<LevelTexture>, mobys: &mut LevelMobys) -> Result<()> {
    let dump = dump_requested();
    if !enabled() && !dump {
        return Ok(());
    }
    let Some(ci) = mobys.classes.iter().position(|c| c.o_class == RATCHET_O_CLASS) else { return Ok(()) };
    let blob = ratchet_blob(core, core_data);
    let back = blob.and_then(|b| gadget::attachment_joint(b, &mobys.classes[ci].class.header, BACK_LIST).ok());
    if dump {
        static DUMPED: AtomicBool = AtomicBool::new(false);
        if !DUMPED.swap(true, Ordering::Relaxed) {
            match dump_ratchet(blob, mobys, ci, back) {
                Ok(text) => match std::fs::write("lauma_dump.txt", &text) {
                    Ok(()) => println!("hero model: wrote Ratchet's skeleton ({} joints) to lauma_dump.txt", mobys.anim[ci].joint_count),
                    Err(e) => eprintln!("hero model: lauma_dump.txt: {e}"),
                },
                Err(e) => eprintln!("hero model: the skeleton dump failed: {e:#}"),
            }
        }
    }
    if !enabled() {
        return Ok(());
    }
    if mobys.cpu_light {
        eprintln!("hero model: RC_MOBY_CPU_LIGHT bakes the class colours at load; Ratchet stays");
        return Ok(());
    }
    let Some(asset) = asset() else { return Ok(()) };

    // The moby table indices after the level's own (slot value 0xff means unused).
    let first = textures.iter().filter(|t| t.table == TextureTable::Moby).map(|t| t.index + 1).max().unwrap_or(0);
    ensure!(first + asset.textures.len() <= 0xff, "the moby texture table is full ({first} entries + {} textures)", asset.textures.len());

    // Fit her to Ratchet: his bind-pose height from his own packets, her feet on his.
    let class = &mobys.classes[ci].class;
    let unit = class.header.scale / 1024.0;
    ensure!(unit > 0.0, "class 0 has no scale");
    let (mut zmin, mut zmax) = (f32::MAX, f32::MIN);
    for v in class.high_lod.iter().flat_map(|s| &s.vertices) {
        let z = v.z as f32 * unit;
        zmin = zmin.min(z);
        zmax = zmax.max(z);
    }
    if zmin > zmax {
        (zmin, zmax) = (0.0, 1.5);
    }
    let (lmin, lmax) = asset.pos.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p[2]), hi.max(p[2])));
    ensure!(lmax > lmin, "the model has no height");
    let asked = env_f32("RC_LAUMA_SCALE", 1.0);
    let relative = if asked.is_finite() { asked.clamp(0.25, LAUMA_MAX / LAUMA_BASE) } else { 1.0 };
    if relative != asked {
        eprintln!("hero model: RC_LAUMA_SCALE={asked} is outside 0.25..{:.2}; using {relative}", LAUMA_MAX / LAUMA_BASE);
    }
    let total = LAUMA_BASE * relative;
    let scale = (zmax - zmin) / (lmax - lmin) * total;
    let (sin, cos) = env_f32("RC_LAUMA_YAW", 0.0).to_radians().sin_cos();
    let fit = Fit { cos, sin, scale, dz: zmin - lmin * scale };

    let skin_mode = std::env::var("RC_LAUMA_SKIN").unwrap_or_default();
    let rig = asset.rig.as_ref().filter(|_| matches!(skin_mode.trim(), "" | "rig"));
    if rig.is_none() && skin_mode.trim() == "rig" {
        eprintln!("hero model: RC_LAUMA_SKIN=rig, but the model is version 1 (no rig); run tools/lauma_convert.py with lauma_dump.txt");
    }
    let (single, skin_note) = match skin_mode.trim() {
        _ if rig.is_some() => (MobySkin::default(), format!("Ratchet's joints per vertex (rig, {} joints)", rig.map_or(0, |r| r.joints.len()))),
        "static" => (MobySkin::default(), "no joint (static)".to_string()),
        "root" => (MobySkin::joint(ROOT_JOINT), format!("the root joint {ROOT_JOINT}")),
        _ => match back {
            Some(j) => (MobySkin::joint(j), format!("the back joint {j} (joint list {BACK_LIST})")),
            None => {
                eprintln!("hero model: joint list {BACK_LIST} could not be read; skinning to the root joint");
                (MobySkin::joint(ROOT_JOINT), format!("the root joint {ROOT_JOINT} (fallback)"))
            }
        },
    };
    let skin_of = |i: usize| rig.map_or(single, |r| r.skin[i]);
    let (sub, clamped) = build_submesh(asset, &fit, 1.0 / unit, &skin_of);
    // His skeleton in her proportions, checked before anything changes.
    let plan = match rig {
        Some(r) => {
            let bob = env_f32("RC_LAUMA_BOB", LAUMA_BOB).clamp(0.0, 2.0);
            let squash = env_f32("RC_LAUMA_SQUASH", LAUMA_SQUASH).clamp(0.0, 1.0);
            Some(skeleton_plan(&mobys.anim[ci], &r.joints, &fit, unit, if bob.is_finite() { bob } else { LAUMA_BOB }, if squash.is_finite() { squash } else { LAUMA_SQUASH })?)
        }
        None => None,
    };

    let mut slots = [0xffu8; 16];
    for (material, &t) in asset.mat_tex.iter().enumerate() {
        if t != 0xff {
            slots[material] = (first + t as usize) as u8;
        }
    }

    // Everything is checked: change the level.
    for (i, tex) in asset.textures.iter().enumerate() {
        let desc = TextureEntry { data_offset: 0, width: tex.width as i16, height: tex.height as i16, ty: 4, palette: 0, mipmap: 0, pad: 0 };
        textures.push(LevelTexture { table: TextureTable::Moby, index: first + i, source: TextureSource::Entry(desc), texture: tex.clone() });
    }
    let lc = &mut mobys.classes[ci];
    lc.entry.textures = slots;
    lc.class.high_lod = vec![sub.clone()];
    lc.class.low_lod = vec![sub];
    lc.class.metal.clear();
    // No glow packets (the glow indices would otherwise point into Ratchet's lists).
    lc.class.header.rac1_byte_a = 0xff;
    lc.class.header.rac1_byte_b = 0xff;
    let size_source = match std::env::var("RC_LAUMA_SCALE") {
        Ok(v) => format!("RC_LAUMA_SCALE={v} set in the environment"),
        Err(_) => "RC_LAUMA_SCALE not set".to_string(),
    };
    let reshaped = plan.as_ref().map(|p| apply_skeleton(mobys, ci, p));
    *PLAN.lock().unwrap_or_else(|e| e.into_inner()) = plan.clone();
    // Her legs are longer than his and his foot angles were made for his feet: keep her soles on the ground in every
    // pose (RC_LAUMA_FLOOR=0 turns that off; RC_LAUMA_FLOOR_SNAP=<metres, default 0.05> is how far above the floor a
    // hovering foot is still lowered onto it). RC_LAUMA_PIN=neck|all|off (default neck) holds joints at their rest
    // translation and scale, so no key, layer or modifier of his can stretch her neck.
    let snap_m = env_f32("RC_LAUMA_FLOOR_SNAP", 0.05);
    let snap_m = (if snap_m.is_finite() { snap_m.clamp(0.0, 0.5) } else { 0.05 }) * total;
    let pin_mode = std::env::var("RC_LAUMA_PIN").unwrap_or_default();
    let guard = rig.and_then(|r| {
        let n = mobys.anim[ci].joint_count;
        let strand_joints: Vec<u8> = r.jiggle.iter().map(|s| s.joint).collect();
        let pins: Vec<u8> = match pin_mode.trim() {
            "off" => Vec::new(),
            "all" => {
                let mut used = [false; 256];
                for sk in &r.skin {
                    for k in 0..sk.count as usize {
                        used[sk.joints[k] as usize] = true;
                    }
                }
                (2..n.min(256)).filter(|&j| used[j] && !strand_joints.contains(&(j as u8))).map(|j| j as u8).collect()
            }
            _ => [7u8, 8].into_iter().filter(|&j| (j as usize) < n).collect(),
        };
        let limits = GuardLimits { floor: zmin, max_lift: 0.20 * total, snap: snap_m, band: 3.0 * snap_m, floor_on: env_f32("RC_LAUMA_FLOOR", 1.0) > 0.5 };
        floor_guard(asset, r, &fit, unit, limits, pins, &mobys.anim[ci])
    });
    let guarded = guard.as_ref().map_or((0, 0), |g| (if g.floor_on { g.points.len() } else { 0 }, g.pins.len()));
    rc_formats::moby_anim::set_floor_guard(guard);
    // Jiggle: the cape, back train, hair and pendants swing on spring strands (RC_LAUMA_JIGGLE=<0..2, default 1>, 0
    // holds them rigid; RC_LAUMA_JIGGLE_STIFF=<0.25..4, default 1> speeds the springs up).
    let jiggle = match rig {
        Some(r) if !r.jiggle.is_empty() && mobys.anim[ci].rest.len() > 1 => {
            let amount = env_f32("RC_LAUMA_JIGGLE", 1.0);
            let stiffness = env_f32("RC_LAUMA_JIGGLE_STIFF", 1.0);
            let n = mobys.anim[ci].joint_count;
            let packed = |p: [f32; 3]| {
                let (x, y) = (p[0] * fit.cos - p[1] * fit.sin, p[0] * fit.sin + p[1] * fit.cos);
                [x * fit.scale / unit, y * fit.scale / unit, (p[2] * fit.scale + fit.dz) / unit]
            };
            Some(JiggleSetup {
                joint_count: n,
                rest1: mobys.anim[ci].rest[1].map(f32::to_bits),
                strands: r.jiggle.iter().filter(|s| (s.joint as usize) < n && (s.anchor as usize) < n).map(|s| JiggleStrand { pivot: packed(s.pivot), tip: packed(s.tip), ..*s }).collect(),
                amount: if amount.is_finite() { amount.clamp(0.0, 2.0) } else { 1.0 },
                stiffness: if stiffness.is_finite() { stiffness.clamp(0.25, 4.0) } else { 1.0 },
            })
        }
        _ => None,
    };
    let strands = jiggle.as_ref().map_or(0, |j| j.strands.len());
    let jiggle_amount = jiggle.as_ref().map_or(0.0, |j| j.amount);
    rc_formats::moby_anim::set_jiggle(jiggle);

    println!(
        "hero model: Lauma replaces class {RATCHET_O_CLASS}: {} vertices, {} triangles, {} textures (moby table {first}..{}); Ratchet is {:.2} tall at scale {:.4}, \
         she is scaled by {scale:.3} ({total:.2} x his height: baseline {LAUMA_BASE} x RC_LAUMA_SCALE {relative}; {size_source}), skinned to {skin_note}{}{}",
        asset.pos.len(),
        asset.tris.len(),
        asset.textures.len(),
        first + asset.textures.len(),
        zmax - zmin,
        unit * 1024.0,
        if clamped > 0 { format!("; {clamped} vertices clamped to the packed range (try RC_LAUMA_SCALE below 1)") } else { String::new() },
        reshaped.map_or(String::new(), |(keys, layers)| format!("; his skeleton reshaped to her proportions ({keys} translation keys rescaled, {layers} layer classes); floor guard on {} foot points, {} pinned joints; {strands} jiggle strands at {jiggle_amount}", guarded.0, guarded.1)),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_formats::moby::moby_normal;

    /// The packed normals decode back to within about a degree of the input in every direction (a step is 1.4 degrees
    /// in each angle), including straight up and down and the sign changes of azimuth.
    #[test]
    fn normals_round_trip() {
        let mut worst = 1.0f32;
        for i in 0..400 {
            let (a, e) = (i as f32 * 0.37, (i as f32 * 0.113).sin() * 1.5);
            let n = [a.cos() * e.cos(), a.sin() * e.cos(), e.sin()];
            let (az, el) = normal_bytes(n);
            let d = moby_normal(az, el);
            worst = worst.min(n[0] * d[0] + n[1] * d[1] + n[2] * d[2]);
        }
        for n in [[0.0, 0.0, 1.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]] {
            let (az, el) = normal_bytes(n);
            let d = moby_normal(az, el);
            worst = worst.min(n[0] * d[0] + n[1] * d[1] + n[2] * d[2]);
        }
        assert!(worst > 0.9995, "worst dot {worst}");
    }

    /// The one-triangle model of the test below, as a version 1 file.
    fn tiny_model() -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(b"RCHM");
        for v in [1u32, 3, 1, 1] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        for p in [[0.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            for c in p {
                f.extend_from_slice(&c.to_le_bytes());
            }
        }
        for _ in 0..3 {
            for c in [1.0f32, 0.0, 0.0] {
                f.extend_from_slice(&c.to_le_bytes());
            }
        }
        for _ in 0..3 {
            for c in [0.5f32, 0.5] {
                f.extend_from_slice(&c.to_le_bytes());
            }
        }
        for v in [0u32, 1, 2, 0] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&1u32.to_le_bytes());
        f.extend_from_slice(&1u32.to_le_bytes());
        f.extend_from_slice(&[255, 255, 255, 255]);
        f
    }

    /// A version 2 file carries per-vertex skin and the joint positions; a wrong weight sum, a missing joint or a
    /// zero influence count is refused.
    #[test]
    fn parse_reads_the_rig_block_and_refuses_a_broken_one() {
        let rig = |weights: [u16; 2], joint: u8, count: u8| {
            let mut f = tiny_model();
            f[4..8].copy_from_slice(&2u32.to_le_bytes());
            f.extend_from_slice(&2u32.to_le_bytes()); // two joints
            for v in 0..3u8 {
                f.extend_from_slice(&[if v == 2 { joint } else { 1 }, 0, 0, count]);
                for w in [weights[0], weights[1], 0] {
                    f.extend_from_slice(&w.to_le_bytes());
                }
            }
            for p in [[0.0f32, 0.0, 0.0], [0.5, 0.25, 1.0]] {
                for c in p {
                    f.extend_from_slice(&c.to_le_bytes());
                }
            }
            f
        };
        let a = parse(&rig([256, 0], 1, 1)).unwrap();
        let r = a.rig.as_ref().expect("a rig");
        assert_eq!((r.skin.len(), r.joints.len()), (3, 2));
        assert_eq!((r.skin[2].count, r.skin[2].joints[0], r.skin[2].weights[0]), (1, 1, 256));
        assert_eq!(r.joints[1], [0.5, 0.25, 1.0]);
        assert!(parse(&tiny_model()).unwrap().rig.is_none(), "version 1 has no rig");
        assert!(parse(&rig([200, 0], 1, 1)).is_err(), "weights must sum to 256");
        assert!(parse(&rig([256, 0], 2, 1)).is_err(), "joint 2 of 2");
        assert!(parse(&rig([256, 0], 1, 0)).is_err(), "no influence");
        let mut truncated = rig([256, 0], 1, 1);
        truncated.pop();
        assert!(parse(&truncated).is_err(), "truncated");
    }

    /// A version 3 file adds the jiggle strands; a spring out of range is refused.
    #[test]
    fn parse_reads_jiggle_strands() {
        let build = |freq: f32| {
            let mut f = tiny_model();
            f[4..8].copy_from_slice(&3u32.to_le_bytes());
            f.extend_from_slice(&2u32.to_le_bytes());
            for _ in 0..3u8 {
                f.extend_from_slice(&[1, 0, 0, 1]);
                for w in [256u16, 0, 0] {
                    f.extend_from_slice(&w.to_le_bytes());
                }
            }
            for _ in 0..2 {
                for c in [0.0f32; 3] {
                    f.extend_from_slice(&c.to_le_bytes());
                }
            }
            f.extend_from_slice(&1u32.to_le_bytes()); // one strand: joint 1, anchor 0
            f.extend_from_slice(&[1, 0, 0, 0]);
            for c in [freq, 0.4, 30.0, 0.0, 1.0, 0.0, 0.0, 0.0, -1.0] {
                f.extend_from_slice(&c.to_le_bytes());
            }
            f
        };
        let a = parse(&build(3.0)).unwrap();
        let st = a.rig.as_ref().unwrap().jiggle[0];
        assert_eq!((st.joint, st.anchor, st.freq, st.max_deg, st.pivot, st.tip), (1, 0, 3.0, 30.0, [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]));
        assert!(parse(&build(0.0)).is_err(), "a zero frequency is refused");
        let mut cut = build(3.0);
        cut.pop();
        assert!(parse(&cut).is_err(), "truncated");
    }

    /// Three joints in a chain, moved to a model twice as tall: the rest translations are the differences of the new
    /// positions, the inverse-bind rows and the translation keys follow, and a joint with no parent stays.
    #[test]
    fn skeleton_plan_reshapes_a_chain() {
        use rc_formats::moby_anim::IDENTITY;
        let mut skeleton = vec![IDENTITY; 3];
        for (j, z) in [0.0f32, 2.0, 3.0].into_iter().enumerate() {
            skeleton[j][3] = [0.0, 0.0, -z, 1.0]; // inverse bind of a joint at height z
        }
        let anim = MobyAnimClass {
            joint_count: 3,
            skeleton,
            rest: vec![[0.0; 3], [0.0, 0.0, 2.0], [0.0, 0.0, 1.0]],
            parent_word: vec![0, 0x7000_0000, 0x7000_0040],
            sequences: Vec::new(),
        };
        let fit = Fit { cos: 1.0, sin: 0.0, scale: 0.5, dz: 0.0 };
        // unit 0.5 m (exact in binary): the model's joints at 0, 4 and 6 m land at 4 and 6 units.
        let plan = skeleton_plan(&anim, &[[0.0; 3], [0.0, 0.0, 4.0], [0.0, 0.0, 6.0]], &fit, 0.5, 1.0, 1.0).unwrap();
        assert_eq!(plan.rest, vec![[0.0; 3], [0.0, 0.0, 4.0], [0.0, 0.0, 2.0]]);
        assert_eq!(plan.shift[1], [0.0, 0.0, 2.0]);
        assert_eq!(plan.shift[2], [0.0, 0.0, 3.0]);
        assert_eq!(plan.ratio, vec![1.0, 2.0, 2.0], "the root's keys are world motion: unscaled");
        let mut m = anim.skeleton[2];
        shift_inverse_bind(&mut m, plan.shift[2]);
        assert_eq!(m[3], [0.0, 0.0, -6.0, 1.0]);
        // a key of joint 2 sitting 10 units above its rest: 20 above the new rest
        assert_eq!(rescale_key(&plan, 2, [0, 0, 11]), [0, 0, 22]);
        assert_eq!(rescale_key(&plan, 2, [0, 0, 1]), [0, 0, 2], "the rest translation maps to the new one");
        assert!(skeleton_plan(&anim, &[[0.0; 3]; 2], &fit, 0.5, 1.0, 1.0).is_err(), "joint count mismatch");
    }

    /// The bob factor scales the pelvis and root keys only; squash keys are pulled toward 1 unless they are far from it.
    #[test]
    fn bob_and_squash_are_softened() {
        use rc_formats::moby_anim::IDENTITY;
        let mut skeleton = vec![IDENTITY; 3];
        for (j, z) in [0.0f32, 2.0, 3.0].into_iter().enumerate() {
            skeleton[j][3] = [0.0, 0.0, -z, 1.0];
        }
        let anim = MobyAnimClass {
            joint_count: 3,
            skeleton,
            rest: vec![[0.0; 3], [0.0, 0.0, 2.0], [0.0, 0.0, 1.0]],
            parent_word: vec![0, 0x7000_0000, 0x7000_0040],
            sequences: Vec::new(),
        };
        let fit = Fit { cos: 1.0, sin: 0.0, scale: 0.5, dz: 0.0 };
        let plan = skeleton_plan(&anim, &[[0.0; 3], [0.0, 0.0, 4.0], [0.0, 0.0, 6.0]], &fit, 0.5, 0.5, 0.5).unwrap();
        assert_eq!(plan.ratio, vec![1.0, 2.0, 2.0], "bob is not part of the ratio");
        assert_eq!((soft_bob(10.0, 0.5, 20.0), soft_bob(-10.0, 0.5, 20.0)), (5.0, -5.0), "small motion halved");
        assert_eq!((soft_bob(30.0, 0.5, 20.0), soft_bob(-30.0, 0.5, 20.0)), (20.0, -20.0), "knee 10, plus the 10 beyond it");
        assert_eq!(soft_bob(20.0, 0.5, 20.0), 10.0, "continuous at the knee");
        assert_eq!(soften_scale([0x1000, 0x1000, 0x0cd6], 0.5), [0x1000, 0x1000, 0x0e6b], "a squash halves toward 1");
        assert_eq!(soften_scale([0x1000, 0x1000, 0x1000], 0.0), [0x1000; 3]);
        assert_eq!(soften_scale([0, 0x1000, 0x1000], 0.5), [0, 0x1000, 0x1000], "a hidden joint stays hidden");
        assert_eq!(soften_scale([0x4000, 0x1000, 0x1000], 0.5), [0x4000, 0x1000, 0x1000], "a big joint stays big");
    }

    /// With a rotated bind frame the translation row moves by `-d * R`, not `-d`.
    #[test]
    fn inverse_bind_shift_follows_the_rotation() {
        let mut m = [[0.0, 1.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [5.0, 6.0, 7.0, 1.0]];
        shift_inverse_bind(&mut m, [1.0, 2.0, 3.0]);
        assert_eq!(m[3], [3.0, 5.0, 4.0, 1.0]);
    }

    /// A one-triangle model parses; damaged files are refused instead of panicking.
    #[test]
    fn parse_accepts_a_tiny_model_and_refuses_damage() {
        let mut f = Vec::new();
        f.extend_from_slice(b"RCHM");
        for v in [1u32, 3, 1, 1] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        for p in [[0.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            for c in p {
                f.extend_from_slice(&c.to_le_bytes());
            }
        }
        for _ in 0..3 {
            for c in [1.0f32, 0.0, 0.0] {
                f.extend_from_slice(&c.to_le_bytes());
            }
        }
        for _ in 0..3 {
            for c in [0.5f32, 0.5] {
                f.extend_from_slice(&c.to_le_bytes());
            }
        }
        for v in [0u32, 1, 2, 0] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&1u32.to_le_bytes());
        f.extend_from_slice(&1u32.to_le_bytes());
        f.extend_from_slice(&[255, 255, 255, 255]);
        let a = parse(&f).unwrap();
        assert_eq!((a.pos.len(), a.tris.len(), a.textures.len()), (3, 1, 1));
        assert_eq!(a.mat_tex[0], 0);
        assert!(parse(&f[..f.len() - 1]).is_err(), "truncated");
        assert!(parse(b"nope").is_err(), "magic");
        let mut bad = f.clone();
        bad[20 + 16 + 3 * 12 * 2 + 3 * 8] = 9; // triangle index 9 of 3 vertices
        assert!(parse(&bad).is_err(), "index out of range");
    }
}
