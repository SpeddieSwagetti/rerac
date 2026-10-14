#!/usr/bin/env python3
"""Rig retargeting for the Lauma hero model: her bones -> Ratchet's 111 joints.  Used by `lauma_convert.py --dump`.

Ratchet's animations are per-joint rotations (plus a few translation / scale keys) in the frame of his bind pose: a T pose
with the arms along +-y and every joint's local axes aligned to the world axes (docs/plan/moby_animation.md).  To play
them on Lauma three things have to line up, and this module produces all three:

1. **Pose.**  Her glTF rest pose is an A pose (arms 40 degrees below horizontal).  Her mesh is first posed into a T pose
   with her own skeleton and skin weights (linear blend skinning, the arms rotated about the shoulders), so a rotation of
   one of his joints means the same thing on her as on him.
2. **Skin.**  Every vertex gets up to three of Ratchet's joints.  Her bones map to his by name (MMD names; see `MAP`).
   Bones that sit between two of his joints (twist bones, the spine) are split between the two neighbours by where the
   bone lies on the chain; hair, ears and ornaments ride the head; the cape rides the spine; the skirt rides the pelvis
   and blends into the thighs toward the hem.
3. **Proportions.**  Her legs are longer and her torso shorter than his.  Rather than squash her to his skeleton, the
   skeleton is reshaped to her: `skeleton_positions` returns where each of his 111 joints sits in her T pose.  The engine
   (`hero_model.rs`) rewrites the joints' rest translations and inverse-bind rows to those positions and rescales his
   translation keys, so his rotations move her limbs about her own joints.  No limb is stretched, nothing tears.

Everything here is in glTF axes (x her left, y up, z her front) until `to_game` at the very end.
"""
import re

import numpy as np

# ----------------------------------------------------------------------------------------------------------------------
# Ratchet's skeleton, from `lauma_dump.txt` (written by RC_LAUMA_DUMP=1)

N_JOINTS = 111


def parse_dump(path):
    """-> (parent[111], rest_world[111, 3] in game axes, metres)."""
    parent = [-1] * N_JOINTS
    rest = np.zeros((N_JOINTS, 3))
    row = re.compile(r"^\s*(\d+)\s+(-|\d+)\s*\|\s*([-+.\d]+)\s+([-+.\d]+)\s+([-+.\d]+)\s*\|")
    seen = 0
    for line in open(path, encoding="utf-8"):
        m = row.match(line)
        if m:
            j = int(m.group(1))
            parent[j] = -1 if m.group(2) == "-" else int(m.group(2))
            rest[j] = [float(m.group(3)), float(m.group(4)), float(m.group(5))]
            seen += 1
    if seen != N_JOINTS:
        raise SystemExit(f"{path}: expected {N_JOINTS} joint rows, found {seen}")
    return parent, rest


# ----------------------------------------------------------------------------------------------------------------------
# Her skeleton

SIDES = {"L": +1, "R": -1}


class Rig:
    """Her bones by (MMD) name, positions in glTF axes, rest pose."""

    def __init__(self, names, parent, pos):
        self.names, self.parent, self.pos = names, parent, pos
        self.index = {n: i for i, n in enumerate(names)}
        self.children = {}
        for i, p in enumerate(parent):
            self.children.setdefault(p, []).append(i)

    def p(self, name):
        return self.pos[self.index[name]]

    def subtree(self, i):
        out, stack = [], [i]
        while stack:
            k = stack.pop()
            out.append(k)
            stack.extend(self.children.get(k, []))
        return out


def rodrigues(axis, angle):
    a = np.asarray(axis, float)
    a = a / np.linalg.norm(a)
    k = np.array([[0, -a[2], a[1]], [a[2], 0, -a[0]], [-a[1], a[0], 0]])
    return np.eye(3) + np.sin(angle) * k + (1 - np.cos(angle)) * (k @ k)


def pose_to_t(rig, pos, nrm, joints, weights):
    """A pose -> T pose.  Each arm (the 腕 bone and everything below it) is rotated about its shoulder about the z axis until
    shoulder -> wrist is horizontal.  Returns the posed positions and normals, the per-bone rigid transforms (R, t) and
    the posed bone positions."""
    nb = len(rig.names)
    R = np.tile(np.eye(3), (nb, 1, 1))
    T = np.zeros((nb, 3))
    angles = {}
    for s, sign in SIDES.items():
        a, w = rig.p(f"腕.{s}"), rig.p(f"手首.{s}")
        d = w - a
        # angle of the arm below horizontal, in the x-y plane (her z is her front, so the rotation axis is z)
        below = np.arctan2(-d[1], sign * d[0])
        rot = rodrigues([0, 0, 1], sign * below)  # raises the arm for +x (her left), mirrored for -x
        angles[s] = np.degrees(below)
        for k in rig.subtree(rig.index[f"腕.{s}"]):
            R[k] = rot
            T[k] = a - rot @ a
    # linear blend skinning with her own weights
    out_p = np.zeros_like(pos)
    out_n = np.zeros_like(nrm)
    for k in range(4):
        j = joints[:, k]
        w = weights[:, k:k + 1]
        out_p += w * (np.einsum("vab,vb->va", R[j], pos) + T[j])
        out_n += w * np.einsum("vab,vb->va", R[j], nrm)
    out_n /= np.maximum(np.linalg.norm(out_n, axis=1, keepdims=True), 1e-8)
    posed_bones = np.einsum("kab,kb->ka", R, rig.pos) + T
    return out_p, out_n, posed_bones, angles


# ----------------------------------------------------------------------------------------------------------------------
# The mapping.  Ratchet joint numbers (see lauma_dump.txt): 0 root, 1 pelvis, 2..7 spine to neck, 8 head,
# 52 / 72 right / left clavicle, 53 / 73 shoulder, 54 / 74 elbow, 55 / 75 forearm, 56 / 76 wrist, 57.. fingers,
# 92 / 98 hip, 93 / 99 knee, 94 / 100 ankle, 95 / 101 foot, 96 / 102 ball, 97 / 103 toes.  Right = y < 0 = her .R bones.

ARM = {"R": dict(clav=52, sh=53, el=54, fore=55, wr=56, elb=71, idx=(57, 58, 59), mid=(60, 61, 62), rng=(63, 64, 65),
                 pnk=(66, 67, 68), thm=(69, 70)),
       "L": dict(clav=72, sh=73, el=74, fore=75, wr=76, elb=91, idx=(77, 78, 79), mid=(80, 81, 82), rng=(83, 84, 85),
                 pnk=(86, 87, 88), thm=(89, 90))}
LEG = {"R": dict(hip=92, knee=93, ankle=94, foot=95, ball=96, toe=97),
       "L": dict(hip=98, knee=99, ankle=100, foot=101, ball=102, toe=103)}
SPINE = [1, 2, 3, 4, 5, 6, 7]
HEAD = 8
NECK = 7
PELVIS = 1

# Fractions along the pelvis -> neck path of Ratchet's spine joints (from his rest heights 0.490 ... 0.952).
SPINE_T = [(z - 0.490) / (0.952 - 0.490) for z in (0.490, 0.520, 0.590, 0.689, 0.795, 0.896, 0.952)]


class Chain:
    """A polyline of Ratchet joints at their target positions (in her glTF T pose); `hat` gives the weights over its two
    neighbouring joints for a point, from where the point projects onto the line."""

    def __init__(self, joints, points):
        self.joints = list(joints)
        self.pts = np.asarray(points, float)

    def hat(self, q):
        best = (1e18, 0, 0.0)
        for i in range(len(self.pts) - 1):
            a, b = self.pts[i], self.pts[i + 1]
            ab = b - a
            f = float(np.clip(np.dot(q - a, ab) / max(np.dot(ab, ab), 1e-12), 0.0, 1.0))
            d = np.linalg.norm(q - (a + f * ab))
            if d < best[0] - 1e-9:
                best = (d, i, f)
        _, i, f = best
        out = {}
        if f < 1.0:
            out[self.joints[i]] = 1.0 - f
        if f > 0.0:
            out[self.joints[i + 1]] = out.get(self.joints[i + 1], 0.0) + f
        return out


def polyline_at(points, fractions):
    """Points at the given arc-length fractions along a polyline."""
    pts = np.asarray(points, float)
    seg = np.linalg.norm(np.diff(pts, axis=0), axis=1)
    cum = np.concatenate([[0], np.cumsum(seg)])
    out = []
    for t in fractions:
        d = t * cum[-1]
        i = int(np.clip(np.searchsorted(cum, d, side="right") - 1, 0, len(seg) - 1))
        f = (d - cum[i]) / max(seg[i], 1e-12)
        out.append(pts[i] + f * (pts[i + 1] - pts[i]))
    return np.array(out)


def joint_targets(rig, bones, foot_front):
    """Where each of the joints Lauma's bones can drive sits in her T pose (glTF axes).  `bones` = posed bone positions;
    `foot_front` = (z of the toe tip) per side."""
    P = lambda n: bones[rig.index[n]]
    tg = {}
    hip_mid = 0.5 * (P("足D.L") + P("足D.R"))
    pelvis = np.array([hip_mid[0], hip_mid[1] + 0.03, hip_mid[2]])
    path = [pelvis, P("腰"), P("上半身"), P("上半身2"), P("首")]
    for j, pt in zip(SPINE, polyline_at(path, SPINE_T)):
        tg[j] = pt
    tg[PELVIS] = pelvis
    tg[NECK] = P("首")
    tg[HEAD] = P("頭")
    for s in "LR":
        a = ARM[s]
        tg[a["clav"]] = P(f"肩.{s}")
        tg[a["sh"]] = P(f"腕.{s}")
        tg[a["el"]] = P(f"ひじ.{s}")
        tg[a["wr"]] = P(f"手首.{s}")
        tg[a["fore"]] = tg[a["el"]] + 0.26 * (tg[a["wr"]] - tg[a["el"]])
        tg[a["elb"]] = tg[a["el"]] + 0.10 * (tg[a["wr"]] - tg[a["el"]])
        for key, base in (("idx", "人指"), ("mid", "中指"), ("rng", "薬指"), ("pnk", "小指")):
            for k, j in enumerate(a[key]):
                tg[j] = P(f"{base}{'１２３'[k]}.{s}")
        tg[a["thm"][0]] = P(f"親指０.{s}")
        tg[a["thm"][1]] = P(f"親指１.{s}")
        l = LEG[s]
        tg[l["hip"]] = P(f"足D.{s}")
        tg[l["knee"]] = P(f"ひざD.{s}")
        tg[l["ankle"]] = P(f"足首D.{s}")
        ball = P(f"足先EX.{s}")
        tg[l["ball"]] = ball
        tg[l["foot"]] = tg[l["ankle"]] + 0.45 * (ball - tg[l["ankle"]])
        tg[l["toe"]] = np.array([ball[0], ball[1] * 0.8, foot_front[s] - 0.02])
    return tg


def build_chains(tg):
    c = {"spine": Chain(SPINE, [tg[j] for j in SPINE])}
    for s in "LR":
        a = ARM[s]
        c[f"upper{s}"] = Chain([a["sh"], a["el"]], [tg[a["sh"]], tg[a["el"]]])
        c[f"lower{s}"] = Chain([a["el"], a["fore"], a["wr"]], [tg[a["el"]], tg[a["fore"]], tg[a["wr"]]])
    return c


# ----------------------------------------------------------------------------------------------------------------------
# Bone -> Ratchet joints

SKIRT = re.compile(r"^(S_\d|前裙|后摆|Bone_Skirt|SkirtSide)")
CAPE = re.compile(r"^F_\d")
TWIST_UP = re.compile(r"^(腕捩\d?|腕饰)\.(L|R)$")
TWIST_LO = re.compile(r"^(手捩\d?)\.(L|R)$")


def bone_maps(rig, chains):
    """-> (fixed: dict bone index -> {joint: weight} | None, kind[bone]): kind is 'skirt', 'cape' or 'fixed'."""
    nb = len(rig.names)
    fixed = [None] * nb
    kind = ["fixed"] * nb
    P = lambda i: rig.pos[i]
    I = rig.index

    def put(name, jmap):
        fixed[I[name]] = jmap

    for n in ("全ての親", "センター", "グルーブ", "腰", "下半身"):
        put(n, {PELVIS: 1.0})
    for n in ("上半身", "上半身2", "胸上2.L", "胸上2.R"):
        put(n, chains["spine"].hat(P(I[n])))
    put("首", {NECK: 1.0})
    put("頭", {HEAD: 1.0})
    for s in "LR":
        a, l = ARM[s], LEG[s]
        put(f"肩P.{s}", {a["clav"]: 1.0})
        put(f"肩.{s}", {a["clav"]: 1.0})
        put(f"肩C.{s}", {a["sh"]: 1.0})
        put(f"腕.{s}", {a["sh"]: 1.0})
        put(f"ひじ.{s}", {a["el"]: 1.0})
        put(f"ひじ補助.{s}", {a["el"]: 1.0})
        put(f"手首.{s}", {a["wr"]: 1.0})
        for i, n in enumerate(rig.names):
            if n.endswith("." + s) and TWIST_UP.match(n):
                fixed[i] = chains[f"upper{s}"].hat(P(i))
            elif n.endswith("." + s) and TWIST_LO.match(n):
                fixed[i] = chains[f"lower{s}"].hat(P(i))
        for key, base in (("idx", "人指"), ("mid", "中指"), ("rng", "薬指"), ("pnk", "小指")):
            for k, j in enumerate(a[key]):
                put(f"{base}{'１２３'[k]}.{s}", {j: 1.0})
        put(f"親指０.{s}", {a["thm"][0]: 1.0})
        put(f"親指１.{s}", {a["thm"][1]: 1.0})
        put(f"親指２.{s}", {a["thm"][1]: 1.0})
        put(f"足D.{s}", {l["hip"]: 1.0})
        put(f"腰キャンセル.{s}", {l["hip"]: 1.0})
        put(f"尻.{s}", {PELVIS: 1.0})
        put(f"ひざD.{s}", {l["knee"]: 1.0})
        put(f"ひざ補助.{s}", {l["knee"]: 1.0})
        put(f"足首D.{s}", {l["ankle"]: 1.0})
        put(f"足先EX.{s}", {l["ball"]: 1.0})
    # the rest inherit from the nearest mapped ancestor (hair, ears, ornaments -> head; sleeve cloth -> forearm)
    for i, n in enumerate(rig.names):
        if SKIRT.match(n):
            kind[i] = "skirt"
        elif CAPE.match(n):
            kind[i] = "cape"
    for i in range(nb):
        if fixed[i] is None and kind[i] == "fixed":
            k = rig.parent[i]
            while k >= 0 and fixed[k] is None:
                k = rig.parent[k]
            fixed[i] = fixed[k] if k >= 0 else {PELVIS: 1.0}
    return fixed, kind


LEG_BLEND = 0.55  # share of the hem that follows the thighs


def vertex_skin(rig, fixed, kind, chains, pos_a, joints, weights):
    """Per-vertex skin on Ratchet's joints, up to three joints, weights summing to 256.
    `pos_a`: the vertices in her A pose (bone positions are the A-pose ones, so the projections match)."""
    n = len(pos_a)
    out_j = np.zeros((n, 3), np.uint8)
    out_w = np.zeros((n, 3), np.uint16)
    out_c = np.zeros(n, np.uint8)
    spine = chains["spine"]
    hip_y = float(np.mean([rig.p("足D.L")[1], rig.p("足D.R")[1]]))
    legs = {"L": LEG["L"]["hip"], "R": LEG["R"]["hip"]}
    for v in range(n):
        acc = {}
        for k in range(4):
            w = float(weights[v, k])
            if w <= 0.0:
                continue
            b = int(joints[v, k])
            kd = kind[b]
            if kd == "skirt":
                h = float(np.clip((hip_y + 0.02 - pos_a[v, 1]) / 0.30, 0.0, 1.0))
                left = float(np.clip(0.5 + pos_a[v, 0] / 0.20, 0.0, 1.0))
                parts = {PELVIS: 1.0 - LEG_BLEND * h, legs["L"]: LEG_BLEND * h * left, legs["R"]: LEG_BLEND * h * (1.0 - left)}
            elif kd == "cape":
                parts = spine.hat(pos_a[v])
            else:
                parts = fixed[b]
            for j, pw in parts.items():
                if pw > 0:
                    acc[j] = acc.get(j, 0.0) + w * pw
        top = sorted(acc.items(), key=lambda t: -t[1])[:3]
        total = sum(w for _, w in top) or 1.0
        q = [w / total * 256.0 for _, w in top]
        qi = [int(x) for x in q]
        for i in sorted(range(len(q)), key=lambda i: -(q[i] - qi[i]))[: 256 - sum(qi)]:
            qi[i] += 1
        keep = [(j, w) for (j, _), w in zip(top, qi) if w > 0]
        out_c[v] = len(keep)
        for i, (j, w) in enumerate(keep):
            out_j[v, i], out_w[v, i] = j, w
    return out_j, out_w, out_c


# ----------------------------------------------------------------------------------------------------------------------

def to_game(a):
    """glTF (x her left, y up, z her front) -> game (x forward, y left, z up)."""
    return a[..., [2, 0, 1]]


def skeleton_positions(tg, parent, ratchet_rest, scale, height_shift):
    """The 111 joint positions in her T pose, game axes, in the model's own frame (the engine applies the same fit as to
    the mesh).  Joints she has no counterpart for (Ratchet's face and ear joints: nothing is skinned to them) hang off
    their parent at his offsets shrunk by `k`, so they stay inside her head."""
    pos = np.zeros((N_JOINTS, 3))
    have = {j: to_game(p) for j, p in tg.items()}
    k_unmapped = 0.55 / scale  # in model units: his offsets (metres) times 0.55 of a metre, divided by the fit scale
    order = []
    seen = set()

    def visit(j):
        if j in seen:
            return
        if parent[j] >= 0:
            visit(parent[j])
        seen.add(j)
        order.append(j)

    for j in range(N_JOINTS):
        visit(j)
    for j in order:
        if j == 0:
            pos[0] = [0.0, 0.0, height_shift]  # root: on the floor under her
        elif j in have:
            pos[j] = have[j]
        else:
            pos[j] = pos[parent[j]] + (ratchet_rest[j] - ratchet_rest[parent[j]]) * k_unmapped
    return pos


# ----------------------------------------------------------------------------------------------------------------------

# ----------------------------------------------------------------------------------------------------------------------
# Jiggle strands.  Ratchet has no joints for a cape, hair or pendants, but he has spare ones nothing is skinned to (his
# tail and face joints).  Each strand below borrows one: the vertices of a cluster of her cloth / hair bones are partly
# skinned to it, more toward the free end, and the engine (`rc-formats` `apply_jiggle`) replaces that joint's palette
# with its anchor joint's, swung by a spring about the strand's pivot.  A strand's joint is not driven by his
# animations at all, so the joint numbers only have to be unused ones that no item attaches to.

CLUSTERS = [
    # name, her bone families, anchor joint, strip centres across x (glTF), spare joints, spring Hz, damping ratio,
    # largest swing (degrees), share of the skin the strand takes at the free end, and how fast that grows along it
    dict(name="cape", fams=("F_", "F"), anchor=6, strips=[-0.20, 0.0, 0.20], joints=[110, 47, 48], freq=2.2, damp=0.45, max_deg=24, share=0.85, power=1.3),
    dict(name="train", fams=("后摆",), anchor=PELVIS, strips=[-0.22, 0.22], joints=[49, 50], freq=2.0, damp=0.5, max_deg=22, share=0.80, power=1.2),
    dict(name="hair", fams=("HairE",), anchor=HEAD, strips=[-0.07, 0.07], joints=[51, 37], freq=3.5, damp=0.35, max_deg=26, share=0.80, power=1.2),
    dict(name="pendants", fams=("Bone_PendantA", "Bone_PendantB", "Bone_PendantC"), anchor=HEAD, strips=[-0.15, 0.15], joints=[38, 39], freq=4.0, damp=0.30, max_deg=35, share=0.90, power=1.0),
]


def bone_family(name):
    if re.match(r"^(D_|F_|S_)\d", name):
        return name[:2]
    if name.startswith("后摆"):
        return "后摆"
    if name.startswith("前裙"):
        return "前裙"
    m = re.match(r"^(Bone_[A-Za-z]+|HairE|F)", name)
    return m.group(1) if m else None


def strip_weights(x, centres):
    """Linear partition of unity across strips by x (clamped beyond the outer ones)."""
    n = len(centres)
    w = np.zeros((len(x), n))
    for i, xi in enumerate(x):
        if xi <= centres[0]:
            w[i, 0] = 1.0
        elif xi >= centres[-1]:
            w[i, -1] = 1.0
        else:
            k = int(np.searchsorted(centres, xi) - 1)
            f = (xi - centres[k]) / (centres[k + 1] - centres[k])
            w[i, k], w[i, k + 1] = 1.0 - f, f
    return w


def add_jiggle(rig, pos, joints, weights, sj, sw, sc):
    """Skins the cluster vertices partly to their strand joints.  `pos` are the T-posed vertices (glTF axes).
    Returns the new (sj, sw, sc) and the strand table [dict(joint, anchor, freq, damp, max_deg, pivot, tip)] (glTF axes)."""
    fam_of = [bone_family(n) for n in rig.names]
    n = len(pos)
    extra = [dict() for _ in range(n)]  # vertex -> {strand joint: share}
    strands = []
    for c in CLUSTERS:
        bones = [i for i, f in enumerate(fam_of) if f in c["fams"]]
        mass = (np.isin(joints, bones) * weights).sum(axis=1)
        sel = np.where(mass > 0.05)[0]
        if len(sel) == 0:
            continue
        core = mass > 0.5
        y_top, y_bot = pos[core, 1].max(), pos[core, 1].min()
        f = np.clip((y_top - pos[sel, 1]) / max(y_top - y_bot, 1e-6), 0.0, 1.0)
        tents = strip_weights(pos[sel, 0], c["strips"])
        share = mass[sel] * c["share"] * f ** c["power"]
        for k, jt in enumerate(c["joints"][: len(c["strips"])]):
            wk = tents[:, k] * share
            for v, w in zip(sel, wk):
                if w > 1e-4:
                    extra[v][jt] = extra[v].get(jt, 0.0) + float(w)
            # the strand's pivot and free end: the top and bottom of the strip's own vertices along its length
            mine = sel[(tents[:, k] > 0.5) & core[sel]]
            if len(mine) < 8:
                mine = sel[tents[:, k] > 0.25]
            if len(mine) < 8:
                continue
            fm = np.clip((y_top - pos[mine, 1]) / max(y_top - y_bot, 1e-6), 0.0, 1.0)
            top = pos[mine[fm < 0.08]].mean(axis=0) if (fm < 0.08).any() else pos[mine[np.argmin(fm)]]
            bot = pos[mine[fm > 0.92]].mean(axis=0) if (fm > 0.92).any() else pos[mine[np.argmax(fm)]]
            strands.append(dict(joint=jt, anchor=c["anchor"], freq=c["freq"], damp=c["damp"], max_deg=c["max_deg"], pivot=top, tip=bot, name=c["name"]))
    sj2, sw2, sc2 = sj.copy(), sw.copy(), sc.copy()
    for v in range(n):
        if not extra[v]:
            continue
        total = min(sum(extra[v].values()), 0.95)
        scale_x = sum(extra[v].values())
        acc = {j: w * (total / scale_x) for j, w in extra[v].items()}
        for k in range(int(sc[v])):
            j = int(sj[v, k])
            acc[j] = acc.get(j, 0.0) + sw[v, k] / 256.0 * (1.0 - total)
        top = sorted(acc.items(), key=lambda t: -t[1])[:3]
        tot = sum(w for _, w in top)
        q = [w / tot * 256.0 for _, w in top]
        qi = [int(x) for x in q]
        for i in sorted(range(len(q)), key=lambda i: -(q[i] - qi[i]))[: 256 - sum(qi)]:
            qi[i] += 1
        keep = [(j, w) for (j, _), w in zip(top, qi) if w > 0]
        sj2[v], sw2[v], sc2[v] = 0, 0, len(keep)
        for i, (j, w) in enumerate(keep):
            sj2[v, i], sw2[v, i] = j, w
    return sj2, sw2, sc2, strands


RATCHET_BOTTOM, RATCHET_TOP = 0.002, 1.416  # his bind-pose box in z (lauma_dump.txt); the engine fits her to exactly his box


def foot_fronts(rig, pos, joints, weights):
    """z of the front of each foot: the farthest-forward vertex clearly weighted to the ankle or toe bone."""
    out = {}
    for s in "LR":
        sel = [rig.index[f"足先EX.{s}"], rig.index[f"足首D.{s}"]]
        rows = np.where((np.isin(joints, sel) & (weights > 0.3)).any(axis=1))[0]
        out[s] = float(pos[rows, 2].max())
    return out


def retarget(names, parent, bone_pos, pos, nrm, joints, weights, dump_path):
    """Everything the converter needs for a rigged hero model.

    names / parent / bone_pos: her skeleton (glTF axes).  pos / nrm / joints / weights: her vertices (all primitives).
    Returns a dict:
      pos, nrm      the vertices posed into the T pose (glTF axes)
      skin          (joints[n, 3] u8, weights[n, 3] u16, count[n] u8): Ratchet joints, weights summing to 256
      joints_game   [111, 3]: where each of his joints sits in her T pose (game axes, model frame)
      jiggle        the spring strands (see CLUSTERS): joint, anchor, spring, pivot and free end (game axes, model frame)
      report        text for the converter's log
    """
    r_parent, r_rest = parse_dump(dump_path)
    rig = Rig(names, parent, bone_pos)
    pos_t, nrm_t, bones, angles = pose_to_t(rig, pos, nrm, joints, weights)
    front = foot_fronts(rig, pos_t, joints, weights)
    tg = joint_targets(rig, bones, front)
    chains = build_chains(tg)
    fixed, kind = bone_maps(rig, chains)
    sj, sw, sc = vertex_skin(rig, fixed, kind, chains, pos, joints, weights)
    sj, sw, sc, strands = add_jiggle(rig, pos_t, joints, weights, sj, sw, sc)
    ymin, ymax = float(pos_t[:, 1].min()), float(pos_t[:, 1].max())
    scale = (RATCHET_TOP - RATCHET_BOTTOM) / (ymax - ymin)
    jg = skeleton_positions(tg, r_parent, r_rest, scale, ymin)
    used = np.zeros(N_JOINTS)
    for k in range(3):
        for v in np.where(sc > k)[0]:
            used[sj[v, k]] += sw[v, k] / 256.0
    lines = [
        f"rig: arms raised {angles['L']:.1f} / {angles['R']:.1f} degrees to the T pose; fit scale {scale:.4f}",
        f"rig: skin influences per vertex 1/2/3: {np.bincount(sc, minlength=4)[1:].tolist()}; joints used: {int((used > 0).sum())} of {N_JOINTS}",
        "rig: leg length (hip to floor) hers {:.3f} m vs his {:.3f} m; shoulder height hers {:.3f} m vs his {:.3f} m".format(
            (tg[92][1] - ymin) * scale, r_rest[92][2], (tg[53][1] - ymin) * scale, r_rest[53][2]),
    ]
    for st in strands:
        st["pivot"], st["tip"] = to_game(np.asarray(st["pivot"])), to_game(np.asarray(st["tip"]))
    lines.append("rig: jiggle strands: " + ", ".join(f"{st['name']}->{st['joint']}(len {np.linalg.norm(st['tip'] - st['pivot']) * scale:.2f} m)" for st in strands))
    return dict(pos=pos_t, nrm=nrm_t, skin=(sj, sw, sc), joints_game=jg, jiggle=strands, report="\n".join(lines))
