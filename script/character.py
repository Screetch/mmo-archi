"""Write crates/mmo-render/assets/character.glb: the character of the game.

The file is a skinned glTF of seven joints with a clip named Idle, a base colour map and a normal map.
It uses only the standard library, so the same run always writes the same bytes.

Usage: python3 script/character.py <output.glb>
"""

import json
import math
import struct
import sys
import zlib

# The joints, as (name, parent, place of the joint in the rest pose, in metres, y up, the front on +z).
JOINTS = [
  ("hips", None, (0.0, 0.95, 0.0)),
  ("spine", 0, (0.0, 1.2, 0.0)),
  ("head", 1, (0.0, 1.6, 0.0)),
  ("arm_left", 1, (0.25, 1.35, 0.0)),
  ("arm_right", 1, (-0.25, 1.35, 0.0)),
  ("leg_left", 0, (0.12, 0.95, 0.0)),
  ("leg_right", 0, (-0.12, 0.95, 0.0)),
]

# The boxes of the body, as (joint, centre, half extents). Each box moves with one joint.
BOXES = [
  (0, (0.0, 1.05, 0.0), (0.18, 0.1, 0.1)),
  (1, (0.0, 1.32, 0.0), (0.2, 0.17, 0.11)),
  (2, (0.0, 1.7, 0.0), (0.11, 0.12, 0.11)),
  (3, (0.31, 1.15, 0.0), (0.06, 0.25, 0.06)),
  (4, (-0.31, 1.15, 0.0), (0.06, 0.25, 0.06)),
  (5, (0.12, 0.47, 0.0), (0.08, 0.47, 0.08)),
  (6, (-0.12, 0.47, 0.0), (0.08, 0.47, 0.08)),
]

# The faces of a box, as (normal, direction of the texture coordinate u).
FACES = [
  ((1, 0, 0), (0, 0, -1)),
  ((-1, 0, 0), (0, 0, 1)),
  ((0, 1, 0), (1, 0, 0)),
  ((0, -1, 0), (1, 0, 0)),
  ((0, 0, 1), (1, 0, 0)),
  ((0, 0, -1), (-1, 0, 0)),
]

IDLE = 2.0
TILE = 8


def cross(a, b):
  return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def quaternion(axis, degrees):
  half = math.radians(degrees) / 2
  return (*(math.sin(half) * a for a in axis), math.cos(half))


def mesh():
  positions, normals, uvs, tangents, joints, indices = [], [], [], [], [], []
  for joint, centre, half in BOXES:
    for normal, tangent in FACES:
      bitangent = cross(normal, tangent)
      base = len(positions)
      for t, b in ((-1, -1), (1, -1), (1, 1), (-1, 1)):
        positions.append(
          tuple(
            centre[i] + half[i] * (normal[i] + tangent[i] * t + bitangent[i] * b) for i in range(3)
          )
        )
        normals.append(normal)
        uvs.append(((1 + t) / 2, (1 - b) / 2))
        tangents.append((*tangent, 1.0))
        joints.append((joint, 0, 0, 0))
      indices += [base, base + 1, base + 2, base, base + 2, base + 3]
  return positions, normals, uvs, tangents, joints, indices


def png(width, height, pixels):
  def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

  rows = b"".join(b"\x00" + bytes(c for p in pixels[y * width : (y + 1) * width] for c in p) for y in range(height))
  header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
  return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")


def colour_map():
  size = 2 * TILE
  pixels = []
  for y in range(size):
    for x in range(size):
      dark = (x // TILE + y // TILE) % 2
      pixels.append((70, 90, 140, 255) if dark else (190, 170, 140, 255))
  return png(size, size, pixels)


def normal_map():
  size = 2 * TILE
  pixels = []
  for y in range(size):
    for x in range(size):
      edge = x % TILE, y % TILE
      nx = -0.5 if edge[0] == 0 else 0.5 if edge[0] == TILE - 1 else 0.0
      ny = 0.5 if edge[1] == 0 else -0.5 if edge[1] == TILE - 1 else 0.0
      length = math.sqrt(nx * nx + ny * ny + 1)
      pixels.append(tuple(round((v / length * 0.5 + 0.5) * 255) for v in (nx, ny, 1.0)) + (255,))
  return png(size, size, pixels)


class Binary:
  def __init__(self):
    self.data = bytearray()
    self.views = []
    self.accessors = []

  def view(self, payload):
    while len(self.data) % 4:
      self.data.append(0)
    self.views.append({"buffer": 0, "byteOffset": len(self.data), "byteLength": len(payload)})
    self.data += payload
    return len(self.views) - 1

  def accessor(self, payload, kind, component, count, **extra):
    self.accessors.append(
      {"bufferView": self.view(payload), "componentType": component, "count": count, "type": kind, **extra}
    )
    return len(self.accessors) - 1


def floats(rows):
  return b"".join(struct.pack("<%df" % len(row), *row) for row in rows)


def build():
  positions, normals, uvs, tangents, joints, indices = mesh()
  binary = Binary()
  count = len(positions)
  low = [min(p[i] for p in positions) for i in range(3)]
  high = [max(p[i] for p in positions) for i in range(3)]
  attributes = {
    "POSITION": binary.accessor(floats(positions), "VEC3", 5126, count, min=low, max=high),
    "NORMAL": binary.accessor(floats(normals), "VEC3", 5126, count),
    "TEXCOORD_0": binary.accessor(floats(uvs), "VEC2", 5126, count),
    "TANGENT": binary.accessor(floats(tangents), "VEC4", 5126, count),
    "JOINTS_0": binary.accessor(b"".join(bytes(j) for j in joints), "VEC4", 5121, count),
    "WEIGHTS_0": binary.accessor(floats([(1.0, 0.0, 0.0, 0.0)] * count), "VEC4", 5126, count),
  }
  index = binary.accessor(struct.pack("<%dH" % len(indices), *indices), "SCALAR", 5123, len(indices))
  inverse = floats(
    [(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, -p[0], -p[1], -p[2], 1) for _, _, p in JOINTS]
  )
  bind = binary.accessor(inverse, "MAT4", 5126, len(JOINTS))
  times = binary.accessor(floats([(0.0,), (IDLE / 2,), (IDLE,)]), "SCALAR", 5126, 3, min=[0.0], max=[IDLE])

  def turn(axis, degrees):
    return binary.accessor(
      floats([quaternion(axis, d) for d in (0, degrees, 0)]), "VEC4", 5126, 3
    )

  bob = binary.accessor(floats([(0, 0.95, 0), (0, 0.96, 0), (0, 0.95, 0)]), "VEC3", 5126, 3)
  outputs = [
    (1, "rotation", turn((1, 0, 0), 3)),
    (2, "rotation", turn((0, 1, 0), 4)),
    (3, "rotation", turn((0, 0, 1), 5)),
    (4, "rotation", turn((0, 0, 1), -5)),
    (0, "translation", bob),
  ]
  animation = {
    "name": "Idle",
    "samplers": [{"input": times, "output": out, "interpolation": "LINEAR"} for _, _, out in outputs],
    "channels": [
      {"sampler": n, "target": {"node": node, "path": path}} for n, (node, path, _) in enumerate(outputs)
    ],
  }
  colour = binary.view(colour_map())
  normal = binary.view(normal_map())
  nodes = []
  for name, parent, place in JOINTS:
    base = JOINTS[parent][2] if parent is not None else (0.0, 0.0, 0.0)
    nodes.append({"name": name, "translation": [place[i] - base[i] for i in range(3)]})
  for n, (_, parent, _) in enumerate(JOINTS):
    if parent is not None:
      nodes[parent].setdefault("children", []).append(n)
  nodes.append({"name": "Character", "mesh": 0, "skin": 0})
  document = {
    "asset": {"version": "2.0", "generator": "script/character.py"},
    "scene": 0,
    "scenes": [{"nodes": [len(JOINTS), 0]}],
    "nodes": nodes,
    "skins": [{"joints": list(range(len(JOINTS))), "skeleton": 0, "inverseBindMatrices": bind}],
    "meshes": [{"name": "Character", "primitives": [{"attributes": attributes, "indices": index, "material": 0}]}],
    "materials": [
      {
        "name": "Cloth",
        "pbrMetallicRoughness": {
          "baseColorTexture": {"index": 0},
          "metallicFactor": 0.2,
          "roughnessFactor": 0.6,
        },
        "normalTexture": {"index": 1},
      }
    ],
    "textures": [{"source": 0, "sampler": 0}, {"source": 1, "sampler": 0}],
    "images": [
      {"bufferView": colour, "mimeType": "image/png"},
      {"bufferView": normal, "mimeType": "image/png"},
    ],
    "samplers": [{"magFilter": 9729, "minFilter": 9987, "wrapS": 10497, "wrapT": 10497}],
    "animations": [animation],
    "accessors": binary.accessors,
    "bufferViews": binary.views,
    "buffers": [{"byteLength": len(binary.data)}],
  }
  text = json.dumps(document, separators=(",", ":")).encode()
  text += b" " * (-len(text) % 4)
  payload = bytes(binary.data) + b"\x00" * (-len(binary.data) % 4)
  body = struct.pack("<II", len(text), 0x4E4F534A) + text + struct.pack("<II", len(payload), 0x004E4942) + payload
  return struct.pack("<III", 0x46546C67, 2, 12 + len(body)) + body


if __name__ == "__main__":
  with open(sys.argv[1], "wb") as out:
    out.write(build())
