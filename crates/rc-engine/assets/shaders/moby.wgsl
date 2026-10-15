// Moby base pass: skinning with the animated joint palette and the VU0 104691 vertex lighting (single-
// matrix path, emulated in f32), then GS TFX = MODULATE and GS fog (PRIM FGE = 1). Rust side:
// moby_render.rs (layout), moby_anim.rs (palette), moby_light.rs (light block, f32 replica of this code).
// Spec: docs/plan/moby_skinning_lighting.md §4-5, docs/plan/moby_animation.md.
//
// Skinning (VU0 104691 entry 0): M = F[j0] (1 joint) or w0·F[j0] + w1·F[j1] (+ w2·F[j2]) with w/256;
// pos = trunc(M0·x + M1·y + M2·z + M3) (ftoi0) wrapped to s16 (ppach keeps the low halfword); then the
// per-moby VU1 matrix, here the instance record's model matrix (packed units → Bevy world) and the view.
// Lighting: n = (cos a·cos e, sin a·cos e, sin e) from the boot-ELF table, n' = M0·n.x + M1·n.y + M2·n.z
// (not normalised), d_k = L_k·n', f_k = max(d_k, −|K_k|·d_k), c = A + ⌊128·(Σ C_k f_k)·rsqrt(|n'|²)⌋
// (the 65536 + c/128 float's 1/128 grid truncation), then the EE pack: s16(c)·m saturated to s16, >> 7,
// low byte. Colours travel as bytes / 128 like the tfrag pass.
//
// Cel shading (a replacement hero model, hero_model.rs; vertices flagged SKIN_TOON, bit 25 of skin.x): the fragment
// replaces the smooth Gouraud result by a banded one. `lam` is the vertex's lambert term against the moby's own three
// lights (normalised by their total colour, so it does not depend on how bright the level is), the band a smoothstep of
// it, shade = pastel cool shadow tint -> white, times the texture; a faint rim from the view-space normal. Vertices
// flagged SKIN_OUTLINE (bit 26) are the outline shell: pre-displaced along smoothed normals at load, drawn only where
// their normal faces away from the camera (no back-face culling here, so the facing test is on the normal), flat dark
// texture colour. Every other vertex has toon.x = 0 and takes the original path unchanged.
//
// MODULATE: Cv = (Ct * Cf) >> 7, clamped. Gouraud RGBA and F are interpolated linearly in screen space
// (@interpolate(linear)), ST perspective-correctly. Alpha: ALPHA_1 = 0x8000000044, TEST_1 = 0x5360b per
// moby (MobyProc GS state at 0x1dedc0), as for tfrags; the fragment returns As / 128 and the GS_ATEST_*
// defs select the half of the alpha-test split this draw is (gs_state.rs). Fog: the moby VU1 program (13859) clamps the F lane between the same
// view-context fog terms as the tfrag program after adding the fog offset, so the tfrag fog uniform is reused.

#import bevy_pbr::mesh_view_bindings::view
#import bevy_pbr::{
    mesh_functions,
    view_transformations::{position_world_to_clip, position_world_to_view},
}
#ifdef DISPLAY_BLEND_MIX
#import rerac::display_blend::gs_mix
#endif
#ifdef DISPLAY_BLEND_ADD
#import rerac::display_blend::gs_add
#endif

const SKIN_TOON: u32 = 0x2000000u;
const SKIN_OUTLINE: u32 = 0x4000000u;
// Cel look (display-encoded colour space, like the texture): band edge and softness on `lam`, the shadow tint, an
// overall gain and the rim strength. The outline colour is derived from the texture it outlines (see OUTLINE_* below),
// so it suits any model or material without per-material data.
// The environment: the moby's own light drives the cel shading. Its brightness sets the level (lit luminance TOON_REF
// and above is full daylight, lower is darker: TOON_LEVEL_POW curves it, TOON_MIN floors it) and its colour the hue
// (TOON_HUE: 0 none, 1 fully): the lit side takes the colour of ambient + lights, the shadow side the ambient's
// (blue at night under a blue ambient), and the shadow gets shallower the more the light is just ambient.
const TOON_REF: f32 = 1.0;
const TOON_MIN: f32 = 0.22;
const TOON_LEVEL_POW: f32 = 0.8;
const TOON_HUE: f32 = 0.9;
// The outline shell is pushed this far (metres) away from the camera along the view ray: its screen position does not
// change, but it never wins a depth test against the model it outlines (hair cards and antlers, which sit within a
// few millimetres of what they outline, no longer show a thick or encroaching line).
const OUTLINE_PUSH: f32 = 0.008;
const TOON_THR: f32 = 0.34;
const TOON_SOFT: f32 = 0.05;
const TOON_SHADOW: vec3<f32> = vec3<f32>(0.68, 0.72, 0.90);
const TOON_GAIN: f32 = 1.06;
const TOON_RIM: f32 = 0.22;
// Outline colour, from the albedo of the surface it outlines: its saturation is boosted (OUTLINE_SAT: pale skin turns
// brown, lilac hair deep purple, a grey stays grey) and its brightness pulled to a darker target
// OUTLINE_LUM_MIN + OUTLINE_LUM_SLOPE * albedo luminance (a bright albedo gets a mid-dark line, a dark one a darker
// but never black one). The albedo is read at a blurred level (OUTLINE_LOD) so texture detail does not speckle the line;
// a texture with a single level just gives its base one.
const OUTLINE_SAT: f32 = 2.0;
const OUTLINE_LUM_MIN: f32 = 0.12;
const OUTLINE_LUM_SLOPE: f32 = 0.28;
const OUTLINE_LOD: f32 = 3.0;

struct MobyFog {
    // rgb = FOGCOL (display-encoded 0..1), w = 1 when fog is enabled.
    color: vec4<f32>,
    // (slope per integer unit of depth, offset qw661.w, lower clamp qw656.y, upper clamp qw656.z).
    params: vec4<f32>,
}

struct MobyInst {
    // Packed game model units -> Bevy world: game_to_bevy · [s/1024 · R | p].
    model: mat4x4<f32>,
    // Row j = (L_0[j], L_1[j], L_2[j], 0): model-space "to light" vectors of the 3 lights.
    light_rows: array<vec4<f32>, 3>,
    // C_0, C_1, C_2 (1.0 adds 128 to a colour byte), w = 0.
    light_colors: array<vec4<f32>, 3>,
    // (−|K_0|, −|K_1|, −|K_2|, 1).
    neg_k: vec4<f32>,
    // Ambient r, g, b and alpha as colour counts (0..255).
    ambient: vec4<f32>,
    // x = first palette slot, y = colour mode (0 GPU light, 1 CPU table, 2 unlit), z = CPU colour base.
    misc: vec4<u32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var tex_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var<storage, read> fog: MobyFog;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var<storage, read> insts: array<MobyInst>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var<storage, read> palette: array<mat4x4<f32>>;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var<storage, read> normal_table: array<vec2<f32>>;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var<storage, read> cpu_colors: array<u32>;

// Per instance, rewritten per frame by MobyProc's replay (crate::moby_lod): x = vertex alpha (the ambient α
// lane: distance fade × moby+0x23 >> 7), y = flags (1 = tint red: low LOD with RC_MOBY_LOD_TINT=1),
// z = shine alpha, w = glow word (bit 24: on the glow list, mode 0x10; bits 0..23: moby+0x90 RGB); e = the metal
// pass's sphere-map basis (read by moby_metal.wgsl).
struct MobyLod {
    misc: vec4<u32>,
    e: array<vec4<f32>, 3>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(7) var<storage, read> lods: array<MobyLod>;

struct MobyVertex {
    @builtin(instance_index) instance_index: u32,
    // Packed model position (s16 as f32), game axes.
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    // x = azimuth | elevation << 8 | joint count << 16 | glow packet << 24; y = joints; z = weights (10 bits each);
    // w = multiplier RGBA.
    @location(2) skin: vec4<u32>,
    // Index of the vertex in the class's high-LOD list (CPU colour table).
    @location(3) vid: u32,
}

struct MobyVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Gouraud RGBA / 128, screen-space linear like the GS.
    @location(1) @interpolate(linear) color: vec4<f32>,
    // GS F / 255, screen-space linear like the GS.
    @location(2) @interpolate(linear) fog: f32,
    // x = 0 plain, 1 cel-shaded, 2 outline shell; y = lambert term (0..1); z = how bright the moby's light is; w = view-space
    // normal z (> 0 faces the camera).
    @location(3) toon: vec4<f32>,
    // The moby's ambient colour and its ambient + lights colour (colour/128; 1,1,1 where unlit), for the cel shading.
    @location(4) amb: vec3<f32>,
    @location(5) lit: vec3<f32>,
}

fn s16(x: i32) -> i32 { return (x << 16u) >> 16u; }

@vertex
fn vertex(v: MobyVertex) -> MobyVertexOutput {
    var out: MobyVertexOutput;
    let tag = mesh_functions::get_tag(v.instance_index);
    let inst = &insts[tag];
    let lod = &lods[tag];
    let base = (*inst).misc.x;
    let alpha = f32((*lod).misc.x);

    // Blended joint matrix (weights are /256; every blend on the disc sums to 256). Count 0: a low-LOD list
    // drawn with job joint count 0, for which MobyAnimEval only writes the identity (moby_render build_parts).
    let count = (v.skin.x >> 16u) & 0xffu;
    let j = vec3<u32>(v.skin.y & 0xffu, (v.skin.y >> 8u) & 0xffu, (v.skin.y >> 16u) & 0xffu);
    var m = palette[base + j.x];
    if (count == 0u) {
        m = mat4x4<f32>(vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
    }
    if (count >= 2u) {
        let w = vec3<f32>(f32(v.skin.z & 0x3ffu), f32((v.skin.z >> 10u) & 0x3ffu), f32((v.skin.z >> 20u) & 0x3ffu)) / 256.0;
        m = palette[base + j.x] * w.x + palette[base + j.y] * w.y;
        if (count >= 3u) {
            m = m + palette[base + j.z] * w.z;
        }
    }

    // ftoi0 (truncate; out-of-range saturates) then the s16 wrap of ppach.
    let sp = clamp(trunc((m * vec4<f32>(v.position, 1.0)).xyz), vec3<f32>(-2147483520.0), vec3<f32>(2147483520.0));
    let pi = vec3<i32>(s16(i32(sp.x)), s16(i32(sp.y)), s16(i32(sp.z)));
    let world = ((*inst).model * vec4<f32>(vec3<f32>(pi), 1.0)).xyz;
    // The outline shell (SKIN_OUTLINE) is moved away from the camera along the view ray: see OUTLINE_PUSH.
    let is_toon = (v.skin.x & SKIN_TOON) != 0u;
    let is_outline = (v.skin.x & SKIN_OUTLINE) != 0u;
    let ray = normalize(world - view.world_position.xyz);
    out.position = position_world_to_clip(select(world, world + ray * OUTLINE_PUSH, is_outline));
    // A class texture scroll (misc.w: s | t << 16, s16 in 1/4096: `FUN_00263d90`'s offset on the class's STs).
    let scroll = (*inst).misc.w;
    out.uv = v.uv + vec2<f32>(f32(s16(i32(scroll & 0xffffu))), f32(s16(i32(scroll >> 16u)))) / 4096.0;

    // Cel shading inputs (only read for flagged vertices).
    var lam = 1.0;
    var bright = 1.0;
    let nz_a = normal_table[v.skin.x & 0xffu];
    let nz_e = normal_table[(v.skin.x >> 8u) & 0xffu];
    let n_model = vec3<f32>(nz_a.x * nz_e.x, nz_a.y * nz_e.x, nz_e.y);
    let n_posed = (m[0] * n_model.x + m[1] * n_model.y + m[2] * n_model.z).xyz;
    let n_world = normalize(((*inst).model * vec4<f32>(n_posed, 0.0)).xyz);
    let n_view = position_world_to_view(world + n_world) - position_world_to_view(world);
    out.toon = vec4<f32>(select(0.0, select(1.0, 2.0, is_outline), is_toon), 1.0, 1.0, n_view.z);
    out.amb = vec3<f32>(1.0);
    out.lit = vec3<f32>(1.0);

    let mode = (*inst).misc.y;
    if (mode == 1u) {
        out.color = unpack4x8unorm(cpu_colors[(*inst).misc.z + v.vid]) * (255.0 / 128.0);
        out.color.a = alpha / 128.0;
    } else if (mode == 2u) {
        out.color = vec4<f32>(1.0, 1.0, 1.0, alpha / 128.0);
    } else {
        let ta = normal_table[v.skin.x & 0xffu];
        let te = normal_table[(v.skin.x >> 8u) & 0xffu];
        let n = vec3<f32>(ta.x * te.x, ta.y * te.x, te.y);
        let np = (m[0] * n.x + m[1] * n.y + m[2] * n.z).xyz;
        let d = (*inst).light_rows[0] * np.x + (*inst).light_rows[1] * np.y + (*inst).light_rows[2] * np.z;
        let f = max(d, d * (*inst).neg_k);
        let s = (*inst).light_colors[0] * f.x + (*inst).light_colors[1] * f.y + (*inst).light_colors[2] * f.z;
        let q = inverseSqrt(np.x * np.x + np.y * np.y + np.z * np.z);
        let csum = (*inst).light_colors[0].xyz + (*inst).light_colors[1].xyz + (*inst).light_colors[2].xyz;
        let lw = vec3<f32>(0.299, 0.587, 0.114);
        lam = clamp(dot(s.xyz * q, lw) / max(dot(csum, lw), 0.0001), 0.0, 1.0);
        bright = dot((*inst).ambient.xyz / 128.0 + csum, lw);
        out.toon.y = lam;
        out.toon.z = bright;
        out.amb = (*inst).ambient.xyz / 128.0;
        out.lit = out.amb + csum;
        let c = vec4<f32>((*inst).ambient.xyz, alpha) + floor(s * q * 128.0);
        let mult = vec4<i32>(unpack4x8unorm(v.skin.w) * 255.0 + 0.5);
        let h = vec4<i32>(s16(i32(c.x)), s16(i32(c.y)), s16(i32(c.z)), s16(i32(c.w)));
        let p = clamp(h * mult, vec4<i32>(-32768), vec4<i32>(32767)) >> vec4<u32>(7u);
        out.color = vec4<f32>(p & vec4<i32>(0xff)) / 128.0;
    }
    // The glow list (MobyProc level01 0x26b890, fun_002116b8 0x26a650; crate::moby_lod): after the skin/light pass
    // every vertex colour of a glow packet is replaced by the moby's glow word, its byte 3 by the vertex alpha.
    let glow = (*lod).misc.w;
    if ((v.skin.x & 0x1000000u) != 0u && (glow & 0x1000000u) != 0u) {
        out.color = vec4<f32>(f32(glow & 0xffu), f32((glow >> 8u) & 0xffu), f32((glow >> 16u) & 0xffu), alpha) / 128.0;
    }
    if (((*lod).misc.y & 1u) != 0u) {
        out.color = vec4<f32>(out.color.r * 0.5 + 1.0, out.color.gb * 0.3, out.color.a);
    }

    // Camera-space depth in the game's integer units (Bevy view space looks down -z, game units).
    let depth = -position_world_to_view(world).z * 1024.0;
    // VU: w = depth * slope (the w row of the matrix), += qw661.w, min qw656.z, max qw656.y, ftoi4.
    let fg = max(min(depth * fog.params.x + fog.params.y, fog.params.w), fog.params.z);
    out.fog = trunc(fg) / 255.0;
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: MobyVertexOutput) -> @location(0) vec4<f32> {
    let t = textureSample(tex, tex_sampler, in.uv);
    var rgb = min(t.rgb * in.color.rgb, vec3<f32>(1.0));
    let lw = vec3<f32>(0.299, 0.587, 0.114);
    let lum_lit = max(dot(in.lit, lw), 0.001);
    let level = pow(clamp(lum_lit / TOON_REF, TOON_MIN, 1.0), TOON_LEVEL_POW);
    if (in.toon.x > 1.5) {
        // Outline shell: only the half whose normal faces away from the camera, flat, coloured from the albedo it
        // outlines and dimmed with the scene's light (a night's outline must not glow against a dark body).
        if (in.toon.w > 0.0) { discard; }
        let albedo = textureSampleLevel(tex, tex_sampler, in.uv, OUTLINE_LOD).rgb;
        let lum = dot(albedo, lw);
        let boosted = max(vec3<f32>(lum) + (albedo - vec3<f32>(lum)) * OUTLINE_SAT, vec3<f32>(0.0));
        let target_lum = OUTLINE_LUM_MIN + OUTLINE_LUM_SLOPE * lum;
        rgb = min(boosted * (target_lum / max(dot(boosted, lw), 0.02)) * mix(1.0, level, 0.6), vec3<f32>(1.0));
    } else if (in.toon.x > 0.5) {
        let band = smoothstep(TOON_THR - TOON_SOFT, TOON_THR + TOON_SOFT, in.toon.y);
        let lum_amb = max(dot(in.amb, lw), 0.001);
        let lit_hue = in.lit / lum_lit;
        let amb_hue = in.amb / lum_amb;
        let depth_k = clamp(1.6 * (1.0 - lum_amb / lum_lit), 0.35, 1.0);
        let shadow_mul = mix(vec3<f32>(1.0), TOON_SHADOW * mix(vec3<f32>(1.0), amb_hue, TOON_HUE), depth_k);
        let lit_mul = mix(vec3<f32>(1.0), lit_hue, TOON_HUE);
        let shade = mix(shadow_mul, lit_mul, band);
        let rim = pow(1.0 - clamp(abs(in.toon.w), 0.0, 1.0), 3.0) * TOON_RIM * (1.0 - 0.6 * band);
        rgb = min(t.rgb * shade * (level * TOON_GAIN) + lit_hue * rim * 0.25, vec3<f32>(1.0));
    }
    if (fog.color.w > 0.5) {
        rgb = mix(fog.color.rgb, rgb, in.fog);
    }
    // As = (At * Af) >> 7 in GS units (At raw 0..0x80, Af 0x80 = 1.0), clamped to 0xff; FGE leaves it alone.
    let a_s = min(floor(round(t.a * 255.0) * round(in.color.a * 128.0) / 128.0), 255.0);
    // GS TEST_1 alpha test (ATST GEQUAL AREF, AFAIL RGB_ONLY): which half of the split this draw is (gs_state.rs).
#ifdef GS_ATEST_PASS
    if (a_s < f32(#{GS_AREF})) { discard; }
#endif
#ifdef GS_ATEST_FAIL
    if (a_s >= f32(#{GS_AREF})) { discard; }
#endif
    // Effect mobys (gs_state `EffectMix` / `AdditiveNoZ`): the GS blend on the frame's display bytes
    // (crate::display_blend).
#ifdef DISPLAY_BLEND_MIX
    return gs_mix(round(rgb * 255.0), a_s);
#else ifdef DISPLAY_BLEND_ADD
    return gs_add(round(rgb * 255.0), a_s);
#else
    return vec4<f32>(srgb_to_linear(rgb), a_s / 128.0);
#endif
}
