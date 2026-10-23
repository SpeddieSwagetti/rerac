# RAC1 tfrag terrain format

*A format specification for the `tfrags` block of Ratchet & Clank (2002, PS2; Wrench game id `rac` / RAC1), reconstructed by reading the Wrench source tree (GPL, reference only — no Wrench code is reproduced here). Every statement is attributed to the Wrench file it was derived from. Statements that are my inference rather than something Wrench asserts are marked **[inferred]**; things nobody knows are marked **[unknown]**.*

---

## 0. Sources and how to read this document

| Short name | Path in the Wrench tree | What it gave us |
| --- | --- | --- |
| `low.h` | `src/engine/tfrag_low.h` | All on-disk record layouts (block header, per-tfrag header, VU header unpack, ad-gif primitive, vertex info, position, strip, light, cube, rgba) |
| `low.cpp` | `src/engine/tfrag_low.cpp` | Block/sub-block slicing arithmetic, exact VIF command order (read + write paths), VU1 memory allocator, triangle counting |
| `high.cpp` | `src/engine/tfrag_high.cpp` | Vertex decode (position scale, UV scale, normal decode, colour decode), strip→face recovery, tface/LOD-parent logic, texture→material mapping |
| `debug.cpp` | `src/engine/tfrag_debug.cpp` | Which arrays belong to which LOD (per-LOD extraction), simple tristrip walk, LOD-parent visualisation |
| `doc` | `docs/tfrag_renderer.md` | Prose on the LOD scheme, the per-LOD VIF command tables, the VU1 data-memory map and the strip command-list pseudocode |
| `vif.h/.cpp` | `src/core/vif.h`, `src/core/vif.cpp` | VIF code decoding, unpack element sizes/packet sizes, the fixed-12 UV macro, STROW record |
| `gif.h` | `src/engine/gif.h` | GIF A+D quadword layout and GS register addresses |
| `core.h/.cpp` | `src/wrenchbuild/level/level_core.h`, `level_core.cpp` | Where the tfrags block lives in core_data, how its size is derived, the tfrag TextureEntry table |
| `tex.h/.cpp` | `src/wrenchbuild/level/level_textures.h`, `level_textures.cpp` | `TextureEntry` layout and texture decode conventions |
| `asset.cpp` | `src/wrenchbuild/level/tfrags_asset.cpp` | Round-trip test, occlusion index renumbering, export entry point |
| `chunks.cpp` | `src/wrenchbuild/level/level_chunks.cpp` | Chunk tfrag blocks, occlusion index numbering across chunks |
| `vis.cpp` | `src/wrenchvis/wrenchvis.cpp` | Occlusion mapping record layout (`bit_index`/`occlusion_id`) |
| `occl.md` | `docs/occlusion_culling.md` | Occlusion system overview |
| `rend.md` | `docs/renderers.md` | PS2 VIF/VU1/GIF/GS data-flow context, tfrag renderer provenance |

Two general cautions before the details:

* Wrench's reader/writer round-trips real RAC1 files **bit-exactly** (`asset.cpp` diffs a rewritten block against the original, including a re-derivation of every VU1 address), so the structural facts below are strongly validated. The *semantics* (what the VU1 program does with the data) are much weaker — Wrench has no VU1 disassembly in the tree, only the prose in `doc`.
* `docs/tfrag_renderer.md` contains at least one internal inconsistency (§2.6) and one mislabelled VU-header row (§2.4). Where doc and code disagree, trust the code.

---

## 1. Where the block lives, and the block-level table of contents

### 1.1 Position within core_data

The level core consists of an *index* (uncompressed header + tables) and a compressed *data* section. After decompression, the data section begins with the tfrags block. (`core.h`, `core.cpp`)

`LevelCoreHeader` (in the index, offset 0) fields relevant here:

| Ofs | Type | Field | Meaning |
| --- | --- | --- | --- |
| 0x00 | 2×s32 | `gs_ram` | ArrayRange (count, offset) |
| 0x08 | s32 | `tfrags` | Byte offset of the tfrags block in decompressed core_data (0 in practice) |
| 0x0c | s32 | `occlusion` | Offset of the occlusion block |
| 0x10 | s32 | `sky` | Offset of the sky block |
| 0x14 | s32 | `collision` | Offset of the collision block |
| 0x30 | 2×s32 | `tfrag_textures` | ArrayRange of `TextureEntry` records **in the index**, for tfrag textures |
| 0x60 | s32 | `textures_base_offset` | Base of texture pixel data within core_data |

The tfrags block has **no stored size**. Wrench derives it as the offset of the next block that exists, in the order `occlusion`, then `sky`, then `collision`; if all three are zero the size cannot be determined and Wrench errors out. (`core.cpp`) This matches the user's observation that Novalis's tfrags block runs 0 … 0x251340 (the occlusion offset).

RAC1 has no level chunks, so there is exactly one tfrags block per level, in core_data. (`docs/asset_reference.md`, `chunks.cpp`) In RAC2/3/DL the same format also appears inside each chunk file (separately WAD-compressed with the `chnktfrag` header), and the core copy is zero-padded to the largest chunk's tfrag block size — which is why a byte-exact round-trip test must strip trailing padding. (`chunks.cpp`, `asset.cpp`)

### 1.2 Tfrags block header (offset 0 of the block, 16 bytes)

(`low.h`, `low.cpp`)

| Ofs | Size | Type | Name | Meaning |
| --- | --- | --- | --- | --- |
| 0x00 | 4 | s32 | `table_offset` | Byte offset (from the start of the tfrags block) of the tfrag header table |
| 0x04 | 4 | s32 | `tfrag_count` | Number of entries in the tfrag header table |
| 0x08 | 4 | f32 | *(unnamed float)* | **[unknown]** — Wrench calls it "thingy" and only preserves it |
| 0x0c | 4 | u32 | *(unnamed word)* | **[unknown]** — Wrench calls it "mysterious second thingy" |

Alignment: Wrench's writer pads to **0x40** before the header table, so `table_offset` is 0x40 in a regenerated file and almost certainly 0x40 in the originals (the round-trip test would fail otherwise). Each tfrag's data block is then aligned to **0x10**. (`low.cpp`)

Layout of the block as a whole:

```
+0x00                tfrags block header (0x10 bytes)
+0x10 .. table_ofs   padding (zero)
+table_offset        TfragHeader[tfrag_count]      (0x40 bytes each)
                     tfrag 0 data block            (0x10-aligned)
                     tfrag 1 data block
                     ...
```

### 1.3 Per-tfrag header record (0x40 bytes)

(`low.h`; offsets confirmed by `low.cpp` read/write, and by the table in `doc`)

**Critical detail:** the `data` field at 0x10 is a byte offset **relative to `table_offset`** (i.e. relative to the start of the header table), *not* relative to the start of the block or to the header record. All the `*_ofs` fields in the record are relative to the tfrag's own data block (`data`). (`low.cpp`)

| Ofs | Size | Type | Name | Meaning |
| --- | --- | --- | --- | --- |
| 0x00 | 16 | 4×f32 | `bsphere` | Bounding sphere: centre x, y, z and radius w. Float, world space. Scale not confirmed — see §6. |
| 0x10 | 4 | s32 | `data` | Offset of this tfrag's data block, **relative to `table_offset`** |
| 0x14 | 2 | u16 | `lod_2_ofs` | Offset of the LOD-2-only VIF list within the data block. **0** in RAC1 (Wrench hardcodes 0 when writing and the round-trip passes). |
| 0x16 | 2 | u16 | `shared_ofs` | Offset of the *common* ("shared") VIF list |
| 0x18 | 2 | u16 | `lod_1_ofs` | Offset of the LOD-1-only VIF list |
| 0x1a | 2 | u16 | `lod_0_ofs` | Offset of the **LOD-0+1 ("lod_01")** VIF list — despite the name this is the shared-between-LOD0-and-LOD1 list, not the LOD-0-only list |
| 0x1c | 2 | u16 | `tex_ofs` | Offset of the **ad-gif payload** inside the common VIF list — i.e. the first byte after the `UNPACK V4_32` VIF code word |
| 0x1e | 2 | u16 | `rgba_ofs` | Offset of the per-vertex RGBA array; also the end of the last VIF list |
| 0x20 | 1 | u8 | `common_size` | Size of the common VIF list, in **quadwords** |
| 0x21 | 1 | u8 | `lod_2_size` | Size in quadwords of (LOD-2 list + common list), measured from the start of the data block |
| 0x22 | 1 | u8 | `lod_1_size` | Size in quadwords of (common + LOD-1 + LOD-01 lists), measured from `shared_ofs` |
| 0x23 | 1 | u8 | `lod_0_size` | Size in quadwords of (LOD-01 + LOD-0 lists), measured from `lod_0_ofs` |
| 0x24 | 1 | u8 | `lod_2_rgba_count` | Count of RGBA entries used by LOD 2 (`doc` marks it a count; exact unit unverified) |
| 0x25 | 1 | u8 | `lod_1_rgba_count` | Same for LOD 1 |
| 0x26 | 1 | u8 | `lod_0_rgba_count` | Same for LOD 0 |
| 0x27 | 1 | u8 | `base_only` | **[unknown]** — name suggests "this tfrag has only the base (lowest) LOD"; Wrench only preserves it |
| 0x28 | 1 | u8 | `texture_count` | Number of ad-gif (texture) primitives in the common list |
| 0x29 | 1 | u8 | `rgba_size` | Size of the RGBA array in **quadwords**; entry count = `rgba_size * 4` |
| 0x2a | 1 | u8 | `rgba_verts_loc` | **[unknown]** — name suggests where the vertex colours go in VU1 memory |
| 0x2b | 1 | u8 | `occl_index_stash` | **[unknown]** — probably a runtime scratch slot related to `occl_index` |
| 0x2c | 1 | u8 | `msphere_count` | Number of entries in the "mini-sphere" array |
| 0x2d | 1 | u8 | `flags` | **[unknown]** — Wrench preserves it and never interprets any bit. No back-face/double-sided semantics are documented anywhere in the tree. |
| 0x2e | 2 | u16 | `msphere_ofs` | Offset of the mini-sphere array |
| 0x30 | 2 | u16 | `light_ofs` | Offset of a quadword holding the tfrag origin, immediately followed by the per-vertex light/normal array |
| 0x32 | 2 | u16 | `light_end_ofs` | **RAC1/GC/UYA**: end of the light array — Wrench writes it equal to `msphere_ofs`. **DL only**: same slot instead holds `light_vert_start_ofs`, the offset of the common positions unpack payload. This is the *only* structural RAC-vs-DL difference in the record. |
| 0x34 | 1 | u8 | `dir_lights_one` | Directional light reference; Wrench writes **0xff** (i.e. "none"/invalid) and the round-trip passes, so RAC1 tfrags do not use it. |
| 0x35 | 1 | u8 | `dir_lights_upd` | Directional light reference; Wrench writes **0** |
| 0x36 | 2 | u16 | `point_lights` | Point light reference; Wrench writes **0xffff** ("none") |
| 0x38 | 2 | u16 | `cube_ofs` | Offset of the 8-corner integer cube (§1.4) |
| 0x3a | 2 | u16 | `occl_index` | Occlusion / visibility index for this tfrag (see §9) |
| 0x3c | 1 | u8 | `vert_count` | Total vertex-position count = common + lod01 + lod0 positions. Also the length of the light array. Being u8, **≤255 positions per tfrag**. |
| 0x3d | 1 | u8 | `tri_count` | Triangle count of **LOD 0** (sum over LOD-0 strips of `vertex_count - 2`). u8, so ≤255. |
| 0x3e | 2 | u16 | `mip_dist` | LOD / mip distance parameter. Preserved by Wrench, semantics **[unknown]** — presumably the distance (or squared distance) at which this tfrag switches LOD or mip level. |

### 1.4 Layout of one tfrag data block

Derived from the writer's emission order and the reader's slicing (`low.cpp`); each section is 0x10-aligned:

```
data + 0x00                          LOD-2-only VIF list      (lod_2_ofs == 0)
data + shared_ofs                    common VIF list          (common_size qwords)
data + lod_1_ofs                     LOD-1-only VIF list
data + lod_0_ofs                     LOD-0+1 ("lod_01") VIF list
data + shared_ofs + lod_1_size*0x10  LOD-0-only VIF list       (runs up to rgba_ofs)
data + rgba_ofs                      TfragRgba[rgba_size * 4]  (4 bytes each)
data + light_ofs                     tfrag origin quadword (4×s32: x, y, z, w)
data + light_ofs + 0x10              TfragLight[vert_count]    (8 bytes each)
data + msphere_ofs                   Vec4f[msphere_count]      (16 bytes each)
data + cube_ofs                      8 × (4×s16)               (0x40 bytes)
```

Exact slice expressions used by Wrench's reader (`low.cpp`), which a loader can copy verbatim in spirit:

| List | Start | End |
| --- | --- | --- |
| LOD 2 only | `lod_2_ofs` | `shared_ofs` |
| common | `shared_ofs` | `lod_1_ofs` |
| LOD 1 only | `lod_1_ofs` | `lod_0_ofs` |
| LOD 0+1 | `lod_0_ofs` | `shared_ofs + lod_1_size*0x10` |
| LOD 0 only | `shared_ofs + lod_1_size*0x10` | `rgba_ofs` (equivalently `lod_0_ofs + lod_0_size*0x10`) |

**Trailing arrays.**
* `TfragRgba` = 4 bytes: r, g, b, a. Count = `rgba_size * 4`. Indexed by **position index**, not by vertex-info index (`high.cpp`).
* The quadword at `light_ofs` is a copy of the same STROW row that the common VIF list uses as the tfrag origin (`low.cpp` writes `base_position` there). So the origin appears **twice** in the file: inside the common VIF list and at `light_ofs`. A loader may read either.
* `TfragLight` = 8 bytes, one per vertex position: `s8 unknown_0`, `s8 intensity`, `s8 azimuth`, `s8 elevation`, `s16 color`, `s16 pad`. Wrench only uses azimuth/elevation (§4).
* Mini-spheres: `msphere_count` × `Vec4f` (x, y, z, radius as floats). Purpose **[unknown]**; the name and the per-tface LOD scheme suggest per-tface bounding spheres used for culling or LOD selection **[inferred]**.
* Cube: 8 vectors of 4×s16 (0x40 bytes). Purpose **[unknown]**; 8 corners of an oriented/axis-aligned box, plausibly used for occlusion tests **[inferred]**.

### 1.5 RAC1 differences from later games

From `low.cpp`/`low.h` — Wrench uses one reader for all four games and only branches on game for the following:

1. Header slot 0x32 means `light_end_ofs` in RAC1/GC/UYA and `light_vert_start_ofs` in DL (§1.3).
2. DL additionally needs `light_vert_start_ofs` to be set to the offset of the common positions unpack payload; RAC1 does not have that pointer.
3. RAC1 has no chunks: exactly one tfrags block, in core_data, and no per-chunk padding requirement (`chunks.cpp`, `docs/asset_reference.md`).
4. Texture decode: DL tfrag textures are additionally pixel-swizzled; RAC1's are not (`tex.cpp`).

Everything else — VIF list structure, VU memory map, strips, LOD scheme, lighting arrays — is identical in Wrench's model across RAC1/GC/UYA/DL. There is no RAC1-specific field or alternate layout anywhere in `low.h`.

---

## 2. The VIF code stream inside a tfrag

### 2.1 VIF primer, as it applies here

(`vif.h`, `vif.cpp`, `rend.md`)

The five lists are raw VIF1 command lists: a sequence of 32-bit VIFcodes each optionally followed by inline payload data. Decoding a VIFcode word:

* bit 31 = interrupt; bits 30..24 = CMD; bits 23..16 = NUM (**NUM == 0 means 256**); bits 15..0 = immediate.
* UNPACK is recognised by `(CMD & 0b1100000) == 0b1100000`; the low 4 bits of CMD are the **VNVL** code (V4_32=0b1100, V4_16=0b1101, V4_8=0b1110, V3_16=0b1001, …). For unpack, bit 15 = FLG (1 = addresses are relative to VIF1_TOPS, which is what tfrags use), bit 14 = USN (0 = signed, 1 = unsigned), bits 9..0 = destination **quadword** address.
* Payload size of an unpack = `NUM * element_size`, rounded up to a multiple of 4 bytes; total packet size = 4 + that. `element_size = ((32 >> VL) * (VN + 1)) / 8`, i.e. 16 bytes for V4_32, 8 for V4_16, 4 for V4_8, 6 for V3_16.
* STROW / STCOL packets are 5 words: the code plus 4 immediate words (R0..R3). STMOD, STCYCL, NOP are 1 word.
* Tfrag lists use **STMOD mode 1 (offset/ADD mode)**: each unpacked component has the corresponding STROW row added to it as it is written into VU memory. They do **not** use STMASK/masked unpack, so a loader can ignore the unpack "m" bit.
* `STCYCL wl=1 cl=2` sets a *skipping write*: write 1 quadword, then skip 1, so consecutive unpacked elements land 2 quadwords apart. `STCYCL wl=4 cl=4` restores the normal packed write. The raw words Wrench emits are `0x01000102` and `0x01000404`; STMOD words are `0x05000001` (mode 1) and `0x05000000` (mode 0).

### 2.2 The three STROW rows used

(`low.cpp`)

| Row set | R0 | R1 | R2 | R3 | Used before |
| --- | --- | --- | --- | --- | --- |
| "indices" | `vertex_info_common_addr` | same | same | same | every index / parent-index unpack (V4_8 UNSIGNED) |
| "single vertex info" | `0x45000000` | `0x45000000` | `0` | `positions_common_addr` | the common vertex-info unpack |
| "double vertex info" | `0x45000000` | `0x45000000` | `positions_common_addr` | `positions_common_addr` | the LOD-01 and LOD-0 vertex-info unpacks |
| tfrag origin | origin x | origin y | origin z | origin w | every position unpack (V3_16 SIGNED) |

Two things fall out of this:

* Index bytes are converted, during the unpack, into **absolute VU quadword addresses of vertex-info entries**. On disk they are plain 8-bit indices into the concatenated vertex-info array (common ‖ lod01 ‖ lod0), because those three arrays are contiguous in VU memory starting at `vertex_info_common_addr`. Since they are u8, a tfrag can have at most 256 vertex-info entries.
* `0x45000000` added to the UV fields is the integer→float conversion trick: 0x45000000 is the IEEE bit pattern of 2048.0f, the vertex-info unpack is V4_16 **signed** (all 76,986 retail unpacks), and STMOD 1 adds the row as an integer, so VU1 loads `f32::from_bits(0x45000000 + s)` and subtracts 2048.0 (qw657.z, `subz`; vu1_tfrag_analysis.md §4). For s >= 0 that is `s / 4096` (mantissa step 2⁻¹²); **for s < 0 the sum drops below 2048.0's exponent, where the step is 2⁻¹³, so the result is `s / 8192`**. Wrench's `vu_fixed12_to_float(s) = s / 4096` is wrong for the 27% of retail vertices with a negative S or T: those textures show at half the frequency on the PS2 (`TfragVertexInfo::st`).

### 2.3 Command order per list

From the writer in `low.cpp` (authoritative; the tables in `doc` agree except where noted). "Payload" gives the on-disk record type.

**List 1 — LOD 2 only** (always exactly 2 unpacks):

| # | Command | Payload |
| --- | --- | --- |
| 1 | STROW (indices row) | — |
| 2 | STMOD mode=1 | — |
| 3 | UNPACK V4_8 UNSIGNED → `indices_addr` | LOD-2 index bytes |
| 4 | STMOD mode=0 | — |
| 5 | UNPACK V4_8 SIGNED → `strips_addr` | LOD-2 strip records |

**List 2 — common** (always exactly 4 unpacks; Wrench asserts both counts):

| # | Command | Payload |
| --- | --- | --- |
| 1 | UNPACK V4_16 UNSIGNED → `header_common_addr` (=0) | VU header, 20×u16 = 0x28 bytes → 5 qwords |
| 2 | UNPACK V4_32 SIGNED → `ad_gifs_common_addr` | `texture_count` × 5-quadword ad-gif primitive. `tex_ofs` points at this payload. |
| 3 | STROW (single vertex info row) | — |
| 4 | STMOD mode=1 | — |
| 5 | UNPACK V4_16 SIGNED → `vertex_info_common_addr` | common vertex info, 8 bytes each |
| 6 | STROW (tfrag origin) | the origin; Wrench reads the origin from **command index 5** of this list |
| 7 | STCYCL wl=1 cl=2 | enable the 2-quadword stride for positions |
| 8 | UNPACK V3_16 SIGNED → `positions_common_addr` | common positions, 6 bytes each |
| 9 | STCYCL wl=4 cl=4 | — |
| 10 | STMOD mode=0 | — |

**List 3 — LOD 1 only** (always exactly 2 unpacks):

| # | Command | Payload |
| --- | --- | --- |
| 1 | UNPACK V4_8 SIGNED → `strips_addr` | LOD-1 strips (overwrite LOD-2 strips) |
| 2 | STROW (indices row) | — |
| 3 | STMOD mode=1 | — |
| 4 | UNPACK V4_8 UNSIGNED → `indices_addr` | LOD-1 indices |

**List 4 — LOD 0+1 ("lod_01")** — everything here is **optional**; the reader accepts each unpack only if present and of the expected VNVL, in this order:

| # | Command | Payload |
| --- | --- | --- |
| 1 | STROW (indices row) — only if parent/unk indices present | — |
| 2 | STMOD mode=1 — if anything in this list or the LOD-0 list follows | — |
| 3 | UNPACK V4_8 UNSIGNED → `parent_indices_lod_01_addr` (optional) | LOD-01 parent indices; count = `positions_lod_01_count`, payload padded up to a multiple of 4 bytes |
| 4 | UNPACK V4_8 UNSIGNED → `unk_indices_2_lod_01_addr` (optional) | "unknown indices 2" for LOD 01 — **[unknown]** purpose |
| 5 | STROW (double vertex info row) — if vertex info follows | — |
| 6 | UNPACK V4_16 SIGNED → `vertex_info_lod_01_addr` (optional) | LOD-01 vertex info |
| 7 | STROW (tfrag origin) | — |
| 8 | STCYCL wl=1 cl=2 | — |
| 9 | UNPACK V3_16 SIGNED → `positions_lod_01_addr` (optional) | LOD-01 positions |

**List 5 — LOD 0 only** — note it *starts* with the VIF still in STMOD=1 / STCYCL 1:2 state left by list 4, which is why the first unpack can be positions:

| # | Command | Payload |
| --- | --- | --- |
| 1 | UNPACK V3_16 SIGNED → `positions_lod_0_addr` (optional) | LOD-0 positions |
| 2 | STMOD mode=0 | — |
| 3 | STCYCL wl=4 cl=4 | — |
| 4 | UNPACK V4_8 SIGNED → `strips_addr` | LOD-0 strips |
| 5 | STROW (indices row) | — |
| 6 | STMOD mode=1 | — |
| 7 | UNPACK V4_8 UNSIGNED → `indices_addr` | LOD-0 indices |
| 8 | UNPACK V4_8 UNSIGNED → `parent_indices_lod_0_addr` (optional) | LOD-0 parent indices; count = `positions_lod_0_count` |
| 9 | UNPACK V4_8 UNSIGNED → `unk_indices_2_lod_0_addr` (optional) | "unknown indices 2" for LOD 0 |
| 10 | STROW (double vertex info row) — if vertex info follows | — |
| 11 | UNPACK V4_16 SIGNED → `vertex_info_lod_0_addr` (optional) | LOD-0 vertex info |
| 12 | STMOD mode=0 | — |

There is **no MSCAL/MSCNT and no MPG** in these lists: the microprogram is uploaded elsewhere and the kick is issued by the surrounding DMA chain, which is not part of the tfrag record. **[inferred from absence]**

`doc`'s per-LOD tables describe the same sequences and additionally mark which commands are "always present" — matching the optionality above.

### 2.4 The VU header record (the "header containing addresses")

On disk: 20 × u16 = 0x28 bytes, unpacked as V4_16 UNSIGNED so each u16 becomes a 32-bit lane; 5 quadwords land at VU address 0. All addresses are **VU quadword addresses** relative to the double-buffer base; all counts are vertex counts. (`low.h`, `low.cpp`)

| Disk ofs | VU qword.lane | Field | Meaning |
| --- | --- | --- | --- |
| 0x00 | 0.x | `positions_common_count` | number of common positions |
| 0x02 | 0.y | — | **[unknown]** |
| 0x04 | 0.z | `positions_lod_01_count` | number of LOD-01 positions |
| 0x06 | 0.w | — | **[unknown]** |
| 0x08 | 1.x | `positions_lod_0_count` | number of LOD-0 positions |
| 0x0a | 1.y | — | **[unknown]** |
| 0x0c | 1.z | `positions_common_addr` | base of the interleaved positions+colours region |
| 0x0e | 1.w | `vertex_info_common_addr` | base of the vertex-info region |
| 0x10 | 2.x | — | **[unknown]** |
| 0x12 | 2.y | `vertex_info_lod_01_addr` | |
| 0x14 | 2.z | — | **[unknown]** |
| 0x16 | 2.w | `vertex_info_lod_0_addr` | |
| 0x18 | 3.x | — | **[unknown]** |
| 0x1a | 3.y | `indices_addr` | |
| 0x1c | 3.z | `parent_indices_lod_01_addr` | |
| 0x1e | 3.w | `unk_indices_2_lod_01_addr` | |
| 0x20 | 4.x | `parent_indices_lod_0_addr` | |
| 0x22 | 4.y | `unk_indices_2_lod_0_addr` | |
| 0x24 | 4.z | `strips_addr` | |
| 0x26 | 4.w | `texture_ad_gifs_addr` | equals `ad_gifs_common_addr` |

**Verified (2026-09-26, all 20,016 retail tfrags, asserted in `crates/rc-formats/tests/formats/golden.rs`):** each vertex-info tier is `[one entry per position] ++ [extra entries]`, and the "unknown" lanes describe the extra range: 0.y / 0.w / 1.y = extra count for common / LOD-01 / LOD-0, and 2.x / 2.z / 3.x = its VU address (= tier address + that tier's position count). So the common, LOD-01 and LOD-0 vertex-info counts are `positions_*_count + extra`, and the LOD-01 / LOD-0 "unknown indices 2" arrays have exactly 0.w / 1.y entries (one per extra entry). This matches the VU1 L47/L48 passes in `docs/plan/vu1_tfrag_analysis.md` §5.

`doc`'s VU-memory table places `vertex_info_part_2_addr` in lane w of quadword 2 and omits several fields; the code's mapping above is the one that round-trips, so prefer it. `doc` also confirms in prose that only the LOD-01 and LOD-0 vertex-info entries have their first "vertex data offset" populated.

### 2.5 Payload record layouts

(`low.h`; "unpacked" describes where each field ends up in the VU quadword)

**Ad-gif texture primitive — 5 quadwords (0x50 bytes), unpacked V4_32 so it is copied verbatim.** Each quadword is a GIF A+D entry: `s32 data_lo`, `s32 data_hi` (together the 64-bit register value), `u8 address` (the GS register id), then 7 padding bytes. Order and intended registers, from Wrench's field names cross-referenced with `gif.h`'s address enum:

| Qword | Register | GS address | Notes |
| --- | --- | --- | --- |
| 0 | TEX0_1 | 0x06 | On disc: `data_lo` = **texture index**, `data_hi` = 0 (§2.5.1) |
| 1 | TEX1_1 | 0x14 | On disc: `data_lo` = LOD K (s16, 1/16), `data_hi` = MMIN (4) |
| 2 | CLAMP_1 | 0x08 | On disc: `data_lo` = WMS, `data_hi` = WMT (0 repeat / 1 clamp) |
| 3 | MIPTBP1_1 | 0x34 | 0 on disc; built from the texture entry at load |
| 4 | MIPTBP2_1 | 0x36 | 0 on disc and at run time |

Wrench never validates the address bytes and only ever reads `data_lo` of quadword 0; the register identities are now confirmed by the EE code (§2.5.1). **Verified on the retail disc:** all 50,067 ad-gifs carry exactly the addresses 0x06, 0x14, 0x08, 0x34, 0x36 in that order, and the w lane (bytes 0x0c–0x0f) of the TEX1 quadword is always 0x45000000 = 2048.0f, the UV bias the VU1 program reads from there.

#### 2.5.1 Ad-gif conversion: level load and per frame (EE code, verified 2026-09-26)

The on-disc ad-gif words are packed fields, not GS register values. Two EE routines turn them into registers, in place, in the loaded tfrags block.

**Level-load init.** Boot `0x2040e0` (Lombyte `fun_002040e0`, unnamed; called from `transition_load_wad`). The level01 overlay copy is at `0x255470` and is called at `0x2584fc` from the level-core setup `0x258128`. Its arguments are `a0` = the tfrags block and `a1` = `core_index + word[0xd]`, the tfrag `TextureEntry` table (`LevelCore::tfrag_textures`). The routine does three things:

1. It sets the LOD distances from block header `+0x08` (§3b).
2. It turns each header's `data` into an absolute pointer.
3. For every tfrag and every one of its `texture_count` ad-gifs (at `data + tex_ofs + 0x50·i`), it overwrites the 64-bit data half of all five quadwords with `sd`. Address bytes and w lanes are not touched.

It reads `i = TEX0.lo` (s32) and `e = table[i]` (16 bytes). Assembly at boot `0x2041f8`–`0x204340`; `base = DAT_0015ee8c >> 8` is the level texture area's GS address in 256-byte blocks; `log2(x) = 30 − PLZCW(x)` (`0x1f97a0`); `w` and `h` are `e.width`/`e.height` as s16:

| Register | Bits | Field | Value written |
| --- | --- | --- | --- |
| TEX0_1 | 0–13 | TBP0 | 0 (patched per frame, below) |
| | 14–19 | TBW | `max(1, w >> 6)` |
| | 20–25 | PSM | 0x13 (PSMT8) |
| | 26–29 / 30–33 | TW / TH | `log2 w` / `log2 h` |
| | 34 | TCC | 1 (RGBA) |
| | 35–36 | TFX | 0 (MODULATE) |
| | 37–50 | CBP | `e.palette + base` |
| | 51–60 | CPSM, CSM, CSA | 0 (PSMCT32, CSM1, 0) |
| | 61–63 | CLD | 4 (load if CBP ≠ CBP0) |
| TEX1_1 | 0 | LCM | 0 (LOD from Q) |
| | 2–4 | MXL | `e.ty − 1` |
| | 5 | MMAG | 1 (LINEAR) |
| | 6–8 | MMIN | `TEX1.hi` (disc value 4 = LINEAR_MIPMAP_NEAREST) |
| | 9, 19–20 | MTBA, L | 0 |
| | 32–63 | K (32–43) | `TEX1.lo << 32` (whole word; see below) |
| CLAMP_1 | 0–1 / 2–3 | WMS / WMT | `CLAMP.lo` / `CLAMP.hi` (0 = REPEAT, 1 = CLAMP) |
| | 24–31 | MINV (unused by REPEAT/CLAMP) | the texture index `i`, which `PatchTfragGifs` reads back from byte 0x23 |
| MIPTBP1_1 | 0–13 | TBP1 | 0 (patched per frame) |
| | 14–19 | TBW1 | `max(1, w >> 7)` |
| | 20–33 / 34–39 | TBP2 / TBW2 | `e.mipmap + base` / 1 |
| | 40–53 / 54–59 | TBP3 / TBW3 | `e.pad + base` / 1 |
| MIPTBP2_1 | all | — | 0 |

What this establishes about the data (retail scan of 50,067 ad-gifs and 1,639 tfrag textures, 19 levels):

* **CLAMP.** The renderer's inference is correct: `clamp.data_lo` is S (WMS) and `clamp.data_hi` is T (WMT). Both are always 0 or 1, i.e. repeat or clamp. lo = 1 on 5,952 ad-gifs; hi = 1 on 17,917.
* **TEX1.** `data_hi` is 4 everywhere (MMIN = LINEAR_MIPMAP_NEAREST). `data_lo` is K written as a sign-extended s16 in 1/16 units: 0xff77 = −137 = **K −8.5625** (23,119 ad-gifs) up to 0xffa7 = −89 = K −5.5625. The GS reads K from bits 32–43 (0xf77); the extra 0xf nibble lands in the undefined bits 44–47. With LCM = 0 and L = 0, the GS mip level is `LOD = log2(1/|Q|) + K`. Q is the per-vertex Q the VU emits (`qw656.x / w`, vu1_tfrag_analysis.md §2). The mip used is the nearest level `round(LOD)` clamped to 0..MXL, with bilinear filtering inside it.
* **`TextureEntry.ty` is the mip-level count** (MXL + 1): 4 for every 64/128/256-px texture and 3 for every 32-px texture.
* **`TextureEntry.mipmap` (0x0c) is the GS block of mip level 2**, relative to the level texture base. It is never −1, so the "−1 = none" note in `texture.rs` is wrong. **`pad` (0x0e) is the block of mip level 3**; it is −1 exactly on the 46 three-level textures. Mips 2 and 3 are resident. The base level and mip 1 are paged. **Mip 1 lives right after the base level in the textures block** (`data_offset + w·h`, `w/2 × h/2`): `BuildTfragTextureDma` (boot `0x234d48`) sends the base level from the texture's source address and mip 1 from `address + 4·(w/2)²`, with `w = h = 1 << n` from the table at `0x1e0900` (all 1,639 retail tfrag textures are square). Every level uses the entry's one CLUT. Checked on all 19 levels (2026-09-26): every level of every tfrag texture decodes in bounds and each level is the 2×2 box average of the one above it (mean |level − box(previous)| = 0.74 per channel, against 23.9 for a shifted window). Rust: `texture::decode_tfrag_mip_levels`.
* `TEX0.hi`, `MIPTBP1` and `MIPTBP2` are 0 on disc.

**Per-frame patch.** `PatchTfragGifs` (boot `0x233308` = Lombyte `patch_tfrag_gifs`, level01 `0x2a7c90`; level01 = boot + 0x74988 for both tfrag routines, checked by byte comparison) walks the `(ad-gif pointer, count)` pairs at `0x1c5480`. `TfragProc` builds that list from the tfrags drawn this frame whose near distance is ≤ `mip_dist`. For each ad-gif, the routine takes `i = byte 0x23` and looks up `u16 pair[i]` in `0x1c5180`: `(TBP0, TBP1)` of the texture's paged base level and mip 1. `BuildTfragTextureDma` rebuilds that pair table every frame. A non-zero TBP0 replaces TEX0 bits 0–13, and a non-zero TBP1 replaces MIPTBP1 bits 0–13. A zero entry leaves the old value, which is safe because the GS never samples that level at that distance.

Which levels are paged is decided per texture sphere (§3b, "texture spheres"). The base level and mip 1 are paged when `near ≤ m·64`, only mip 1 when `m·64 < near ≤ m·128`, and nothing otherwise.

A renderer that keeps every texture resident can ignore TBP and CBP and use: wrap S/T from CLAMP, mag linear, min linear with nearest mip, `ty` levels, and the K bias.

**Rust:** `TfragAdGifs::{texture_index, wrap_s, wrap_t, mag_filter, min_filter, lod_k, lod_k_raw, mip_levels, gs_registers}` in `crates/rc-formats/src/tfrag.rs`. `gs_registers` reproduces the init bit for bit and is unit-tested with synthetic values.

**Vertex position — 6 bytes on disk (V3_16 SIGNED), 1 quadword in VU but written with stride 2:**

| Disk ofs | VU lane | Field |
| --- | --- | --- |
| 0x00 | x | `x` (s16) |
| 0x02 | y | `y` (s16) |
| 0x04 | z | `z` (s16) |

The w lane is not written by a V3 unpack; what it contains is **[unknown]** (whatever the previous frame left, or the unpack's fill behaviour). The tfrag origin STROW row is added to x/y/z during the unpack, so VU memory holds `origin + local` as integers.

**Vertex info — 8 bytes on disk (V4_16 SIGNED), 1 quadword in VU:**

| Disk ofs | VU lane | Field | Meaning |
| --- | --- | --- | --- |
| 0x00 | x | `s` | texture coordinate S, **signed 1/4096 fixed point**; STROW adds 0x45000000 |
| 0x02 | y | `t` | texture coordinate T, same |
| 0x04 | z | `parent` | quadword offset of this vertex's *second* LOD parent's position. STROW adds `positions_common_addr` for LOD-01/LOD-0 info, and **0** for common info (so common vertices have no meaningful parent). Position index = `parent / 2`. |
| 0x06 | w | `vertex` | quadword offset of this vertex's own position; STROW adds `positions_common_addr`. **Position index = `vertex / 2`** (positions occupy 2 quadwords each because of the 1:2 skipping write). |

**Strip record — 4 bytes on disk (V4_8 SIGNED), 1 quadword in VU:**

| Disk ofs | VU lane | Field | Meaning |
| --- | --- | --- | --- |
| 0x00 | x | `vertex_count_and_flag` | signed vertex count, biased by −128 when the strip also carries a control action; 0 terminates the list |
| 0x01 | y | `end_of_packet_flag` | 0 ⇒ load ad-gif; negative (−128) ⇒ XGKICK / end of GS packet |
| 0x02 | z | `ad_gif_offset` | quadword offset into the ad-gif array; **ad-gif index = offset / 5** |
| 0x03 | w | pad | |

**Index arrays** — plain u8, unpacked V4_8 UNSIGNED (4 per quadword). Each byte is an index into the concatenated vertex-info array. On disk each array is **padded with zeros to a multiple of 4 bytes**; the reader tolerates a parent-index array being 0–3 bytes longer than `positions_lod_XX_count` and truncates to the count. (`low.cpp`)

### 2.6 How strips + indices become triangles

The strip array is a little command list the microprogram walks to emit a GS packet. Interpretation, merging `doc`'s pseudocode with the two implementations in `high.cpp` and `debug.cpp`:

```
index_cursor = 0
active_ad_gif = none
for each strip record S in strips:
    n = S.x                       # signed
    if n <= 0:
        if n == 0: end of list; stop
        if S.y >= 0:              # 0 == "load ad-gif"
            active_ad_gif = S.z / 5
        else:                     # negative (-128) == "XGKICK, start a new GS packet"
            flush current GS packet
        n = n + 128               # remove the bias
    emit a primitive run of n vertices, consuming indices[index_cursor .. index_cursor+n-1]
    index_cursor += n
```

`doc` states "the first strip always encodes an AD GIF", and its VU memory table shows `y = 0` meaning *copy ad gifs* and `y = -128` meaning *do XGKICK*. **Caution:** `doc`'s pseudocode contradicts its own table by testing `strip[y] <= 0` for the XGKICK branch; both Wrench implementations test `>= 0` for the ad-gif branch (`high.cpp` tests `S.z >= 0`, `debug.cpp` tests `S.y >= 0`), which agrees with the table. Treat the table/code as correct and the pseudocode's comparison as a doc bug.

**Primitive topology.** The runs are triangle strips in the GS sense, but Wrench's exporter makes a distinction that a bit-accurate renderer should be aware of (`high.cpp`):

* If `n` is **even**, the run is treated as a **quad strip**: for `i = 0, 2, 4, … n-4`, one quad is emitted from indices `i+2, i+3, i+1, i+0` (in that output order — Wrench's comment shows the 1-3/2-4 → 4-1/3-2 remap).
* If `n` is **odd**, the run is treated as a plain **triangle strip**: for `i = 0 … n-3`, a triangle from indices `i, i+1, i+2`.

`debug.cpp`'s simpler path ignores the parity and always walks the run as a GS triangle strip (`i-2, i-1, i` for `i ≥ 2`) without alternating the winding — which is what the hardware actually does. The quad interpretation in `high.cpp` exists purely so that the exported mesh has quads. For an exact-output renderer, emit `GS_PRIMITIVE_TRIANGLE_STRIP` runs of `n` vertices and let the GS handle winding. The VU1 program writes each run behind its own GIF tag with NLOOP = `n` (L79 `isw.x vi12, -1(vi06)`), and walking every run as a strip of `n − 2` triangles reproduces the header's `tri_count` on every retail tfrag (§9b).

No back-face or double-sided flag is encoded in the strip record, and `high.cpp` has to *repair* winding order after the fact using vertex normals (§8), which strongly suggests the strips as stored do **not** carry a consistent winding and that the game either relies on GS defaults (no culling, or culling configured globally) or on data that Wrench has not identified. **[unknown]**

**Verified against the VU1 program (2026-09-26; `work/vu/55907_tfrag.txt` L79–L84, second buffer L93/L122–L125; the fallback program 903379 L4–L7 is identical).** This supersedes the pseudocode above. The walk implemented by `tfrag_triangles` (Rust and C++ oracle) is:

```
record 0:  always load ad-gif z/5; n = x + 128             # L84/L93: no test of x or y
record k>0:
  x > 0:   plain run of x vertices                         # L79 iblez
  x == 0:  end of list (kick the packet)                   # L80 ibeq -> L83
  x < 0:   n = x + 128
           y >= 0:  load ad-gif z/5 into the open packet   # L80 fallthrough
           y <  0:  XGKICK; then if z >= 0 load ad-gif z/5 # L81 ibgez vi13, L82
                    else keep the current texture          # z = -1 on disc
```

`z` is the quadword offset of the 5-qword ad-gif (`adgif_base + z`), sign-extended because the strip unpack is V4_8 signed, so at most 25 ad-gifs are addressable. A kick with `z < 0` starts a packet without A+D registers, so the GS keeps the last TEX0/TEX1/CLAMP/MIPTBP values. The walkers reject a load whose `z` is negative, not a multiple of 5, or past the ad-gif array; retail data has none.

**Retail counts (19 levels, all three LODs):** 137,980 plain load records (`y ≥ 0`, all `z ≥ 0`, all multiples of 5) and 28,838 kick records, of which **12,221 load an ad-gif** (`z ≥ 0`) and the other 16,617 carry `z = −1` (not −128). Every list's first record has `x < 0, y ≥ 0`, so the unconditional first-record load and the general rule agree on retail data. Before this was fixed, the walkers loaded ad-gifs only on `y ≥ 0`; the fix reassigns the texture of **198,052 triangles** (LOD 0/1/2: 171,578 / 13,806 / 12,668 of 1,090,454 / 404,152 / 379,524). `tfrags_for_every_level` checks the counts and that every triangle's ad-gif index is in range.

On retail data every list's first record is an ad-gif load. The strip and index arrays of all three LODs share one VU address, so the list drawn is the one the chosen draw mode transferred (§3b). The LOD-01/LOD-0 vertex-info entries a strip references may have been rewritten by the collapse pass (§3b); that turns their triangles into degenerate ones.

### 2.7 VU1 memory map and the allocator

`doc`'s table plus Wrench's re-allocator (`low.cpp`, function that recomputes every address and is validated byte-exactly against retail data). One buffer is **0x148 quadwords (328)**; `doc` says the renderer double-buffers, so two such buffers plus the GS output buffers live in VU1's 1024 quadwords (leaving 368 quadwords for outputs and code-side scratch **[inferred arithmetic]**). All unpack addresses have FLG=1, i.e. they are relative to VIF1_TOPS — the buffer base — so the same lists work for both buffers, and the BASE/OFFSET setup lives outside the tfrag data.

Allocation order (each item's size in quadwords):

| Region | Size (quadwords) | Notes |
| --- | --- | --- |
| VU header | 5 | at address 0; from the file |
| Transform matrix | 4 | **not** in the file — written per tfrag per frame by the EE |
| Ad-gifs | `5 × texture_count` | from the file |
| Common positions (+ colour slots) | `2 × positions_common_count` | positions at even offsets, colours at odd |
| LOD-01 positions (+ colour slots) | `2 × positions_lod_01_count` | |
| LOD-0 positions (+ colour slots) | `2 × positions_lod_0_count` | |
| *slack* | variable | gap between the end of the positions region and the vertex-info region. Wrench must store this per tfrag ("positions_slack") to reproduce retail addresses, and has no explanation for it. **[unknown]** |
| Common vertex info | `positions_common_count` + extra (header 0.y) | **corrected:** not 1 per position; see §2.4 verified note |
| LOD-01 vertex info | `positions_lod_01_count` + extra (header 0.w) | |
| LOD-0 vertex info | `positions_lod_0_count` + extra (header 1.y) | |
| LOD-01 parent indices | `ceil(count/4)` | |
| LOD-01 unknown indices 2 | `ceil(count/4)` | |
| LOD-0 parent indices | `ceil(count/4)` | |
| LOD-0 unknown indices 2 | `ceil(count/4)` | |
| Indices | `ceil(max(lod0, lod1, lod2 index count)/4)` | **shared address across all three LODs** |
| Strips | `max(lod0, lod1, lod2 strip count)` | **shared address across all three LODs** |

Total must be ≤ 0x148. The colours are *not* in the file's VIF lists at all: `doc` explicitly says that everything in the map comes from the command lists "with the exception of the matrix and colours". So per-vertex colour quadwords are written into the interleaved slots by the engine each frame (from the RGBA array plus lighting), and the matrix likewise. **[this is Wrench's statement plus inference about the mechanism]**

---

## 3. LOD

(`doc`, `debug.cpp`, `low.cpp`)

Conceptual model, per `doc`: a tfrag model is built from patches of faces called **tfaces**, which can be reduced in a predetermined way — a 4×4 patch of quads reduces to 2×2 and then to a single quad. The reduced versions are stored so that data is *not duplicated* across LODs, which also enables smooth interpolation between LODs and prevents cracks at LOD boundaries. Each tfrag has exactly **3 LODs; LOD 0 is highest quality, LOD 2 lowest**.

Concretely, the data is partitioned into three tiers:

| Tier | Positions | Vertex info | Parent indices | Strips + indices |
| --- | --- | --- | --- | --- |
| common (needed by all LODs) | `common_positions` | `common_vertex_info` | — (common verts have no parents) | — |
| LOD 0+1 | `lod_01_positions` | `lod_01_vertex_info` | `lod_01_parent_indices`, `lod_01_unknown_indices_2` | — |
| LOD 0 | `lod_0_positions` | `lod_0_vertex_info` | `lod_0_parent_indices`, `lod_0_unknown_indices_2` | — |

and three per-LOD strip/index pairs (LOD-2, LOD-1, LOD-0) which occupy the *same* VU addresses — later LODs overwrite earlier ones.

Which arrays each LOD uses (`debug.cpp`, which builds one mesh per LOD):

| LOD | Positions | Vertex info | Strips / indices |
| --- | --- | --- | --- |
| 2 | common | common | LOD-2 |
| 1 | common ‖ lod01 | common ‖ lod01 | LOD-1 |
| 0 | common ‖ lod01 ‖ lod0 | common ‖ lod01 ‖ lod0 | LOD-0 |

**Shared across LODs:** the VU header, the ad-gif/texture array, the tfrag origin, the RGBA array, the light/normal array, the mini-spheres, the cube, and the VU addresses of the strip and index arrays.

**How the game transfers each LOD** — implied by the four size fields (`low.cpp`); Wrench never says it outright, so **[inferred, but the size definitions leave little room for doubt]**:

| LOD | DMA transfer(s) |
| --- | --- |
| 2 | one transfer: `data + 0` for `lod_2_size` quadwords (LOD-2 list then common list) |
| 1 | one transfer: `data + shared_ofs` for `lod_1_size` quadwords (common, LOD-1, LOD-0+1 lists) |
| 0 | two transfers: `data + shared_ofs` for `common_size` quadwords, then `data + lod_0_ofs` for `lod_0_size` quadwords (LOD-0+1 then LOD-0 lists) |

**LOD selection** is not described by Wrench. `mip_dist` in the header is the obvious candidate, and `doc`'s remark about smooth interpolation implies the VU blends a LOD-0/LOD-01 vertex toward the midpoint of its two parents as the LOD fades. The per-vertex parent pair supports exactly that: `parent_indices[i]` gives one parent (as a vertex-info index) and the vertex-info `parent` field gives the other (as a position address). **[inferred; Wrench's "poles" debug mode visualises both parent links, and `high.cpp` treats them as the two endpoints of the edge the vertex subdivides]**

**Superseded by §3b**, which derives LOD selection, the morph weights and the parent arrays from the EE and VU1 code.

---

## 3b. LOD selection and morphing (EE + VU1 code, verified 2026-09-26)

Sources:

* **LOD distances.** The level-load tfrag init (boot `0x2040e0`, level01 `0x255470`) and `set_tfrag_dists` (boot `0x233068`, Lombyte's name; level01 `SetTfragDists` `0x2a79f0`, byte-compared; also called every frame from `UpdateViewContext` `0x219580`).
* **Per-tfrag choice.** `TfragProc` (boot `0x233fb0`, level01 `0x2a7e58`).
* **Morphing.** The tfrag VU1 program `55907` (EE `0x103578`), subroutines L12–L36 and L47–L59.

Addresses below are boot addresses unless noted. The level01 copies are the same code with data shifted by +0xc0 (e.g. boot `0x160ea0` = level01 `0x160f60`).

### 3b.1 Distances

The block header float at `+0x08` (called `unknown_8` so far) is the **LOD base distance L** in world units. Retail values per level: 12, 15, 16, 18, 20 or 32. The init stores three switch distances at `0x160ea0`:

```
D0 = 6L   (LOD1 -> LOD2)      D1 = 4L   (LOD0 -> LOD1)      D2 = 2L   (LOD0 starts morphing)
```

`set_tfrag_dists` then derives two sets of values from them:

* integer thresholds `Di_raw = trunc(Di * 1024)` at `0x160eb0` (raw units, for `TfragProc`);
* for the VU, `f0 = D0·s`, `f1 = D1·s`, `f2 = D2·s`, where `s = DAT_0016d0e0` (level01) is the w-per-world-unit slope computed in `UpdateViewContext`.

The VU1 path uses clip-space w, which is linear in view depth, `w = s·depth`. That `s` is negative (w decreases with distance, as the fog lane needs; the static snapshot of the block has `f0 = −10.71`) is inferred from the fog maths and that snapshot, not measured at run time. The fog intercept is added after the divide (`qw661.w`) and does not enter the LOD maths. With `a = 1/(f0 − f1)` and `b = 1/(f1 − f2)`, it zeroes VU qw 666–669 (boot `0x1de7f0`, level01 `0x1c2d70`) and writes:

| VU qw | x | y | z | w |
| --- | --- | --- | --- | --- |
| 666 (LOD-01 slope) | `a/2` | `−a` | 0 | `f0` (LOD-01 collapse threshold) |
| 667 (LOD-0 slope) | `b/2` | `−b` | 0 | `f1` (LOD-0 collapse threshold) |
| 668 (LOD-01 intercept) | `−f1·a/2` | `f0·a` | 0 | 0 |
| 669 (LOD-0 intercept) | `−f2·b/2` | `f1·b` | 0 | 0 |

The constant block's source is 17 quadwords at boot `0x1de740` / level01 `0x1c2cc0`: a DMA CNT tag, STCYCL 4/4, UNPACK V4_32 of 15 qw to VU address 656, then `MSCAL 0; BASE 0; OFFSET 0x148`. `TfragProc` copies the whole block into the head of the tfrag chain every frame. qw 657 is `(0.5, 1.0, 2048.0, 0)`, so the morph clamps are 0.5 and 1.0.

### 3b.2 Per-tfrag draw mode (`TfragProc`, boot `0x2341dc`–`0x234568`)

For each tfrag header the routine computes:

* `d = R·(bsphere.xyz − camera·1024)`, where R is the camera rotation and `bsphere` is in raw units (§6);
* `far = int(d.z + r)` and `near = int(d.z − r)`, where `r = bsphere.w` (raw) and `int` is `vftoi0`, i.e. truncation.

It then culls against the frustum. A tfrag whose sphere crosses the guard band is re-tested with the 8 clip-box corners at `cube_ofs` (corner = s16 × 64, absolute raw units). If any corner is outside the guard band, the tfrag goes to the clipping program (`0x107028`, MSCAL 2) at **full LOD 0 with no morphing**. Otherwise the first matching row of this table picks the mode (compared against `Di_raw`):

| Condition (first match) | MSCAL | VU1 entry | DMA'd lists | Meaning |
| --- | --- | --- | --- | --- |
| `base_only` (hdr 0x27) ≠ 0, or `near ≥ D0` | 6 | L2 → L127 | LOD-2 list (`lod_2_ofs`, `lod_2_size` qw), rgba `lod_2_rgba_count` | LOD 2, no morph |
| `far ≥ D0` | 8 | L3: L12, L26, L48, L102 | LOD-1 list (`shared_ofs`, `lod_1_size`), rgba `lod_1_rgba_count` | LOD 1, LOD-01 morph + collapse at D0 |
| `near ≥ D1` | 0xa | L4: L12, L18, L102 | LOD-1 list | LOD 1, LOD-01 morph |
| `far ≥ D1` | 0xe | L6: L12, L18, L25, L47, L102 | common (`shared_ofs`, `common_size`) + LOD-0 (`lod_0_ofs`, `lod_0_size`), rgba `lod_0_rgba_count` | LOD 0, LOD-01 morph, LOD-0 morph + collapse at D1 |
| `far ≥ D2` | 0x10 | L7: L13, L17, L102 | LOD-0 lists | LOD 0, LOD-01 at rest, LOD-0 morph |
| otherwise (`far < D2`) | 0x14 | L9 → L127 | LOD-0 lists | LOD 0, no morph |

The choice is **per tfrag** from its bounding sphere's view-depth interval, so a morphing mode is used whenever any part of the tfrag lies in a fade band. A tfrag switches abruptly between modes, but the geometry is continuous across the switch: at the boundaries the morph weights are exactly 0 or 1 (see 3b.3).

In every packet the colour transfer uses `QWC = lod_N_rgba_count >> 2`, VIF `NUM = (lod_N_rgba_count >> 2) << 2` and unpack address `rgba_verts_loc` (hdr 0x2a).

**Texture paging.** A tfrag also joins the texture-paging and `PatchTfragGifs` lists (§2.5.1) when `mip_dist − near ≥ 0` (`mip_dist` in raw units).

### 3b.3 Morph (VU1 L17/L18; L25/L26 add the collapse test)

For every **primary** entry `k` of a tier (the first `positions_lod_01_count`, VU hdr 0.z, or the first `positions_lod_0_count`, hdr 1.x, entries of that tier's vertex info):

```
e      = vertex_info[tier_start + k]
own    = position[e.vertex / 2]                  (transformed inline: c = M·own)
p1info = vertex_info[parent_indices_tier[k]]     (byte k of the V4_8 array; + vertex_info_common_addr by STROW,
                                                  so it indexes the whole concatenated vertex_info)
P1     = position[p1info.vertex / 2]             (already transformed, and already morphed in this frame)
P2     = position[e.parent / 2]                  (likewise)
t.xy   = clamp(c.w * qw66x.xy + qw66y.xy, 0, (0.5, 1.0))  = (u/2, 1-u)
c'     = (P1 + P2)·t.x + c·t.y                   = lerp(c, midpoint(P1, P2), u)
col'   = (C1 + C2)·t.x + col·t.y                 (same blend of the itof0 vertex colours)
```

The result is written back into `own`'s position/colour slot. Every vertex-info entry that shares that position (the primary and any extras) therefore sees the morphed position. **UVs are not morphed.** In world-depth terms, with `depth` the vertex's own view depth:

```
u_LOD01 = clamp((depth − D1) / (D0 − D1), 0, 1) = clamp((depth − 4L) / 2L, 0, 1)
u_LOD0  = clamp((depth − D2) / (D1 − D2), 0, 1) = clamp((depth − 2L) / 2L, 0, 1)
```

Verified on every retail tfrag:

* LOD-01 parent 1 is always a common entry, and parent 2 always a common position.
* LOD-0 parents are always common or LOD-01. In mode 0xe, LOD-0 vertices therefore blend toward LOD-01 parents that have already been morphed this frame.
* Vertex-info tiers are contiguous in VU memory (`vertex_info_lod_01_addr = common_addr + vinfo_common`, and so on).

**Collapse** (modes 8 and 0xe only, L28/L33–L36) applies to primary entry `k` when `P1.w < thr` and `P2.w < thr`, i.e. both parents are at depth > D0 (LOD-01, `thr = qw666.w = f0`) or > D1 (LOD-0, `thr = qw667.w = f1`). Then:

* the morphed position is **not** stored;
* the vertex-info quadword `e` itself is overwritten with `p1info` (UV, parent and position pointer).

The strips then reference parent 1 with parent 1's UV, and the triangles through that vertex become degenerate. This fixes UV and colour seams that the midpoint position alone (u = 1) would leave.

**Extra entries** (L47/L48; counts `unk_06`/`unk_0a` = hdr 0.w/1.y; entries at hdr 2.z/3.x = right after each tier's primaries) are not morphed, because they share a primary's already-morphed position slot. They have their own collapse rule with the same test: `P2 = position[e.parent/2]`, `P1 = position[vertex_info[unk_indices_2_tier[j]].vertex/2]`. If both have `w < thr`, `e := vertex_info[unk_indices_2_tier[j]]`. So **`unk_indices_2` is the parent-1 array of the extra entries.** Retail extras can point at common positions (64 LOD-01 and 869 LOD-0 cases), and their parents often differ from those of the primary that owns the same position.

The collapse is a per-vertex step at exactly D0 (or D1), inside a tfrag that straddles that distance. Beyond it the tfrag as a whole switches to the coarser list, where the vertex does not exist.

### 3b.4 Rust helpers (`crates/rc-formats/src/tfrag.rs`)

| Helper | What it gives |
| --- | --- |
| `TfragBlockHeader::lod_distances` | `[D0, D1, D2]` |
| `TfragBlockHeader::lod_thresholds_raw` | `Di_raw` |
| `parse_tfrag_block_header` | the block header |
| `Tfrag::draw_mode(centre_depth_raw, thresholds)` | a `TfragDrawMode` with `mscal()` and `lod()` |
| `TfragMorphTier::{weight, collapse_distance}` | `u` and the collapse distance |
| `Tfrag::lod_link(vinfo)` | tier, whether the entry morphs, own/parent-1/parent-2 positions, and the collapse replacement entry |
| `Tfrag::morph_parents(vinfo)` | `(parent1_position, parent2_position)` for primary entries |

A renderer that evaluates the morph per vertex from world depth, instead of from w, matches the VU exactly when `w = s·depth` holds.

### 3b.5 Texture spheres (the former "mini-spheres")

The `msphere_count` records at `msphere_ofs` are texture-usage spheres, read by `ComputeTfragTextureUsage` (boot `0x234bd8`, level01 `0x2a8a80`). Each record is 0x10 bytes:

| Offset | Type | Field |
| --- | --- | --- |
| 0x00 | 3 × f32 | centre (absolute raw units) |
| 0x0c | u16 | radius (raw) |
| 0x0e | u8 | `m` |
| 0x0f | u8 | texture index (always one of the tfrag's ad-gif textures) |

For each sphere in view, the routine compares `near = depth − radius` against `m`:

* `near ≤ m·64`: mark the texture −1 (upload the base level and mip 1);
* `near ≤ m·128`: mark it 1 (mip 1 only).

`msphere_count ≥ texture_count` on every retail tfrag. `mip_dist − m·128` lies in `[0, 256)` for every sphere (m is capped at 255, so `mip_dist` tops out near 32,768 raw). Rust: `Tfrag::texture_spheres`.


### 3b.6 LOD in the port (`crates/rc-engine`, 2026-09-26)

* **Draw mode.** `tfrag_lod::tfrag_proc` replays `TfragProc` per tfrag per frame on the CPU. It uses the sphere test (camera-space centre via `0x186f40`, cull at `near > 512000` (`0x160ec0`, set by the level init `0x1e9b10`) or `far ≤ n`, side planes `tan·z − |x| ± r·√(1 + tan²)` with `0x18cee0 = 1/cos(atan tan)`) and "wholly inside" = both side margins ≥ 0, `far < f`, `near ≥ n` (`0x18cef0`). Otherwise it runs the 8-corner CLIP test with `0x187040` = the view frustum as a clip volume (`w = z/n`), unscaled and with xy × `0x160e60` = (0.25, 0.25), i.e. the 4× guard band. All corners outside one unscaled plane culls the tfrag. Any corner outside the guard volume sends it to the clipping path (LOD 0, no morph). Otherwise `Tfrag::draw_mode` applies. The result goes into a storage buffer, one u32 per tfrag. Occlusion culling (hdr `0x3a`/`0x3b` against the bitmap at `0x70003b00`) is not modelled.
* **Geometry.** All three strip lists are in the meshes, batched per (texture, CLAMP). A vertex carries a (vertex-info, tfrag | list << 16 | TEX1 K << 18) reference. The vertex shader drops vertices of lists the mode does not draw. It then evaluates L18/L17 (morph) and L26/L25/L48/L47 (collapse) from static per-position and per-vertex-info storage buffers. It uses the VU's own constants qw666–669 (computed as `SetTfragDists` does) and the VU w (`depth_raw · 0x18cdec`). The morph blends world positions, which equals the VU's clip-space blend because the transform is affine. Colours are truncated like `ftoi0`. A collapsed entry takes parent 1's UV and position slot. Verified on the disc: no `unk_indices_2` target lies in the tier being rewritten, so the collapse passes never read an entry the same pass overwrote.
* **Mips.** Each texture is uploaded with its full chain. The fragment shader picks `level = clamp(round(log2(z/32) + K), 0, MXL)`: GS `LOD = log2(1/|Q|) + K` with `Q = n/z` (z = per-pixel camera depth in raw units), MMIN = LINEAR_MIPMAP_NEAREST. It then samples that level bilinearly with `textureSampleLevel`. K is per ad-gif (49 distinct values on the disc), so it travels per vertex. The GS rounding of fractional LOD is taken as round-to-nearest **[inferred from the GS manual's "nearest" mip rule, not traced]**.
* Debug: `RC_LOD=0` forces MSCAL 0x14 (LOD 0, no morph); `RC_LOD_TINT=1` tints LOD 1 red, LOD 2 blue and the clipping path green.

---

## 4. Lighting and colour

(`low.h`, `high.cpp`, `doc`)

**Per-vertex RGBA array** (`rgba_ofs`, `rgba_size * 4` entries of 4 bytes, indexed by *position index*):

* R, G, B: Wrench copies them through unchanged to the exported vertex colour. Note that PS2 GS vertex colours are conventionally 0x80 = 1.0 for texture modulation, and Wrench applies no such scaling to RGB — so an exact renderer should verify empirically whether RGB is 0…0x80 (modulate-by-2) or 0…0xff. **[open question]**
* A: the file uses the GS convention 0x80 = opaque. Wrench converts with `a < 0x80 ? a*2 : 255` — exactly the same conversion it applies to texture alpha (`tex.cpp`), which corroborates the convention.
* These colours are *not* in the VIF lists. They occupy the odd quadwords interleaved with positions in VU1 memory, and `doc` states the colours (like the matrix) are the one part of the VU map that does not come from the command lists. So the engine writes them per frame — presumably after recomputing lighting on the EE/VU0. **[inferred]**
* `lod_2/1/0_rgba_count` presumably say how many colour entries must be uploaded for each LOD, and `rgba_verts_loc` presumably says where. Neither is confirmed. **[unknown]**

**Per-vertex light/normal array** (`light_ofs + 0x10`, `vert_count` entries of 8 bytes, indexed by position index):

| Ofs | Type | Field | Wrench's understanding |
| --- | --- | --- | --- |
| 0x00 | s8 | unknown_0 | **[unknown]** |
| 0x01 | s8 | `intensity` | **[unknown]**, unused by Wrench |
| 0x02 | s8 | `azimuth` | normal azimuth, **π/128 radians per unit** (full s8 range = one turn) |
| 0x03 | s8 | `elevation` | normal elevation, same scale |
| 0x04 | s16 | `color` | **[unknown]**, unused by Wrench |
| 0x06 | s16 | pad | |

The normal is stored in **spherical coordinates** and reconstructed as

```
nx = cos(azimuth) * cos(elevation)
ny = sin(azimuth) * cos(elevation)
nz = sin(elevation)
```

`high.cpp`'s comments add two facts about the runtime: the game uses a **cosine/sine lookup table at the top of the scratchpad** for this conversion, and the conversion itself **runs on VU0**, not VU1. So per-vertex lighting (normal → colour) happens on the EE side each frame, feeding the colour quadwords described above. That is consistent with the header's `dir_lights_one`, `dir_lights_upd`, `point_lights` fields being *references to light sources* rather than baked data — Wrench writes 0xff / 0 / 0xffff for them and retail RAC1 data round-trips, so RAC1 tfrags apparently reference no explicit per-tfrag light. **[Wrench's write-path constants are a fact; the interpretation is inferred]**

There is **no per-vertex light index** anywhere in the format as Wrench models it. The nearest thing is the per-vertex normal + the header's light references.

Ambient/directional interaction: Wrench documents nothing beyond the above. **[unknown]**

### 4.1 Verified from the game's `LightTfrags` (2026-09-26)

Full derivation and math: `docs/plan/tfrag_lighting.md`; port: `crates/rc-formats/src/tfrag_light.rs`.
Checked on every tfrag of all 19 NTSC-U levels by `crates/rc-formats/tests/formats/tfrag_light_golden.rs`.

| Ofs | Type | Field | Meaning (from the EE code) |
| --- | --- | --- | --- |
| 0x00 | u16 | `pos_ofs` | byte offset of this vertex's position (3 x s16) from the tfrag data start; used only by the point-light pass (Wrench's `unknown_0`/`intensity` bytes) |
| 0x02 | u8 | `azimuth` | index into a 256-entry `(cos, sin)` f32 table, **unsigned** |
| 0x03 | u8 | `elevation` | same table |
| 0x04 | u16 | `color` | base (ambient) colour, 5:5:5:1 expanded by PEXT5 (`c5 << 3`, alpha bit 15 -> 0x80); set in every retail vertex |
| 0x06 | u16 | `select` | directional light set: bits 8..15 zero -> set `bits 0..3`; else blend set `bits 0..3` with set `bits 4..7` by `t = bits 8..15 / 256` |

* The stored RGBA array is **overwritten** by the pass (level load for all tfrags, then every frame for visible ones); its on-disc contents are never shown. Lit RGB is 0..0xff with 0x80 = 1.0 under GS MODULATE.
* The stored normal is decoded as `-(cos az cos el, sin az cos el, sin el)` and dotted with light directions; the table values are not IEEE `cos`/`sin`, so they come from the executable (boot ELF 0x165500, level01 overlay 0x166500).
* Header 0x34 (`dir_lights_one`) as a signed byte: < 0 (0xff in all retail data) = per-vertex `select`; >= 0 = one set for the whole tfrag. 0x35 is a flag cleared by the pass. 0x36 is a list of up to 4 point-light slot nibbles (0xf = end); the loader resets it to 0xffff and `CreatePointLight` fills it at run time.

---

## 5. Texture binding

(`high.cpp`, `debug.cpp`, `low.h`, `core.cpp`, `tex.h`)

* A tfrag carries an array of `texture_count` **ad-gif primitives** in its common VIF list (5 quadwords each, §2.5), unpacked verbatim into VU1 memory. `tex_ofs` in the header points straight at this payload in the file.
* Quadword 0 is the TEX0_1 A+D entry, and in the file its `data_lo` word contains a small integer: the **index into the level's tfrag texture table**, i.e. into `LevelCoreHeader.tfrag_textures` (an ArrayRange of `TextureEntry` records living in the *index*, not in core_data). Wrench establishes the link by computing `max(data_lo)+1` over all tfrags as the material count and then matching that against the `i`-th `TextureEntry` unpacked from `header.tfrag_textures`, naming both `i.png` (`high.cpp` + `core.cpp`).
* Because a texture *index* sits where a GS TEX0 register value belongs, the loader must be patching these quadwords at level-load time with real TBP/TBW/PSM/CBP values once the textures have been uploaded to GS RAM. That is also why a separate `tex_ofs` pointer exists. **[inferred — Wrench neither states nor implements this]**
* `TextureEntry` (0x10 bytes, in the index): `s32 data_offset` (relative to `textures_base_offset` in core_data), `s16 width`, `s16 height`, `s16 type`, `s16 palette` (× 0x100 = byte offset in the GS-RAM block), `s16 mipmap`, `s16 pad`. Pixels are 8-bit paletted, `width*height` bytes; the palette is 256 × u32 and is **swizzled** (swap the middle two bits of the index: `0b00010000 ↔ 0b00001000`); texture alpha uses the 0x80 = opaque convention. RAC1 pixel data is *not* additionally swizzled (only DL is). (`tex.cpp`, `docs/textures.md`)
* **Texture switching inside a tfrag** is encoded in the strip list: a strip record whose `x` is biased (≤0, ≠0) and whose `y` is ≥ 0 carries `z = ad_gif_offset`, and the ad-gif index is `z / 5`. All following runs use that ad-gif until another switch. `doc` notes the first strip always carries an ad-gif, so the initial texture is always set explicitly. In terms of GS work, "process the ad-gif" means copying those 5 quadwords into the GIF packet being built, so a texture switch costs a 5-register A+D block.
* Only TEX0's `data_lo` is interpreted by Wrench. TEX1 (filtering/LOD), CLAMP (wrap mode) and the two MIPTBP registers are carried through untouched; a faithful renderer must decode them itself, and should expect `CLAMP_1` to be the authority on repeat-vs-clamp per strip. **[inferred from register identity]**

---

## 6. World transform and coordinate conventions

(`high.cpp`, `debug.cpp`, plus engine-wide conventions in `tie.cpp`, `moby_low.cpp`, `sky.cpp`, `shrub.cpp`, `renderer.h`, `collada.cpp`)

* Each tfrag has a **per-tfrag integer origin**: the 4×s32 STROW row used before the position unpacks (and duplicated at `light_ofs`). Position components are s16 *offsets* from that origin, and the VIF adds them during the unpack.
* World position in Wrench's float world space:
  `world = (origin.xyz + local.xyz) / 1024.0`
  i.e. **1024 integer units = 1 world unit**. This is the same scale the whole engine uses (ties multiply by `scale/1024`, moby/sky vertices divide by 1024).
* The s16 range therefore gives a tfrag a local extent of ±32 world units, and the s32 origin covers the whole level.
* No per-tfrag rotation or scale exists in the data. The 4×4 matrix in VU1 memory is supplied by the EE at draw time (it is not in the file) and is presumably the world→screen matrix, including the 1/1024 scale and any perspective/VU-clipping scaling. **[inferred]**
* **Coordinate system: Z-up, right-handed** — `renderer.h` states the games use Z-up (OpenGL Y-up requires a conversion), the COLLADA writer emits `<up_axis>Z_UP</up_axis>`, and the tfrag normal reconstruction puts `sin(elevation)` in Z.
* **`bsphere` is in raw position units (1024 = 1 world unit), absolute, not origin-relative.** Divide by 1024 for world units, radius included. This corrects an earlier "world units" inference. Evidence:
  * `TfragProc` subtracts `camera · 1024` from it (boot `0x234058`: `vf28 = cam * 1024.0`);
  * the renderer measured it;
  * on every retail tfrag, `|bsphere.xyz − origin|` is under 13·1024 and the radius under 32·1024.

  The same holds for the texture-sphere centres and radii (§3b.5). The clip-box corners at `cube_ofs` are s16 × 64 raw units.
* UV convention: `s`, `t` are signed 1/4096 fixed point (`vif.h`'s fixed-12 macros, used by `high.cpp`). One texture repeat = 4096. Note the export hack in §8 concerning negative UVs.

---

## 7. The VU1 microprogram, as Wrench understands it

Everything here comes from `doc` (plus the two comments in `high.cpp`). Wrench contains **no VU1 disassembly**, does not emulate the microprogram, and does not render tfrags itself (the editor displays the exported COLLADA mesh instead — `editor/level.cpp`). So this section is the weakest part of the spec.

**Wrench's stated understanding:**

1. The renderer is Naughty Dog's, written for Jak & Daxter and ported to Ratchet & Clank; it draws most large non-instanced geometry. Each level has one world-space tfrag model plus one per loaded chunk. (`doc`)
2. The tfrag data is transferred into VU1 data memory by the VIF command lists described above, into a double-buffered 0x148-quadword region, with addresses relative to VIF1_TOPS. (`doc`, `low.cpp`)
3. The VU1 program reads: the 5-quadword header (counts + addresses), the matrix at header+5, the interleaved positions/colours, the vertex-info table (UVs + parent/position addresses), the index array, the parent/unknown index arrays, the strip command list, and the ad-gif blocks. (`doc`)
4. It walks the **strip command list** (§2.6) to build a GS packet: loading ad-gif register blocks on demand, XGKICKing to flush a packet, and terminating on a zero entry. (`doc`)
5. Generic PS2 behaviour, stated in `rend.md` rather than measured: a VU1 microprogram "will usually apply a matrix transformation to all the vertices and then write them out to a GS packet which is sent to the GIF" (PATH1).
6. The per-vertex normal → colour computation is **not** on VU1: it happens on **VU0**, using a sine/cosine table at the top of the scratchpad. (`high.cpp` comments)

**Not documented anywhere in Wrench — treat as unknown:**

* The GIF tag(s) the program emits: nloop/eop/nreg/regs, and the PRIM value (primitive type, IIP/TME/FGE/ABE/AA1/FST bits). `gif.h` defines all these bit fields but nothing in the tree ties a value to tfrags. Given the strips are runs consumed as strips, `GS_PRIMITIVE_TRIANGLE_STRIP` is the natural guess. **[inferred]**
* Clipping/culling: whether VU1 does frustum clipping or backface rejection, and how the bounding sphere / cube / mini-spheres feed into it.
* Fog: the GS FOG register and the PRIM FGE bit exist, and XYZF2 is in the register enum, but Wrench says nothing about tfrag fog.
* Whether and how LOD morphing (parent interpolation) is actually performed on VU1, and what the "unknown indices 2" arrays are for.
* How the colour quadwords and the matrix are delivered (separate DMA/unpack per tfrag per frame is the obvious mechanism, but it is not in the tfrag record).
* The role of `positions_slack`, `rgba_verts_loc`, `base_only`, `flags`, `occl_index_stash`, `mip_dist` at runtime.

---

## 8. How Wrench turns a tfrag into an exported triangle mesh

This is the algorithm to cross-check a new loader against. From `high.cpp` (production path; `debug.cpp` holds simpler variants).

**Setup (whole block).**
1. Material count = `max(ad_gif.tex0.data_lo) + 1` over every ad-gif of every tfrag; material `i` is named `i` and textured with `i.png`, which is the `i`-th entry of the level's tfrag `TextureEntry` table.
2. One merged mesh by default, or one mesh per tfrag with the "separate meshes" flag (used when generating occlusion data). Mesh flags: has quads, has texture coordinates, has vertex colours. Submesh 0 is a "lost and found" bucket with material 0.

**Per tfrag.**
3. Build the **position array** = `common_positions ‖ lod_01_positions ‖ lod_0_positions` (this is the index space that vertex-info `vertex/2`, the lights array and the RGBA array all use).
4. Build the **vertex-info array** = `common_vertex_info ‖ lod_01_vertex_info ‖ lod_0_vertex_info` (this is the index space the strip index bytes use).
5. **Propagate tface membership** (only needed to split the mesh into per-tface submeshes; a plain loader can skip it):
   * For each LOD-01 vertex info `i`: parent A = `vertex_info[lod_01_parent_indices[i]].vertex / 2`, parent B = `info.parent / 2`. Same for LOD-0 with its own parent-index array.
   * Recover the **LOD-2** faces (step 7 applied to the LOD-2 strips/indices) and mark every vertex they touch as belonging to that LOD-2 face index — these are the tfaces; the tface count is the LOD-2 face count.
   * For LOD-01 then LOD-0 vertices in order, a child's tface set = the intersection of its two parents' tface sets. A vertex can belong to at most 16 tfaces (Wrench had to raise this limit once for UYA).
6. **Emit one exported vertex per vertex-info entry** (so positions shared by several vertex-info entries are duplicated, and the exported vertex count equals the vertex-info count):
   * `pos_index = info.vertex / 2`
   * `position = (origin.xyz + positions[pos_index].xyz) / 1024`
   * `uv = (info.s / 4096, info.t / 4096)`, **then** the hack: if a component is negative it is multiplied by 0.5. This came from a commit titled "Fix terrain UV stretching/tiling". It is almost certainly not what the hardware does; treat it as a Wrench export heuristic and do **not** copy it into a faithful renderer. **[flagged]**
   * `normal` from `lights[pos_index]`'s azimuth/elevation (§4)
   * `colour` = `rgbas[pos_index]` with `a < 0x80 ? a*2 : 255`
7. **Recover faces from the LOD-0 strips + LOD-0 indices** using the strip walk of §2.6 with the even/odd parity rule: even runs → quads (index order `i+2, i+3, i+1, i+0`), odd runs → triangles (`i, i+1, i+2`). Each face records the currently active ad-gif index.
8. **Assign each face to a submesh.** Quads are mapped to a tface by intersecting the tface sets of all four corners and requiring a unique survivor; the submesh for that tface gets material `ad_gifs[face.ad_gif].tex0.data_lo`. Faces with no unique tface (all triangles, and any ambiguous quad) go to submesh 0.
9. **Fix winding order per face**: average the three corner normals, compare with `cross(v1-v0, v2-v0)`, and swap v0/v2 if the dot product is negative (`core/mesh.cpp`). This is done for the whole scene at the end.

**Cross-check suggestions for a new loader:** compare vertex counts (should equal total vertex-info count), `tri_count` in the header against your LOD-0 triangle total, `vert_count` against total positions, and the exported per-face material against `ad_gifs[strip.z/5].tex0.data_lo`. Note that Wrench's exported mesh is LOD 0 only unless the debug switches are compiled in (`debug.cpp` has `TFRAG_DEBUG_RECOVER_ALL_LODS`, `TFRAG_DEBUG_RAINBOW_STRIPS`, `TFRAG_DEBUG_POLES`, `TFRAG_DEBUG_TFRAGS_AS_SEPARATE_MESHES`, all commented out by default).

---

## 9. Occlusion linkage

(`occl.md`, `low.h`, `asset.cpp`, `chunks.cpp`, `vis.cpp`, `instancemgr/gameplay_impl_misc.inl`)

* Each tfrag header carries `occl_index` (u16 at 0x3a) and the separate `occl_index_stash` (u8 at 0x2b, purpose **[unknown]**).
* The **gameplay file** contains an occlusion-mappings section: a 0x10-byte header with `tfrag_mapping_count`, `tie_mapping_count`, `moby_mapping_count`, pad; followed by `total_count` records of 8 bytes each, each record being `s32 bit_index; s32 occlusion_id`. The tfrag mappings come first, then ties, then mobies.
* The mapping resolves a tfrag's `occl_index` (the `occlusion_id` side) to a **bit index** in the per-octant visibility mask. (`vis.cpp`, `occl.md`)
* The visibility data itself: the playable space is divided into a grid of 4×4×4 **octants**; each octant references a 128-byte bit mask saying which objects are potentially visible (multiple objects may be merged onto one bit, and multiple octants may share a mask); a tree maps a position to a mask. Shrubs are excluded from the system. (`occl.md`)
* When Wrench rebuilds a level it **renumbers** `occl_index` for every tfrag, sequentially from 0, in chunk order (chunk 0's tfrags first, then chunk 1, …) — so in retail data, tfrag occlusion indices are a contiguous 0-based sequence over the level's tfrags in file order, shared across chunks. (`asset.cpp`, `chunks.cpp`)

---

## 9b. Verified on the retail NTSC-U disc (2026-09-26)

Correction from the EE code (docs/plan/render_pipeline.md §6): per-vertex lighting for tfrags is done by `LightTfrags` in VU0 macro mode against a 16-entry light bank at 0x180340, DMA'd through scratchpad; no sine table at 0x70000000 was found, so §4's "sine table" remark is Wrench's guess, not fact.

The retired C++ reference (`src/core/tfrag.cpp`) implemented §1–§2 and its `rc_extract tfrag --level N` validated it: for every tfrag of all 19 levels (20,016 tfrags) the parsed position count equals `vert_count` and the LOD-0 strip walk yields exactly `tri_count` triangles (Novalis: 1004 tfrags, 61,919 triangles, 55,392 vertex-info entries). Unpack addresses match the VU header addresses one-to-one, which confirms the header lane mapping in §2.4. A top-down raster of the LOD-0 mesh shows the expected level layout. Vertex RGB values are visibly in the 0..0x80 range (the raster doubles them), supporting the "0x80 = 1.0" convention flagged in §4.

**Empty-region address aliasing.** An empty VU region gets the same address as the one allocated after it. In 18,117 of 20,016 tfrags there are no LOD-01 "unknown indices 2", so `unk_indices_2_lod_01_addr == parent_indices_lod_0_addr`; a loader that classifies V4_8 unpacks by address alone must skip regions whose header count is zero, or it files the LOD-0 parent indices as LOD-01 "unknown indices 2". The C++ oracle (retired 2026-09-27) had this bug until 2026-09-26 (fixed there together with the Rust port).

**Rust loader.** `crates/rc-formats/src/tfrag.rs` (`parse_tfrags`, `parse_level_tfrags`, `tfrag_triangles`) mirrors the retired C++ reference's `tfrag.cpp` and additionally reads the mini-spheres and the cube. The retired `rc_extract tfrag --level N` wrote `tfrag_dump.bin` (every parsed field, per-LOD triangles and world positions); the golden test `tfrags_for_every_level` required byte-identical results on all 19 levels and now checks the same sections against the committed snapshot hashes.

## 10. Unknowns, in one place

Structural fields nobody in Wrench has explained:

| Where | Field | Status |
| --- | --- | --- |
| Block header 0x08, 0x0c | the f32 and the u32 | unknown; preserved verbatim |
| Tfrag header 0x27 | `base_only` | unknown (name suggests "lowest LOD only") |
| Tfrag header 0x2a | `rgba_verts_loc` | unknown (name suggests a VU location for vertex colours) |
| Tfrag header 0x2b | `occl_index_stash` | unknown |
| Tfrag header 0x2d | `flags` | unknown — no bit is documented, including anything about back-faces or double-sidedness |
| Tfrag header 0x24–0x26 | per-LOD rgba counts | documented as "counts"; exact unit and relation to vertex counts unverified |
| Tfrag header 0x34–0x36 | `dir_lights_one`, `dir_lights_upd`, `point_lights` | light references; RAC1 seems to use the "none" values 0xff/0/0xffff |
| Tfrag header 0x3e | `mip_dist` | LOD/mip distance; formula unknown |
| Mini-spheres | `Vec4f[msphere_count]` | purpose unknown (per-tface culling spheres?) |
| Cube | 8 × 4×s16 at `cube_ofs` | purpose unknown |
| Light record | `unknown_0`, `intensity`, `color` | unknown; only azimuth/elevation are used |
| VU header | lanes 0.y, 0.w, 1.y, 2.x, 2.z, 3.x | **resolved:** extra vertex-info counts and addresses per tier (§2.4) |
| VU memory | `positions_slack` between positions and vertex info | unknown, but must be preserved to reproduce retail addresses |
| Command lists | the "unknown indices 2" arrays (LOD-01 and LOD-0) | one byte per *extra* vertex-info entry of the tier (§2.4); VU1 uses it as a collapse replacement index (vu1_tfrag_analysis.md §5) |
| Position unpack | what ends up in the w lane of a V3_16 position | unknown |

Behavioural unknowns: the VU1 microprogram's actual instruction stream; the GIF tag / PRIM value it emits; clipping, culling and fog handling; how LOD is selected and whether/how parent interpolation morphs vertices; how the matrix and the per-vertex colour quadwords are delivered each frame; whether vertex RGB is 0…0x80 or 0…0xff; and whether the negative-UV halving in Wrench's exporter corresponds to anything real (it very likely does not).

---

## Appendix: quick provenance index

* Block + record layouts, field names/offsets, VU record layouts: `src/engine/tfrag_low.h`.
* Sub-block slicing, VIF command order, STROW contents, VU1 allocation, triangle counting, RAC-vs-DL header union: `src/engine/tfrag_low.cpp`.
* Position scale 1/1024, UV fixed-12, spherical normals + "VU0 / scratchpad sin-cos table", alpha ×2, strip→quad/triangle recovery, tface propagation, texture index → material: `src/engine/tfrag_high.cpp`.
* Per-LOD array membership, plain tristrip walk, texture-switch condition on the strip's `y` byte, LOD-parent visualisation: `src/engine/tfrag_debug.cpp`.
* LOD/tface prose, the per-LOD VIF command tables, the VU1 data-memory map, the strip pseudocode and "first strip always encodes an AD GIF": `docs/tfrag_renderer.md`.
* VIF code/unpack decoding, element and packet sizes, NUM==0 means 256, fixed-12 macros, STROW record: `src/core/vif.h`, `src/core/vif.cpp` (and `src/vifcli.cpp` is a standalone VIF list disassembler useful for eyeballing a real tfrag).
* GIF A+D quadword layout, GS register addresses, PRIM/GIFtag bit fields: `src/engine/gif.h`.
* Location and size derivation of the tfrags block, tfrag TextureEntry table: `src/wrenchbuild/level/level_core.{h,cpp}`.
* TextureEntry layout, palette swizzle, alpha convention, RAC-vs-DL pixel swizzle: `src/wrenchbuild/level/level_textures.{h,cpp}`, `docs/textures.md`.
* Round-trip test, occlusion renumbering, export entry point: `src/wrenchbuild/level/tfrags_asset.cpp`.
* Chunk handling and occlusion index sequencing: `src/wrenchbuild/level/level_chunks.cpp`.
* Occlusion mapping record and system overview: `src/wrenchvis/wrenchvis.cpp`, `src/instancemgr/gameplay_impl_misc.inl`, `docs/occlusion_culling.md`.
* PS2 data-flow context and renderer provenance: `docs/renderers.md`.
* Z-up convention: `src/editor/renderer.h`, `src/core/collada.cpp`.
* Winding-order repair: `src/core/mesh.cpp`.
