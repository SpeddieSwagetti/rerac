# tfrag VU1 microprogram (55907) — analysis for a shader port

All line numbers refer to `work/vu/55907_tfrag.txt` unless prefixed `FB:` (`work/vu/903379.txt`). Micro addresses are instruction-pair indices (MSCAL units).

## 1. Program map

**Jump table** (lines 1–26, one `b Ln | xtop vi14` per 2 addresses; `xtop` makes vi14 = 0 or 0x148, the current VIF double-buffer):

| MSCAL | target | path |
|---|---|---|
| 0x00 | L11 (l.83) | init: vf03 = (0x2a0,0x350,0x2a0,0x350) output ring, vf04 = qw664. Must be called once. |
| 0x02, 0x16, 0x18 | L10 (l.80) | no-op: reload vf04 from qw664, end. |
| 0x04 | L1→L112 | LOD2: inline transform, **no** clip test, colours raw. |
| 0x06, 0x0c, 0x14 | L2/L5/L9→L127 | LOD2: inline transform, **with** guard-band clip test (ADC drop). |
| 0x08 | L3: L12, L26, L48, L102 | LOD1: pre-transform common; morph LOD-01 verts with collapse; "unk indices 2" pass; emit (no clip). |
| 0x0a | L4: L12, L18, L102 | LOD1: morph LOD-01 without collapse; emit. |
| 0x0e | L6: L12, L18, L25, L47, L102 | LOD0: morph LOD-01, morph LOD-0 with collapse, unk-2 pass, emit. |
| 0x10 | L7: L13, L17, L102 | LOD0: pre-transform common+LOD01; morph LOD-0; emit. |
| 0x12 | L8: L13, L17, L84 | as 0x10 but emit loop with clip test (L84 — see §9, suspicious). |

Every subroutine has two copies: one for vi14==0 and one with hard-coded `328(...)` offsets (e.g. L19 vs L22, L102 vs L107), selected by `ibne vi00, vi14`. So buffer A = qw 0–0x147, buffer B = 0x148–0x28f; "matrix at 0x14d" is just buffer B's qw 5.

**Subroutines.** L12/L13/L14/L15 (l.91–153): transform positions in place (common count = hdr 0.x; L13 adds 0.z), also `itof0` the interleaved colour qwords. L17/L18→L19/L20 (l.155–271): LOD-0 / LOD-01 morph. L25/L26→L27/L28 (l.367–535): same morph plus "collapse to parent" branches L33–L36. L47/L48 (l.679–896): the unk-indices-2 pass. L79–L83 (l.897–981) and L122–L126 (l.1655–1744): strip-command processor (two copies; L122 additionally keeps a per-strip vertex counter vi07 and tracks the kick buffer in vf24.w). Emit loops: L84 (l.982), L102 (l.1219), L112 (l.1421), L127 (l.1745), each unrolled ×4 with return addresses into the unrolled body (`iaddiu vi15, 0x3b9…`).

**Header reads** (all via `n(vi14)`): 0.x common count; 0.z LOD-01 count; 0.w = count for L48 pass; 1.x LOD-0 count; 1.y = count for L47 pass; 1.z positions base; 2.y/2.w vertex-info LOD-01/LOD-0; 2.z / 3.x = address of qword arrays used by L48/L47 (see §5); 3.y indices; 3.z/4.x parent indices; 3.w/4.y unk-indices-2; 4.z strips; 4.w ad-gifs. 0.y, 1.w, 2.x are never read. Matrix rows qw 5–8 read at l.111–118, 174–178, 388–392, 1429–1432, 1757–1762.

**Memory:** constants at qw 656–669 (0x290–0x29d, absolute, EE-supplied — not in the tfrag record and not mentioned in render_pipeline.md); output GS packets double-buffered at **0x2a0 and 0x350** (176 qw each, `mr32 vf03` rotates after every kick, l.938/1697). Data written back into the input buffer: positions+colours (in place, float), vertex-info entries (collapse), and array 2.z/3.x. **xgkick sites:** l.942, 953, 978 (L81/L82/L83) and l.1701, 1713, 1741 (L124/L125/L126). The kick is issued *before* the new packet is filled in the other buffer.

**Strip processor (L79/L80):** next strip x already in vi12. x>0: new GIF tag with NLOOP=x (`isw.x vi12,-1(vi06)`, l.905), end pointer = 3·x. x==0: OR 0x8000 (EOP) into the last tag, kick, reload vf04, end (L83). x<0: unbias +0x80; y≥0 → copy tag qw660 + 5 ad-gif qw from `adgif_base + z` (l.916–927); y<0 → EOP-patch, kick current buffer, switch buffer, and **if z≥0 also copy an ad-gif** (L82, l.952) — the doc's §2.6 misses that a kick record may also switch texture. All emit preambles assume strip[0] carries an ad-gif (unconditional +0x80 and copy, l.997–1010).

Packet layout: `[tag660][5 A+D qw]` then per strip `[tag658 NLOOP=n][n × (ST, RGBAQ, XYZF2)]`; only the last tag before a kick has EOP. NREG=3, register order ST, RGBAQ, XYZ(F)2 ⇒ REGS=0x412 (inferred from store order l.1047–1053). PRE/PRIM/FLG literals live in qw 658/660 and are not visible in the microcode.

## 2. Per-vertex transform

Positions arrive as integers (origin+local) and are converted with **itof0** (l.111, 1443). Multiply (l.114–124, 1447–1454):
```
acc = M[3]            ; mulaw ACC, qw8, vf00.w
p   = acc + qw5*x + qw6*y + qw7*z
```
i.e. `out = x·row5 + y·row6 + z·row7 + row8` — column-vector convention, qw5 holds the coefficients that multiply x, qw8 the translation. The 1/1024 world scale, if any, must be inside the matrix.

Then (L127 order, l.1799–1822): `Q = qw656.x / p.w` (`div Q, vf01.x, vfN.w`); `p.xyz *= Q`; `p += qw661` (screen offset x,y, z offset, fog offset); `p.w = min(p.w, qw656.z)`; `p.w = max(p.w, qw656.y)`; if clipped `p.w += qw656.w`; `ftoi4 p`. Stored as the third register per vertex. With PACKED XYZF2, X/Y are 12.4 from lanes x/y, Z = lane-z bits 4..27 = floor(z_float) (24-bit), **F = lane-w bits 4..11 = floor(w)&0xff**, ADC = lane-w bit 15. So: fog F = clamp(p.w + qw661.w, qw656.y, qw656.z), and qw656.w is almost certainly 2048.0 so the conditional add sets ADC (vertex/triangle not drawn) without disturbing F.

**Clip test** (L127/L84 only): `c = p * qw662` (per-lane guard-band scale, pre-divide) then `clipw.xyz c, c` (l.1806/1808). Per emitted vertex `fcand vi01, 0x3ffff` (last 3 judgements = the triangle this vertex completes); if any bit set the vertex gets the ADC add (l.1813–1817). `fcset 0` at every strip start (l.897/1655). No geometric clipping; whole triangles touching the guard band are dropped. L112/L102 have no clip test at all (EE guarantees fully-inside tfrags).

Sign note (inference): since F increases with p.w and GS F=0xFF means "no fog", p.w must *decrease* with distance (negative w column, with qw656.x negative to keep Q>0), or the EE relies on something I can't see. Verify against the EE's matrix builder before mirroring.

## 3. Texture coordinates

Vertex-info x,y are loaded as floats (`lq.xy`, the bits `0x45000000 + s` of the signed unpack: 2048 + s/4096 for s >= 0, 2048 + s/8192 below 0). L102/L112/L127: `st = (s − qw657.z, t − qw657.z)` (subz, l.1451/1791), Q lane forced to 1.0 (`addw.z vf13,vf00,vf00`+`addz.z`, l.1219–1223, or `maxw vf24,vf00,vf00`+`move.z`, l.1437/1478), then `st.xyz *= Q` (l.1466). So packet ST = (s'·Q, t'·Q, Q) — GS-perspective-correct, qw657.z must be 2048.0. Texture switch = tag qw660 + verbatim 5-qw ad-gif copy (TEX0 already patched by the EE). The VU also reads **the w lane of ad-gif qw 1 (TEX1)** as a float (l.988 `lq.w vf29, 1(vi08)`; FB l.135) and in the fallback program uses it as the UV bias instead of qw657.z (FB l.141–166) — the data must hold 2048.0 there; worth confirming in the extracted files.

## 4. Colours

RGBA is the odd qword after each position (written by LightTfrags), already in PACKED RGBAQ layout (low byte per lane). L112/L127 copy it verbatim (l.1470, 1837). Morph paths convert with itof0 in the pre-pass (l.112), blend as floats, and on emit do `c *= qw664; ftoi0 c` (l.1276–1277). For LOD paths to match LOD2 output, qw664 must be (1,1,1,1) unless the EE intentionally fades. Alpha is passed through untouched; fog is purely the F lane of XYZF2 (no VU-side alpha/fog interaction).

## 5. LOD / morphing

Yes, parents are used. For each LOD-01 (L18) / LOD-0 (L17) vertex i (l.180–204): own position transformed by the matrix; parent1 = `vertex_info[parent_indices[i]].w`, parent2 = `own_info.z` (both already float clip-space from the pre-pass); `t.xy = clamp(own.w·qw666.xy + qw668.xy, 0, qw657.xy)` (LOD-0 uses qw667/669); `pos = (p1+p2)·t.x + own·t.y`, same blend for colour (`mulax/maddy`, l.211–216). So the weight is linear in clip-space w (depth), with the EE choosing coefficients so 2·t.x + t.y = 1 (t.x≤0.5 presumably).

L25/L26 add (l.418–431): if `p1.w < qw666.w` **and** `p2.w < qw666.w` (sign flag of `subw.w`, 4-cycle spaced fsand), skip storing and instead **overwrite vertex i's info qword with parent1's info** (L33–L36), snapping the vertex onto parent1 — degenerate triangles rather than a midpoint.

L47/L48 ("unknown indices 2", l.679–896): iterate `count = hdr 0.w (L48) / 1.y (L47)` entries of a qword array at `hdr 2.z / 3.x`; entry.z is a position address (its parent2); `r = unk2[i]` (vertex-info address). If `pos[entry.z].w < thr` **and** `pos[vinfo[r].w].w < thr` then `entry := vinfo[r]` (`sq vf09, -3(vi02)`, l.764). So these arrays are vertex-info entries and unk-indices-2 names a replacement vertex — a second collapse rule. Whether 2.z aliases 2.y (same LOD-01 entries, different rule) or a sub-range, compare 2.z/0.w with 2.y/0.z in real headers. (Note: common entries have z=0 → pos[0] is the header → the parent test degenerates to "always true" if thr>0.)

## 6. Constants the EE must supply

**qw 5–8 (and 0x14d–0x150)**: the 4×4 transform, rows = coefficient columns as in §2 (row k multiplies coordinate k, row 3 = translation), mapping integer world coordinates to (x_clip, y_clip, z_clip, w).

**qw 656–669 (absolute)** — usage by lane:
- 656: x = perspective numerator (÷w), y = w/fog lower clamp, z = upper clamp, w = ADC add (2048.0).
- 657: x,y = morph-weight upper clamps, z = UV bias 2048.0, w unused.
- 658: vertex GIF tag template (x lane must be 0; VU writes NLOOP/EOP), NREG=3, REGS ST/RGBAQ/XYZF2.
- 659: fallback-only: tag for clipped polygons.
- 660: ad-gif tag (NLOOP=5, NREG=1, REGS=A+D).
- 661: post-divide add: x,y screen offset, z depth offset, w fog offset.
- 662: guard-band scale for the clip test (xyz vs w).
- 663: fallback-only: rescale for clipped vertices.
- 664: colour multiplier (morph paths).
- 665: fallback-only: precise clip-volume scale.
- 666/667: xy = LOD-01/LOD-0 morph slope, w = collapse threshold (w-space).
- 668/669: xy = morph intercept.
- 0x29e–0x29f: fallback-only 2-qw GS packet kicked after every packet (FB l.906+).

## 7. Vertex-stage pseudocode (WGSL mirror)

```
// per tfrag: M (qw5..8), K = consts
p_int = origin + local                      // integers
p = float(p_int)                            // itof0
c = M[3] + p.x*M[0] + p.y*M[1] + p.z*M[2]   // clip: (x,y,z,w)
col = float4(rgba)                          // from interleaved slot

// LOD-01 / LOD-0 vertices only:
t = clamp(c.w * K666.xy + K668.xy, 0, K657.xy)
c   = (P1 + P2) * t.x + c * t.y            // P1,P2 = parents' c
col = (C1 + C2) * t.x + col * t.y
if (P1.w < K666.w && P2.w < K666.w) { use parent1's info entirely }

// emit
Q  = K656.x / c.w
st = float3(s - 2048, t - 2048, 1) * Q     // s,t already 2048 + s/4096 (s >= 0) or 2048 + s/8192 (s < 0)
xyz = c.xyz * Q + K661.xyz
w   = clamp(c.w + K661.w, K656.y, K656.z)  // fog F = floor(w) & 0xff
// clip: if any of last 3 verts has |c.xyz*K662.xyz| > |c.w*K662.w| -> drop triangle
rgba = int(col * K664)                     // low byte per lane
GS: X=xyz.x*16 (12.4), Y likewise, Z=floor(xyz.z) (24-bit), F, ADC
```

## 8. Fallback program (903379)

Only two entries (init at 0 → L2, draw at 2 → L10). Draws the base (common) strips exactly like L127 — inline transform, no morphing, no parents — but replaces the ADC-drop with real clipping: on a clip flag, if the strip already has 3 verts (`vi07-2 ≥ 0`), re-test the triangle against the tight volume `c*qw665` (FB l.320–333, `fcget` ×3 ANDed with 0x3f = trivial reject), otherwise kick the current packet, write the 3 verts/colours/UVs to scratch at buffer+28..48 (FB l.337–345), run a 6-plane Sutherland–Hodgman clipper (scratch list at 0x3ce, plane stubs at 0x373–0x37d, interpolation L57), emit the polygon (≤9 verts, own tag qw659, FB L49: Q=1/w, `*qw663`, then the usual offset/clamp/ftoi4), kick it, then restart the strip (L21/L22 — bookkeeping not fully verified). Output buffers are 0x2a0/0x337 (151 qw), UV bias comes from ad-gif qw1.w, qw657 unused, and every xgkick is followed by kicking the 2-qw packet at 0x29e.

## 9. Open questions / contradictions

1. **L84 (entry 0x12) looks broken**: `addz.z vf02, vf00, vf29` (l.993) overwrites the UV bias with vf29.**z**, which was never loaded in this path (only `lq.w vf29`); the fallback's analogous code uses the w lane. Either disassembler field glitch, a genuine bug, or entry 0x12 is unused by the EE — check TfragProc's MSCAL selection.
2. Header lanes 0.w, 1.y, 2.z, 3.x (doc "unknown") are count/address pairs for the unk-indices-2 pass; 0.y, 1.w, 2.x are unused by VU1.
3. Doc §2.6: a kick record with z≥0 also loads an ad-gif; first strip must carry one.
4. Sign of w / fog direction (§2), exact PRIM/REGS literals, and vf04 (qw664) value need EE-side confirmation; where qw 656–669 and 0x29e are uploaded is not covered in render_pipeline.md.
5. Whether the "collapse to parent1" is a pop or hidden by the morph reaching the midpoint first depends on the EE's choice of qw666–669 vs thresholds.
6. XYZF2 vs XYZ2 cannot be distinguished from the microcode; the fog-lane clamping strongly implies XYZF2.

*Derived from the disassembly of program 55907 (and 903379) on 2026-09-26; used as the reference for the terrain vertex shader.*
