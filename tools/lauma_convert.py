#!/usr/bin/env python3
"""Converts the Lauma glTF binary (.glb) into `lauma.rcm`, the hero-model asset `rc-engine/src/hero_model.rs` loads.

    python3 tools/lauma_convert.py crates/rc-engine/assets/models/lauma.glb crates/rc-engine/assets/models/lauma.rcm
    python3 tools/lauma_convert.py in.glb out.rcm --tex-max 512      # smaller textures (default 1024)

Needs numpy and Pillow. What it does (everything the Rust side would otherwise need a glTF / PNG decoder for):

* Reads the rest-pose geometry (positions, normals, UV set 0, triangles) of every primitive. The skin's bind pose is
  the identity here (checked below), so the stored vertices ARE her rest pose; the 60 face morph targets and UV sets
  1 / 2 are ignored.
* With `--dump lauma_dump.txt` (written by `RC_LAUMA_DUMP=1`, Ratchet's skeleton) it also retargets her onto his 111
  joints (tools/lauma_rig.py): her mesh is posed from the A pose into his T pose, every vertex is skinned to up to three
  of his joints through her own bone weights, and the positions his joints should have in her proportions are written
  beside the mesh. The engine then plays his animations on her. Without the dump the file is version 1 (no skin: she
  rides one joint and does not deform).
* Converts glTF axes (x = her left, y up, z = her front) to the game's (x forward, y left, z up): (gx, gy, gz) =
  (z, x, y), a proper rotation, so nothing is mirrored and the triangle winding is kept.
* Builds the textures the engine's moby material takes: RGBA8 with the alpha in the GS form `texture::scale_alpha`
  produces (the engine undoes it: moby_render::gs_image). Materials the glTF marks OPAQUE get alpha 255 (the hair
  sheet has stray alpha the skirt alone uses); the skirt (BLEND) keeps its alpha; the skin overlay (material 14,
  the same vertices and UVs as material 13 with a mostly transparent texture) is composited into material 13's
  texture and its duplicate mesh dropped, so no coplanar layers fight for depth.

File layout (little-endian): "RCHM", u32 version (1, 2 with the rig block, 3 with the jiggle strands, 4 with the body contact points), u32 vertices, u32 triangles, u32 textures, u8[16] material ->
texture (0xff: none; a dropped material), f32 positions[3 * V], f32 normals[3 * V], f32 uvs[2 * V],
u32 triangles[4 * T] (a, b, c, material), then per texture u32 width, u32 height, width * height * 4 bytes RGBA.
Version 2 appends, after the textures (version 3 adds the jiggle strands after that: u32 count, then per strand u8 joint, u8 anchor,
u16 group (0 cloth and hair, 1 bust), f32 spring Hz, damping ratio, largest swing in degrees, f32 pivot[3], f32 tip[3]; game axes, the model's own frame;
version 4 adds the body contact points after that: u32 count, then per point u8 joint, 3 pad bytes, f32 position[3]): u32 joint count (111), then per vertex u8 joint[3], u8 influence count, u16
weight[3] (sum 256), then f32 joint position[3] per joint (game axes, the model's own frame, before the engine's fit).
"""
import argparse
import io
import os
import json
import struct
import sys

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

CT = {5120: np.int8, 5121: np.uint8, 5122: np.int16, 5123: np.uint16, 5125: np.uint32, 5126: np.float32}
NC = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT4": 16}

# Material that is an overlay of another (same vertices and UVs, alpha texture): overlay -> base.
OVERLAYS = {14: 13}
BASES = {base: overlay for overlay, base in OVERLAYS.items()}
# Materials in the order their textures should get table indices (the engine draws a class's textures in index
# order): face, baked skin, body sheet, hair, skirt. Any others follow.
DRAW_ORDER = [0, 13, 12, 8, 15]


class Glb:
    def __init__(self, path):
        data = open(path, "rb").read()
        magic, version, _ = struct.unpack("<III", data[:12])
        if magic != 0x46546C67 or version != 2:
            raise SystemExit(f"{path}: not a glTF 2.0 binary")
        off, self.bin = 12, b""
        while off < len(data):
            length, kind = struct.unpack("<II", data[off:off + 8])
            body = data[off + 8:off + 8 + length]
            if kind == 0x4E4F534A:
                self.j = json.loads(body)
            elif kind == 0x004E4942:
                self.bin = body
            off += 8 + length

    def accessor(self, index):
        a = self.j["accessors"][index]
        if "sparse" in a:
            raise SystemExit("sparse base accessors are not supported")
        v = self.j["bufferViews"][a["bufferView"]]
        dt, n = np.dtype(CT[a["componentType"]]), NC[a["type"]]
        off = v.get("byteOffset", 0) + a.get("byteOffset", 0)
        stride = v.get("byteStride") or dt.itemsize * n
        if stride == dt.itemsize * n:
            out = np.frombuffer(self.bin, dtype=dt, count=a["count"] * n, offset=off).reshape(a["count"], n)
        else:
            out = np.empty((a["count"], n), dtype=dt)
            for i in range(a["count"]):
                out[i] = np.frombuffer(self.bin, dtype=dt, count=n, offset=off + i * stride)
        if a.get("normalized"):
            out = out.astype(np.float32) / np.iinfo(dt).max
        return out

    def skeleton(self):
        """Her skin's joints: names, parent (index into the joint list, -1 root), rest world position (glTF axes)."""
        nodes = self.j["nodes"]
        parent = {c: i for i, n in enumerate(nodes) for c in n.get("children", [])}
        joints = self.j["skins"][0]["joints"]
        index = {n: k for k, n in enumerate(joints)}
        names = [nodes[n].get("name", "?") for n in joints]
        par = [index.get(parent.get(n), -1) for n in joints]
        pos = np.array([node_world(nodes, parent, n)[:3, 3] for n in joints])
        return names, par, pos

    def image(self, index):
        v = self.j["bufferViews"][self.j["images"][index]["bufferView"]]
        off = v.get("byteOffset", 0)
        return Image.open(io.BytesIO(self.bin[off:off + v["byteLength"]]))


def node_world(nodes, parent, i):
    def trs(n):
        if "matrix" in n:
            return np.array(n["matrix"]).reshape(4, 4).T
        x, y, z, w = n.get("rotation", [0, 0, 0, 1])
        r = np.array([[1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w)],
                      [2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w)],
                      [2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y)]])
        m = np.eye(4)
        m[:3, :3] = r * np.array(n.get("scale", [1, 1, 1]))
        m[:3, 3] = n.get("translation", [0, 0, 0])
        return m
    m = trs(nodes[i])
    while i in parent:
        i = parent[i]
        m = trs(nodes[i]) @ m
    return m


def check_rest_pose(g):
    """The vertices are used as stored, which is only right if skinning is the identity at rest."""
    j, nodes = g.j, g.j["nodes"]
    parent = {c: i for i, n in enumerate(nodes) for c in n.get("children", [])}
    sk = j["skins"][0]
    ibm = g.accessor(sk["inverseBindMatrices"]).reshape(-1, 4, 4).transpose(0, 2, 1)
    dev = max(np.abs(node_world(nodes, parent, jn) @ ibm[k] - np.eye(4)).max() for k, jn in enumerate(sk["joints"]))
    mesh_node = next(i for i, n in enumerate(nodes) if "mesh" in n)
    mesh_dev = np.abs(node_world(nodes, parent, mesh_node) - np.eye(4)).max()
    print(f"rest pose: max |joint world * inverse bind - I| = {dev:.2e}, mesh node deviation {mesh_dev:.2e}")
    if dev > 1e-3 or mesh_dev > 1e-3:
        raise SystemExit("the bind pose is not the identity: apply the skin before converting")


def fit(img, tex_max):
    """Downscale with premultiplied alpha (no dark fringes), keeping the aspect."""
    if max(img.size) <= tex_max:
        return img
    k = tex_max / max(img.size)
    size = (max(1, round(img.width * k)), max(1, round(img.height * k)))
    return img.convert("RGBa").resize(size, Image.LANCZOS).convert("RGBA")


def gs_alpha(a8):
    """8-bit alpha -> the stored form: GS alpha 0..0x80 (0x80 = opaque), then texture::scale_alpha."""
    gs = np.rint(a8.astype(np.float64) * 128.0 / 255.0).astype(np.int32)
    return np.where(gs >= 128, 255, gs * 2).astype(np.uint8)


def encode(img, keep_alpha):
    arr = np.asarray(img.convert("RGBA")).copy()
    arr[..., 3] = gs_alpha(arr[..., 3]) if keep_alpha else 255
    return arr


def main():
    ap = argparse.ArgumentParser(description="glTF binary -> lauma.rcm (see the module doc)")
    ap.add_argument("glb")
    ap.add_argument("rcm")
    ap.add_argument("--tex-max", type=int, default=1024, help="longest texture side (default 1024)")
    ap.add_argument("--dump", default="lauma_dump.txt", help="Ratchet's skeleton dump (RC_LAUMA_DUMP=1); makes a rigged "
                    "version 2 file (default lauma_dump.txt when it exists)")
    ap.add_argument("--no-rig", action="store_true", help="write the version 1 file (no skin, no skeleton)")
    args = ap.parse_args()
    g = Glb(args.glb)
    j = g.j
    check_rest_pose(g)
    prims = j["meshes"][0]["primitives"]
    n_mat = len(j["materials"])
    if n_mat > 16:
        raise SystemExit(f"{n_mat} materials: a class has 16 texture slots")

    def source(m):
        return j["textures"][j["materials"][m]["pbrMetallicRoughness"]["baseColorTexture"]["index"]]["source"]

    def blended(m):
        return j["materials"][m].get("alphaMode", "OPAQUE") == "BLEND"

    def key(m):
        if m in BASES:
            return ("baked", source(m), source(BASES[m]))
        return ("alpha" if blended(m) else "opaque", source(m))

    sheets = {}

    def sheet(i):
        if i not in sheets:
            sheets[i] = g.image(i).convert("RGBA")
        return sheets[i]

    kept = [m for m in range(n_mat) if m not in OVERLAYS]
    seq = [m for m in DRAW_ORDER if m in kept] + [m for m in kept if m not in DRAW_ORDER]
    mat_tex = [0xFF] * 16
    tex_keys = []
    for m in seq:
        k = key(m)
        if k not in tex_keys:
            tex_keys.append(k)
        mat_tex[m] = tex_keys.index(k)

    textures = []
    for k in tex_keys:
        if k[0] == "baked":
            base = sheet(k[1]).copy()
            base.putalpha(255)
            textures.append(encode(fit(Image.alpha_composite(base, sheet(k[2])), args.tex_max), False))
        else:
            textures.append(encode(fit(sheet(k[1]), args.tex_max), k[0] == "alpha"))

    # Geometry: glTF (x her left, y up, z her front) -> game (x forward, y left, z up).
    raw = []
    for p in prims:
        at = p["attributes"]
        raw.append(dict(m=p["material"], P=g.accessor(at["POSITION"]).astype(np.float32), N=g.accessor(at["NORMAL"]).astype(np.float32),
                        U=g.accessor(at["TEXCOORD_0"]).astype(np.float32), I=g.accessor(p["indices"]).astype(np.uint32).reshape(-1, 3),
                        J=g.accessor(at["JOINTS_0"]).astype(np.int64), W=g.accessor(at["WEIGHTS_0"]).astype(np.float64)))
    rig = None
    if not args.no_rig and os.path.exists(args.dump):
        import lauma_rig
        allp = lambda k, dt: np.concatenate([r[k] for r in raw]).astype(dt)
        names, par, bone_pos = g.skeleton()
        rig = lauma_rig.retarget(names, par, bone_pos, allp("P", np.float64), allp("N", np.float64), allp("J", np.int64), allp("W", np.float64), args.dump)
        print(rig["report"])
        at = 0
        for r in raw:  # the posed vertices back into the primitives
            n = len(r["P"])
            r["P"], r["N"] = rig["pos"][at:at + n].astype(np.float32), rig["nrm"][at:at + n].astype(np.float32)
            r["sj"], r["sw"], r["sc"] = (a[at:at + n] for a in rig["skin"])
            at += n
    elif not args.no_rig:
        print(f"{args.dump}: not found; writing the version 1 model (no rig). Run the game once with RC_LAUMA_DUMP=1 to make it.")
    pos, nrm, uv, tris, skj, skw, skc = [], [], [], [], [], [], []
    base = 0
    for r in raw:
        m = r["m"]
        if m in OVERLAYS:
            continue
        P, N, U, I = r["P"], r["N"], r["U"], r["I"]
        pos.append(P[:, [2, 0, 1]])
        n = N[:, [2, 0, 1]]
        nrm.append(n / np.maximum(np.linalg.norm(n, axis=1, keepdims=True), 1e-8))
        uv.append(U)
        tris.append(np.column_stack([I + base, np.full(len(I), m, dtype=np.uint32)]))
        if rig:
            skj.append(r["sj"]), skw.append(r["sw"]), skc.append(r["sc"])
        base += len(P)
    pos, nrm, uv, tris = (np.concatenate(a).astype(t) for a, t in [(pos, np.float32), (nrm, np.float32), (uv, np.float32), (tris, np.uint32)])

    # Sanity: her face parts must be in front (+x) of her back hair, or the facing assumption is wrong.
    face = pos[tris[tris[:, 3] == 6][:, 0]]
    back = pos[tris[tris[:, 3] == 9][:, 0]]
    print(f"facing: eyes at x = {face[:, 0].mean():+.3f}, back hair at x = {back[:, 0].mean():+.3f} (the eyes must be larger)")
    print(f"extent (game axes, m): x {pos[:, 0].min():+.3f}..{pos[:, 0].max():+.3f}, y {pos[:, 1].min():+.3f}..{pos[:, 1].max():+.3f}, "
          f"z {pos[:, 2].min():+.3f}..{pos[:, 2].max():+.3f}")
    print(f"uv range {uv.min():.4f}..{uv.max():.4f}")

    with open(args.rcm, "wb") as f:
        f.write(b"RCHM")
        f.write(struct.pack("<IIII", (4 if rig["contacts"] else 3 if rig["jiggle"] else 2) if rig else 1, len(pos), len(tris), len(textures)))
        f.write(bytes(mat_tex))
        for a in (pos, nrm, uv):
            f.write(np.ascontiguousarray(a, dtype="<f4").tobytes())
        f.write(np.ascontiguousarray(tris, dtype="<u4").tobytes())
        for t in textures:
            f.write(struct.pack("<II", t.shape[1], t.shape[0]))
            f.write(np.ascontiguousarray(t).tobytes())
        if rig:
            sj, sw, sc = np.concatenate(skj), np.concatenate(skw), np.concatenate(skc)
            f.write(struct.pack("<I", len(rig["joints_game"])))
            rec = np.zeros(len(pos), dtype=[("j", "u1", 3), ("c", "u1"), ("w", "<u2", 3)])
            rec["j"], rec["c"], rec["w"] = sj, sc, sw
            assert rec.itemsize == 10 and (rec["w"].sum(axis=1) == 256).all()
            f.write(rec.tobytes())
            f.write(np.ascontiguousarray(rig["joints_game"], dtype="<f4").tobytes())
            if rig["jiggle"] or rig["contacts"]:
                f.write(struct.pack("<I", len(rig["jiggle"])))
                for st in rig["jiggle"]:
                    f.write(struct.pack("<BBHfff", st["joint"], st["anchor"], st["group"], st["freq"], st["damp"], st["max_deg"]))
                    f.write(np.asarray(st["pivot"], dtype="<f4").tobytes() + np.asarray(st["tip"], dtype="<f4").tobytes())
            if rig["contacts"]:
                f.write(struct.pack("<I", len(rig["contacts"])))
                for j, x, y, z in rig["contacts"]:
                    f.write(struct.pack("<BBBBfff", j, 0, 0, 0, x, y, z))
    print(f"{args.rcm}: {len(pos)} vertices, {len(tris)} triangles, {len(textures)} textures "
          f"{[t.shape[1::-1] for t in textures]}, materials -> textures {mat_tex}")


if __name__ == "__main__":
    main()
