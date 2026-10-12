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
//!   path runs and she follows the root joint's motion; she does not deform yet. Mapping her bones onto his joints
//!   later means filling [`MobySkin`] per vertex in [`build_submesh`], nothing else. `RC_LAUMA_SKIN=static` skins
//!   to no joint at all (joint count 0: the identity) if the root joint's motion looks wrong.
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
//! model forward is +x; the converter faces her that way); `RC_LAUMA_SKIN=static` as above. With `RC_MOBY_CPU_LIGHT`
//! the class colours are baked at load before this runs, so Ratchet stays.

use crate::moby_render::LevelMobys;
use anyhow::{ensure, Context, Result};
use rc_formats::gadget::RATCHET_O_CLASS;
use rc_formats::level::TextureEntry;
use rc_formats::moby::{MobySkin, MobySubmesh, MobyTriangle, MobyVertex, MobyVertexKind};
use rc_formats::texture::{LevelTexture, Texture, TextureSource, TextureTable};
use std::path::PathBuf;
use std::sync::OnceLock;

/// The joint every vertex is skinned to until her bones are mapped: Ratchet's root.
const ROOT_JOINT: u8 = 0;

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
pub fn inject(textures: &mut Vec<LevelTexture>, mobys: &mut LevelMobys) -> Result<()> {
    if !enabled() {
        return Ok(());
    }
    if mobys.cpu_light {
        eprintln!("hero model: RC_MOBY_CPU_LIGHT bakes the class colours at load; Ratchet stays");
        return Ok(());
    }
    let Some(asset) = asset() else { return Ok(()) };
    let Some(ci) = mobys.classes.iter().position(|c| c.o_class == RATCHET_O_CLASS) else { return Ok(()) };

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

    let skin = if std::env::var("RC_LAUMA_SKIN").is_ok_and(|v| v.trim() == "static") { MobySkin::default() } else { MobySkin::joint(ROOT_JOINT) };
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
         she is scaled by {scale:.3}{}",
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
