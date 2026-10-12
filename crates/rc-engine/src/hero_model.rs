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
//! * **Skinning**: every vertex is skinned to one joint of Ratchet's skeleton (`MobySkin::joint`), so the palette
//!   path runs and she moves with that joint; she does not deform yet. The joint is the one his back attachment
//!   list (joint list 5: where Clank and the packs ride) resolves to, so she turns with his body in flips, bobs
//!   with his run and lowers in his crouch; the root joint stays put in all of them. `RC_LAUMA_SKIN=root` binds to
//!   the root joint instead, `RC_LAUMA_SKIN=static` to no joint at all (joint count 0: the identity). Mapping her
//!   bones onto his joints later means filling [`MobySkin`] per vertex in [`build_submesh`], nothing else.
//! * **Data structure**: one packet of packed `i16` positions (in Ratchet's own class scale, so his animation
//!   translations still apply), 4.12 ST, the game's triangle list with texture slots 0..15, drawn as the high and
//!   the low LOD alike (a far hero must not vanish), no metal packets (his chrome would hang in the air) and no
//!   glow packets.
//!
//! The model is `assets/models/lauma.rcm`, written by `tools/lauma_convert.py` from the glTF binary (axes, textures
//! and alpha already in the form the engine takes). Her size is fitted to Ratchet's: his bind-pose height from his own
//! packets, feet on his feet.
//!
//! Environment: `RC_LAUMA=1` enables; `RC_LAUMA_MODEL=<path>` another file; `RC_LAUMA_SCALE=<f>` (default 1) scales her
//! relative to the fitted size; `RC_LAUMA_YAW=<degrees>` (default 0) turns her about the vertical axis (the game's
//! model forward is +x; the converter faces her that way); `RC_LAUMA_SKIN=back|root|static` as above (default `back`).
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
use rc_formats::texture::{LevelTexture, Texture, TextureSource, TextureTable};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

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

    fn floats<const N: usize>(&mut self, count: usize) -> Result<Vec<[f32; N]>> {
        let bytes = count.checked_mul(N * 4).context("the vertex count is absurd")?;
        let raw = self.take(bytes)?;
        Ok(raw.chunks_exact(N * 4).map(|c| std::array::from_fn(|i| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap()))).collect())
    }
}

fn parse(data: &[u8]) -> Result<Asset> {
    let mut r = Reader { data, at: 0 };
    ensure!(r.take(4)? == b"RCHM", "not a hero model file (magic)");
    ensure!(r.u32()? == 1, "unsupported hero model version");
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
    Ok(Asset { pos, nrm, uv, tris, mat_tex, textures })
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
/// (returns how many vertices had to be clamped), normals as bytes, ST in 4.12, every vertex on one joint.
fn build_submesh(a: &Asset, fit: &Fit, inv: f32, skin: MobySkin) -> (MobySubmesh, usize) {
    let mut clamped = 0;
    let mut vertices = Vec::with_capacity(a.pos.len());
    for (i, ((p, n), t)) in a.pos.iter().zip(&a.nrm).zip(&a.uv).enumerate() {
        let (x, y) = (p[0] * fit.cos - p[1] * fit.sin, p[0] * fit.sin + p[1] * fit.cos);
        let packed = [x * fit.scale * inv, y * fit.scale * inv, (p[2] * fit.scale + fit.dz) * inv].map(f32::round);
        if packed.iter().any(|c| !(-32768.0f32..=32767.0).contains(c)) {
            clamped += 1;
        }
        let (normal_azimuth, normal_elevation) = normal_bytes([n[0] * fit.cos - n[1] * fit.sin, n[0] * fit.sin + n[1] * fit.cos, n[2]]);
        vertices.push(MobyVertex {
            raw: [0; 8],
            normal_azimuth,
            normal_elevation,
            x: packed[0] as i16,
            y: packed[1] as i16,
            z: packed[2] as i16,
            id: (i & 0x1ff) as u16,
            kind: MobyVertexKind::Single,
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
    let scale = (zmax - zmin) / (lmax - lmin) * env_f32("RC_LAUMA_SCALE", 1.0);
    let (sin, cos) = env_f32("RC_LAUMA_YAW", 0.0).to_radians().sin_cos();
    let fit = Fit { cos, sin, scale, dz: zmin - lmin * scale };

    let (skin, skin_note) = match std::env::var("RC_LAUMA_SKIN").unwrap_or_default().trim() {
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
    let (sub, clamped) = build_submesh(asset, &fit, 1.0 / unit, skin);

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

    println!(
        "hero model: Lauma replaces class {RATCHET_O_CLASS}: {} vertices, {} triangles, {} textures (moby table {first}..{}); Ratchet is {:.2} tall at scale {:.4}, \
         she is scaled by {scale:.3}, skinned to {skin_note}{}",
        asset.pos.len(),
        asset.tris.len(),
        asset.textures.len(),
        first + asset.textures.len(),
        zmax - zmin,
        unit * 1024.0,
        if clamped > 0 { format!("; {clamped} vertices clamped to the packed range (try RC_LAUMA_SCALE below 1)") } else { String::new() },
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
