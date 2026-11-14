#!/usr/bin/env node
/**
 * Parametric Binary glTF 2.0 (.glb) Synthesizer
 * Target: Codex OmniBridge Smart 3-Way Router Stage (M3 Expressive Edition)
 *
 * Clean, open industrial architecture (zero fog-inducing enclosures) visually
 * demonstrating:
 *   Unified Ingress (`127.0.0.1:8787`) -> Catalog Rings -> Rotary Crossbar ->
 *     - Upper Lane : Official OpenAI Direct Bypass (`Z = -1.12`)
 *     - Middle Lane: Protocol Translator + Isolated Keychain Vault (`Z = 0.0`)
 *     - Lower Lane : Local Offline Loopback (`127.0.0.1`, `Z = +1.12`)
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as THREE from 'three';
import { mergeGeometries, mergeVertices } from 'three/examples/jsm/utils/BufferGeometryUtils.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_OUT = path.resolve(__dirname, '../public/models/omnibridge-photonic-matrix.glb');
const OUT_PATH = process.argv[2] ? path.resolve(process.argv[2]) : DEFAULT_OUT;

const MATERIALS = [
  {
    name: 'Platform_Ceramic',
    baseColorFactor: [0.85, 0.91, 0.92, 1.0],
    metallicFactor: 0.06,
    roughnessFactor: 0.44,
  },
  {
    name: 'Chassis_Slate',
    baseColorFactor: [0.22, 0.30, 0.32, 1.0],
    metallicFactor: 0.58,
    roughnessFactor: 0.34,
  },
  {
    name: 'Hardware_Silver',
    baseColorFactor: [0.76, 0.82, 0.84, 1.0],
    metallicFactor: 0.86,
    roughnessFactor: 0.24,
  },
  {
    name: 'Hardware_Copper',
    baseColorFactor: [0.76, 0.52, 0.30, 1.0],
    metallicFactor: 0.82,
    roughnessFactor: 0.26,
  },
  {
    name: 'Crystal_Lens',
    baseColorFactor: [0.68, 0.91, 0.96, 0.36],
    metallicFactor: 0.02,
    roughnessFactor: 0.08,
    alphaMode: 'BLEND',
    doubleSided: false,
  },
  {
    name: 'Indicator_Official',
    baseColorFactor: [0.0, 0.65, 0.72, 1.0],
    metallicFactor: 0.12,
    roughnessFactor: 0.25,
    emissiveFactor: [0.0, 0.35, 0.40],
  },
  {
    name: 'Indicator_Bridge',
    baseColorFactor: [0.58, 0.31, 0.63, 1.0],
    metallicFactor: 0.12,
    roughnessFactor: 0.25,
    emissiveFactor: [0.34, 0.16, 0.38],
  },
  {
    name: 'Indicator_Vault',
    baseColorFactor: [0.78, 0.50, 0.05, 1.0],
    metallicFactor: 0.12,
    roughnessFactor: 0.25,
    emissiveFactor: [0.42, 0.25, 0.02],
  },
  {
    name: 'Indicator_Local',
    baseColorFactor: [0.20, 0.54, 0.24, 1.0],
    metallicFactor: 0.12,
    roughnessFactor: 0.25,
    emissiveFactor: [0.10, 0.34, 0.14],
  },
];

const MAT = Object.fromEntries(MATERIALS.map((m, idx) => [m.name, idx]));

function prep(geom, transform) {
  const g = geom.index ? geom.toNonIndexed() : geom.clone();
  if (!g.getAttribute('normal')) {
    g.computeVertexNormals();
  }
  const clean = new THREE.BufferGeometry();
  clean.setAttribute('position', g.getAttribute('position'));
  clean.setAttribute('normal', g.getAttribute('normal'));
  if (transform) {
    clean.applyMatrix4(transform);
  }
  return clean;
}

function trs(x = 0, y = 0, z = 0, rx = 0, ry = 0, rz = 0, sx = 1, sy = 1, sz = 1) {
  const m = new THREE.Matrix4();
  const q = new THREE.Quaternion().setFromEuler(new THREE.Euler(rx, ry, rz));
  m.compose(new THREE.Vector3(x, y, z), q, new THREE.Vector3(sx, sy, sz));
  return m;
}

function chamferedPlate(w, h, d, bevel = 0.22) {
  const hw = w / 2;
  const hd = d / 2;
  const b = Math.min(bevel, hw * 0.4, hd * 0.4);
  const shape = new THREE.Shape();
  shape.moveTo(-hw + b, -hd);
  shape.lineTo(hw - b, -hd);
  shape.lineTo(hw, -hd + b);
  shape.lineTo(hw, hd - b);
  shape.lineTo(hw - b, hd);
  shape.lineTo(-hw + b, hd);
  shape.lineTo(-hw, hd - b);
  shape.lineTo(-hw, -hd + b);
  shape.closePath();

  const extrude = new THREE.ExtrudeGeometry(shape, {
    depth: h,
    bevelEnabled: true,
    bevelThickness: Math.min(0.025, h * 0.22),
    bevelSize: Math.min(0.025, b * 0.25),
    bevelSegments: 2,
    curveSegments: 4,
  });
  extrude.rotateX(-Math.PI / 2);
  extrude.translate(0, -h / 2, 0);
  extrude.computeVertexNormals();
  return extrude;
}

function hollowRing(outerR, innerR, height, radialSegs = 40) {
  const shape = new THREE.Shape();
  shape.absarc(0, 0, outerR, 0, Math.PI * 2, false);
  const hole = new THREE.Path();
  hole.absarc(0, 0, innerR, 0, Math.PI * 2, true);
  shape.holes.push(hole);

  const g = new THREE.ExtrudeGeometry(shape, {
    depth: height,
    bevelEnabled: true,
    bevelThickness: Math.min(0.012, height * 0.18),
    bevelSize: Math.min(0.012, (outerR - innerR) * 0.18),
    bevelSegments: 1,
    curveSegments: radialSegs,
  });
  g.translate(0, 0, -height / 2);
  g.computeVertexNormals();
  return g;
}

function mergeList(geoms) {
  if (geoms.length === 0) return null;
  if (geoms.length === 1) return geoms[0];
  return mergeGeometries(geoms, false);
}

function buildRouterStageHierarchy() {
  const nodes = [];

  function addNode({
    name,
    translation = [0, 0, 0],
    rotation = [0, 0, 0, 1],
    scale = [1, 1, 1],
    primitives = [],
    children = [],
  }) {
    const idx = nodes.length;
    nodes.push({ name, translation, rotation, scale, primitives, children });
    return idx;
  }

  // 1. Chassis_Base: Clean M3-sculpted tabletop ceramic & anodized slate deck
  {
    const ceramicParts = [];
    const slateParts = [];
    const silverParts = [];
    const accentParts = [];

    ceramicParts.push(prep(chamferedPlate(6.4, 0.20, 3.85, 0.42), trs(0, -0.44, 0)));
    slateParts.push(prep(chamferedPlate(5.95, 0.08, 3.45, 0.34), trs(0, -0.32, 0)));
    ceramicParts.push(prep(chamferedPlate(5.65, 0.05, 3.16, 0.26), trs(0, -0.27, 0)));

    // 4 clean aluminum feet
    for (const sx of [-2.6, 2.6]) {
      for (const sz of [-1.45, 1.45]) {
        slateParts.push(prep(new THREE.CylinderGeometry(0.19, 0.22, 0.12, 24), trs(sx, -0.59, sz)));
        silverParts.push(prep(new THREE.TorusGeometry(0.20, 0.016, 10, 24), trs(sx, -0.54, sz, Math.PI / 2, 0, 0)));
      }
    }

    // Subtle track guide beds for the 3 output lanes
    for (const z of [-1.12, 0.0, 1.12]) {
      silverParts.push(prep(new THREE.BoxGeometry(5.1, 0.025, 0.08), trs(0.15, -0.24, z)));
    }

    // Front & rear slim status strips
    accentParts.push(prep(new THREE.BoxGeometry(4.8, 0.016, 0.022), trs(0, -0.30, 1.73)));
    accentParts.push(prep(new THREE.BoxGeometry(4.8, 0.016, 0.022), trs(0, -0.30, -1.73)));

    addNode({
      name: 'Chassis_Base',
      primitives: [
        { geom: mergeList(ceramicParts), material: MAT.Platform_Ceramic },
        { geom: mergeList(slateParts), material: MAT.Chassis_Slate },
        { geom: mergeList(silverParts), material: MAT.Hardware_Silver },
        { geom: mergeList(accentParts), material: MAT.Indicator_Official },
      ],
    });
  }

  // 2. Module_Ingress: Codex Unified Entry (`127.0.0.1:8787`)
  {
    const slate = [];
    const silver = [];
    const lens = [];
    const accent = [];

    slate.push(prep(chamferedPlate(0.68, 0.44, 0.88, 0.14), trs(-0.08, -0.04, 0)));
    silver.push(prep(new THREE.CylinderGeometry(0.30, 0.34, 0.46, 32), trs(0.22, 0.06, 0, 0, 0, Math.PI / 2)));
    silver.push(prep(new THREE.CylinderGeometry(0.065, 0.065, 0.20, 16), trs(-0.42, 0.08, -0.22, 0, 0, Math.PI / 2)));
    silver.push(prep(new THREE.CylinderGeometry(0.065, 0.065, 0.20, 16), trs(-0.42, 0.08, 0.22, 0, 0, Math.PI / 2)));
    lens.push(prep(new THREE.CylinderGeometry(0.19, 0.19, 0.06, 28), trs(0.46, 0.06, 0, 0, 0, Math.PI / 2)));
    accent.push(prep(new THREE.TorusGeometry(0.24, 0.022, 12, 32), trs(0.47, 0.06, 0, 0, Math.PI / 2, 0)));

    addNode({
      name: 'Module_Ingress',
      translation: [-2.55, 0.04, 0],
      primitives: [
        { geom: mergeList(slate), material: MAT.Chassis_Slate },
        { geom: mergeList(silver), material: MAT.Hardware_Silver },
        { geom: mergeList(lens), material: MAT.Crystal_Lens },
        { geom: mergeList(accent), material: MAT.Indicator_Official },
      ],
    });
  }

  // 3. Module_Registry: Model Catalog Rings (`crates/catalog`)
  {
    const outer = [];
    const outerAccent = [];
    outer.push(prep(hollowRing(0.50, 0.39, 0.065, 44), trs(0, 0, 0, 0, Math.PI / 2, 0)));
    for (let i = 0; i < 8; i++) {
      const a = (i / 8) * Math.PI * 2;
      outer.push(
        prep(
          new THREE.BoxGeometry(0.08, 0.045, 0.032),
          trs(0, Math.cos(a) * 0.515, Math.sin(a) * 0.515, a, 0, 0),
        ),
      );
    }
    outerAccent.push(prep(new THREE.TorusGeometry(0.44, 0.015, 10, 40), trs(0.036, 0, 0, 0, Math.PI / 2, 0)));

    const wheelOuterIdx = addNode({
      name: 'Registry_Wheel_Outer',
      translation: [-0.07, 0.08, 0],
      primitives: [
        { geom: mergeList(outer), material: MAT.Hardware_Silver },
        { geom: mergeList(outerAccent), material: MAT.Indicator_Official },
      ],
    });

    const innerFrame = [];
    innerFrame.push(prep(hollowRing(0.35, 0.24, 0.055, 36), trs(0, 0, 0, 0, Math.PI / 2, 0)));
    const wheelInnerIdx = addNode({
      name: 'Registry_Wheel_Inner',
      translation: [0.07, 0.08, 0],
      primitives: [{ geom: mergeList(innerFrame), material: MAT.Chassis_Slate }],
    });

    const cradle = [];
    cradle.push(prep(chamferedPlate(0.42, 0.14, 1.18, 0.10), trs(0, -0.22, 0)));
    cradle.push(prep(new THREE.BoxGeometry(0.22, 0.54, 0.08), trs(0, 0.02, -0.56)));
    cradle.push(prep(new THREE.BoxGeometry(0.22, 0.54, 0.08), trs(0, 0.02, 0.56)));

    addNode({
      name: 'Module_Registry',
      translation: [-1.48, 0.02, 0],
      primitives: [{ geom: mergeList(cradle), material: MAT.Chassis_Slate }],
      children: [wheelOuterIdx, wheelInnerIdx],
    });
  }

  // 4. Module_Crossbar: Open-frame Directional Rotary Switch (`crates/router`)
  {
    const pitchMetal = [];
    const pitchLens = [];
    const pitchPointer = [];

    // Rotary prism block + sleek directional pointer fin pointing downstream (+X)
    pitchMetal.push(prep(hollowRing(0.29, 0.22, 0.06, 32), trs(0, 0, 0)));
    pitchLens.push(prep(new THREE.CylinderGeometry(0.20, 0.20, 0.24, 8), trs(0, 0, 0)));
    pitchPointer.push(prep(new THREE.BoxGeometry(0.34, 0.06, 0.055), trs(0.28, 0, 0)));
    pitchPointer.push(prep(new THREE.TorusGeometry(0.21, 0.016, 10, 32), trs(0, 0.13, 0, Math.PI / 2, 0, 0)));

    const pitchIdx = addNode({
      name: 'Crossbar_Gimbal_Pitch',
      translation: [0, 0, 0],
      primitives: [
        { geom: mergeList(pitchMetal), material: MAT.Hardware_Silver },
        { geom: mergeList(pitchLens), material: MAT.Crystal_Lens },
        { geom: mergeList(pitchPointer), material: MAT.Indicator_Official },
      ],
    });

    const yawMetal = [];
    yawMetal.push(prep(hollowRing(0.43, 0.35, 0.07, 40), trs(0, 0, 0, Math.PI / 2, 0, 0)));
    yawMetal.push(prep(new THREE.CylinderGeometry(0.028, 0.028, 0.88, 12), trs(0, 0, 0, Math.PI / 2, 0, 0)));

    const yawIdx = addNode({
      name: 'Crossbar_Gimbal_Yaw',
      translation: [0, 0.11, 0],
      primitives: [{ geom: mergeList(yawMetal), material: MAT.Hardware_Silver }],
      children: [pitchIdx],
    });

    const baseParts = [];
    const copperAccents = [];
    baseParts.push(prep(chamferedPlate(1.02, 0.14, 1.08, 0.16), trs(0, -0.22, 0)));
    baseParts.push(prep(new THREE.CylinderGeometry(0.48, 0.54, 0.09, 32), trs(0, -0.13, 0)));
    // Slim open arch holding rotary spindle
    baseParts.push(prep(new THREE.BoxGeometry(0.11, 0.58, 0.09), trs(0, 0.12, -0.49)));
    baseParts.push(prep(new THREE.BoxGeometry(0.11, 0.58, 0.09), trs(0, 0.12, 0.49)));
    baseParts.push(prep(new THREE.BoxGeometry(0.13, 0.07, 1.06), trs(0, 0.43, 0)));

    for (const [ax, az] of [[-0.36, -0.38], [0.36, -0.38], [-0.36, 0.38], [0.36, 0.38]]) {
      copperAccents.push(prep(new THREE.CylinderGeometry(0.068, 0.068, 0.14, 20), trs(ax, -0.07, az)));
    }

    addNode({
      name: 'Module_Crossbar',
      translation: [-0.36, 0.02, 0],
      primitives: [
        { geom: mergeList(baseParts), material: MAT.Chassis_Slate },
        { geom: mergeList(copperAccents), material: MAT.Hardware_Copper },
      ],
      children: [yawIdx],
    });
  }

  // 5. Channel_Official_Bypass: Upper Straight Direct Rail (Z = -1.12)
  {
    const posts = [];
    const rail = [];
    const cyan = [];

    posts.push(prep(new THREE.CylinderGeometry(0.14, 0.17, 0.36, 20), trs(-0.36, -0.06, -1.12)));
    rail.push(prep(new THREE.CylinderGeometry(0.065, 0.065, 2.38, 20), trs(1.02, 0.12, -1.12, 0, 0, Math.PI / 2)));

    for (const px of [0.22, 1.05, 1.88]) {
      posts.push(prep(new THREE.BoxGeometry(0.10, 0.30, 0.22), trs(px, -0.08, -1.12)));
      cyan.push(prep(new THREE.TorusGeometry(0.11, 0.018, 10, 24), trs(px, 0.12, -1.12, 0, Math.PI / 2, 0)));
    }

    addNode({
      name: 'Channel_Official_Bypass',
      primitives: [
        { geom: mergeList(posts), material: MAT.Chassis_Slate },
        { geom: mergeList(rail), material: MAT.Hardware_Silver },
        { geom: mergeList(cyan), material: MAT.Indicator_Official },
      ],
    });
  }

  // 6. Module_ProtocolBridge: Clean Protocol Translator Bay (X = +1.06, Z = 0)
  {
    const stageSlate = [];
    const silverMounts = [];
    const lenses = [];
    const plumRings = [];

    stageSlate.push(prep(chamferedPlate(1.56, 0.11, 0.86, 0.14), trs(0, -0.22, 0)));
    for (const p of [
      { x: -0.46, ry: 0.28 },
      { x: 0.0, ry: -0.32 },
      { x: 0.46, ry: 0.24 },
    ]) {
      silverMounts.push(prep(new THREE.CylinderGeometry(0.20, 0.22, 0.09, 24), trs(p.x, -0.14, 0)));
      // Clean vertical thin translator lens disc framed by metallic ring
      silverMounts.push(prep(hollowRing(0.20, 0.15, 0.04, 28), trs(p.x, 0.12, 0, 0, Math.PI / 2 + p.ry, 0)));
      lenses.push(prep(new THREE.CylinderGeometry(0.15, 0.15, 0.032, 24), trs(p.x, 0.12, 0, 0, p.ry, Math.PI / 2)));
      plumRings.push(prep(new THREE.TorusGeometry(0.20, 0.014, 8, 24), trs(p.x, -0.08, 0, Math.PI / 2, 0, 0)));
    }

    addNode({
      name: 'Module_ProtocolBridge',
      translation: [1.06, 0.02, 0],
      primitives: [
        { geom: mergeList(stageSlate), material: MAT.Chassis_Slate },
        { geom: mergeList(silverMounts), material: MAT.Hardware_Silver },
        { geom: mergeList(lenses), material: MAT.Crystal_Lens },
        { geom: mergeList(plumRings), material: MAT.Indicator_Bridge },
      ],
    });
  }

  // 7. Module_KeychainVault + Physical Isolation Divider (`crates/credentials`)
  {
    const rotorMetal = [];
    const rotorAccent = [];
    rotorMetal.push(prep(hollowRing(0.29, 0.11, 0.055, 30), trs(0, 0, 0, Math.PI / 2, 0, 0)));
    for (let i = 0; i < 6; i++) {
      const a = (i / 6) * Math.PI * 2;
      rotorMetal.push(
        prep(
          new THREE.BoxGeometry(0.055, 0.05, 0.12),
          trs(Math.cos(a) * 0.21, 0.018, Math.sin(a) * 0.21, 0, -a, 0),
        ),
      );
    }
    rotorAccent.push(prep(new THREE.TorusGeometry(0.17, 0.016, 10, 28), trs(0, 0.032, 0, Math.PI / 2, 0, 0)));

    const vaultRotorIdx = addNode({
      name: 'Vault_LockDial',
      translation: [-0.24, 0.25, 0],
      primitives: [
        { geom: mergeList(rotorMetal), material: MAT.Hardware_Copper },
        { geom: mergeList(rotorAccent), material: MAT.Indicator_Vault },
      ],
    });

    const vaultBody = [];
    const vaultRibs = [];
    const vaultAccent = [];

    vaultBody.push(prep(chamferedPlate(0.92, 0.12, 0.72, 0.12), trs(-0.24, -0.22, 0)));
    vaultBody.push(prep(new THREE.CylinderGeometry(0.34, 0.36, 0.40, 32), trs(-0.24, 0.01, 0)));
    for (let r = -1; r <= 1; r++) {
      vaultRibs.push(prep(new THREE.TorusGeometry(0.36, 0.016, 10, 32), trs(-0.24, 0.02 + r * 0.09, 0, Math.PI / 2, 0, 0)));
    }
    // Crisp physical separation wall between Official Lane (Z = -1.12) and Third-Party Lanes (Z >= 0)
    vaultBody.push(prep(new THREE.BoxGeometry(2.45, 0.34, 0.055), trs(0.55, -0.06, -1.86)));
    vaultRibs.push(prep(new THREE.BoxGeometry(2.48, 0.036, 0.08), trs(0.55, 0.12, -1.86)));
    vaultAccent.push(prep(new THREE.BoxGeometry(2.38, 0.016, 0.065), trs(0.55, 0.07, -1.86)));

    addNode({
      name: 'Module_KeychainVault',
      translation: [0.22, 0.02, 1.24],
      primitives: [
        { geom: mergeList(vaultBody), material: MAT.Chassis_Slate },
        { geom: mergeList(vaultRibs), material: MAT.Hardware_Silver },
        { geom: mergeList(vaultAccent), material: MAT.Indicator_Vault },
      ],
      children: [vaultRotorIdx],
    });
  }

  // 8. Module_LocalLoop: Local Offline Loopback Ring (`127.0.0.1`, Lower Lane Z = +1.12)
  {
    const baseSlate = [];
    const copperCoils = [];
    const emeraldRing = [];

    baseSlate.push(prep(chamferedPlate(0.96, 0.11, 0.78, 0.14), trs(0, -0.22, 0)));
    baseSlate.push(prep(new THREE.TorusGeometry(0.28, 0.058, 16, 38), trs(0, 0.07, 0, Math.PI / 2, 0, 0)));
    for (let i = 0; i < 10; i++) {
      const a = (i / 10) * Math.PI * 2;
      copperCoils.push(
        prep(
          new THREE.TorusGeometry(0.074, 0.015, 8, 18),
          trs(Math.cos(a) * 0.28, 0.07, Math.sin(a) * 0.28, 0, -a, 0),
        ),
      );
    }
    emeraldRing.push(prep(new THREE.TorusGeometry(0.28, 0.018, 10, 38), trs(0, 0.135, 0, Math.PI / 2, 0, 0)));
    baseSlate.push(prep(new THREE.CylinderGeometry(0.14, 0.16, 0.34, 18), trs(-1.82, -0.06, -0.06)));

    addNode({
      name: 'Module_LocalLoop',
      translation: [1.46, 0.02, 1.18],
      primitives: [
        { geom: mergeList(baseSlate), material: MAT.Chassis_Slate },
        { geom: mergeList(copperCoils), material: MAT.Hardware_Copper },
        { geom: mergeList(emeraldRing), material: MAT.Indicator_Local },
      ],
    });
  }

  // 9. Manifold_Egress: Clean 3-Port Output Collector (X = +2.55)
  {
    const slate = [];
    const silver = [];
    const glowOfficial = [];
    const glowBridge = [];
    const glowLocal = [];

    slate.push(prep(chamferedPlate(0.42, 0.44, 2.90, 0.12), trs(-0.08, -0.04, 0)));

    const ports = [
      { z: -1.12, arr: glowOfficial },
      { z: 0.0, arr: glowBridge },
      { z: 1.12, arr: glowLocal },
    ];
    for (const p of ports) {
      silver.push(prep(new THREE.CylinderGeometry(0.22, 0.25, 0.38, 28), trs(0.12, 0.09, p.z, 0, 0, Math.PI / 2)));
      p.arr.push(prep(new THREE.TorusGeometry(0.18, 0.022, 12, 28), trs(0.32, 0.09, p.z, 0, Math.PI / 2, 0)));
    }

    addNode({
      name: 'Manifold_Egress',
      translation: [2.52, 0.02, 0],
      primitives: [
        { geom: mergeList(slate), material: MAT.Chassis_Slate },
        { geom: mergeList(silver), material: MAT.Hardware_Silver },
        { geom: mergeList(glowOfficial), material: MAT.Indicator_Official },
        { geom: mergeList(glowBridge), material: MAT.Indicator_Bridge },
        { geom: mergeList(glowLocal), material: MAT.Indicator_Local },
      ],
    });
  }

  const childSet = new Set();
  for (const n of nodes) {
    for (const c of n.children) childSet.add(c);
  }
  const topChildren = [];
  for (let i = 0; i < nodes.length; i++) {
    if (!childSet.has(i)) topChildren.push(i);
  }

  const rootIdx = addNode({
    name: 'OmniBridge_Router_Stage',
    children: topChildren,
  });

  return { nodes, rootIdx };
}

function exportToGLB({ nodes, rootIdx }) {
  const bufferViews = [];
  const accessors = [];
  const meshes = [];
  const binChunks = [];
  let byteOffset = 0;

  function pushChunk(typedArray, target) {
    const raw = Buffer.from(typedArray.buffer, typedArray.byteOffset, typedArray.byteLength);
    const pad = (4 - (raw.length % 4)) % 4;
    const viewIndex = bufferViews.length;
    bufferViews.push({
      buffer: 0,
      byteOffset,
      byteLength: raw.length,
      ...(target ? { target } : {}),
    });
    binChunks.push(raw);
    if (pad > 0) binChunks.push(Buffer.alloc(pad));
    byteOffset += raw.length + pad;
    return viewIndex;
  }

  let totalVertices = 0;
  let totalTriangles = 0;

  const gltfNodes = nodes.map((n) => {
    const outNode = { name: n.name };
    if (n.translation.some((v) => v !== 0)) outNode.translation = n.translation;
    if (n.rotation[0] !== 0 || n.rotation[1] !== 0 || n.rotation[2] !== 0 || n.rotation[3] !== 1) {
      outNode.rotation = n.rotation;
    }
    if (n.scale.some((v) => v !== 1)) outNode.scale = n.scale;
    if (n.children && n.children.length > 0) outNode.children = n.children;

    const validPrims = (n.primitives || []).filter((p) => p.geom);
    if (validPrims.length > 0) {
      const gltfPrims = [];
      for (const prim of validPrims) {
        const indexedGeom = mergeVertices(prim.geom, 1e-4);
        const posAttr = indexedGeom.getAttribute('position');
        const normAttr = indexedGeom.getAttribute('normal');
        const idxAttr = indexedGeom.getIndex();
        const count = posAttr.count;
        const indexCount = idxAttr ? idxAttr.count : count;
        totalVertices += count;
        totalTriangles += Math.floor(indexCount / 3);

        const posArr = new Float32Array(count * 3);
        const normArr = new Float32Array(count * 3);
        const min = [Infinity, Infinity, Infinity];
        const max = [-Infinity, -Infinity, -Infinity];

        for (let i = 0; i < count; i++) {
          const px = posAttr.getX(i);
          const py = posAttr.getY(i);
          const pz = posAttr.getZ(i);
          posArr[i * 3] = px;
          posArr[i * 3 + 1] = py;
          posArr[i * 3 + 2] = pz;
          if (px < min[0]) min[0] = px;
          if (py < min[1]) min[1] = py;
          if (pz < min[2]) min[2] = pz;
          if (px > max[0]) max[0] = px;
          if (py > max[1]) max[1] = py;
          if (pz > max[2]) max[2] = pz;

          const nx = normAttr.getX(i);
          const ny = normAttr.getY(i);
          const nz = normAttr.getZ(i);
          const len = Math.hypot(nx, ny, nz) || 1;
          normArr[i * 3] = nx / len;
          normArr[i * 3 + 1] = ny / len;
          normArr[i * 3 + 2] = nz / len;
        }

        const posView = pushChunk(posArr, 34962);
        const posAcc = accessors.length;
        accessors.push({
          bufferView: posView,
          componentType: 5126,
          count,
          type: 'VEC3',
          min,
          max,
        });

        const normView = pushChunk(normArr, 34962);
        const normAcc = accessors.length;
        accessors.push({
          bufferView: normView,
          componentType: 5126,
          count,
          type: 'VEC3',
        });

        let indicesAcc;
        if (idxAttr) {
          const useU32 = count > 65535;
          const idxArr = useU32 ? new Uint32Array(indexCount) : new Uint16Array(indexCount);
          for (let i = 0; i < indexCount; i++) {
            idxArr[i] = idxAttr.getX(i);
          }
          const idxView = pushChunk(idxArr, 34963);
          indicesAcc = accessors.length;
          accessors.push({
            bufferView: idxView,
            componentType: useU32 ? 5125 : 5123,
            count: indexCount,
            type: 'SCALAR',
          });
        }

        gltfPrims.push({
          attributes: {
            POSITION: posAcc,
            NORMAL: normAcc,
          },
          ...(indicesAcc !== undefined ? { indices: indicesAcc } : {}),
          material: prim.material,
          mode: 4,
        });
      }

      const meshIndex = meshes.length;
      meshes.push({
        name: `${n.name}_Mesh`,
        primitives: gltfPrims,
      });
      outNode.mesh = meshIndex;
    }

    return outNode;
  });

  const gltfMaterials = MATERIALS.map((m) => {
    const mat = {
      name: m.name,
      pbrMetallicRoughness: {
        baseColorFactor: m.baseColorFactor,
        metallicFactor: m.metallicFactor,
        roughnessFactor: m.roughnessFactor,
      },
    };
    if (m.emissiveFactor) mat.emissiveFactor = m.emissiveFactor;
    if (m.alphaMode) mat.alphaMode = m.alphaMode;
    if (m.doubleSided) mat.doubleSided = true;
    return mat;
  });

  const binBuffer = Buffer.concat(binChunks);

  const gltfJson = {
    asset: {
      version: '2.0',
      generator: 'Codex OmniBridge Router Stage GLB Synthesizer',
      copyright: 'MIT License - Codex OmniBridge',
    },
    scene: 0,
    scenes: [
      {
        name: 'OmniBridgeRouterStageScene',
        nodes: [rootIdx],
      },
    ],
    nodes: gltfNodes,
    meshes,
    materials: gltfMaterials,
    accessors,
    bufferViews,
    buffers: [
      {
        byteLength: binBuffer.length,
      },
    ],
  };

  let jsonBytes = Buffer.from(JSON.stringify(gltfJson), 'utf8');
  const jsonPad = (4 - (jsonBytes.length % 4)) % 4;
  if (jsonPad > 0) {
    jsonBytes = Buffer.concat([jsonBytes, Buffer.alloc(jsonPad, 0x20)]);
  }

  const totalLength = 12 + 8 + jsonBytes.length + 8 + binBuffer.length;
  const header = Buffer.alloc(12);
  header.writeUInt32LE(0x46546c67, 0);
  header.writeUInt32LE(2, 4);
  header.writeUInt32LE(totalLength, 8);

  const jsonChunkHeader = Buffer.alloc(8);
  jsonChunkHeader.writeUInt32LE(jsonBytes.length, 0);
  jsonChunkHeader.writeUInt32LE(0x4e4f534a, 4);

  const binChunkHeader = Buffer.alloc(8);
  binChunkHeader.writeUInt32LE(binBuffer.length, 0);
  binChunkHeader.writeUInt32LE(0x004e4942, 4);

  const glb = Buffer.concat([header, jsonChunkHeader, jsonBytes, binChunkHeader, binBuffer]);
  return { glb, totalVertices, totalTriangles, nodeCount: gltfNodes.length, meshCount: meshes.length };
}

const hierarchy = buildRouterStageHierarchy();
const { glb, totalVertices, totalTriangles, nodeCount, meshCount } = exportToGLB(hierarchy);

fs.mkdirSync(path.dirname(OUT_PATH), { recursive: true });
fs.writeFileSync(OUT_PATH, glb);

console.log(
  `[build-photonic-glb] Generated ${OUT_PATH}\n` +
    `  Size: ${(glb.length / 1024).toFixed(1)} KB | Nodes: ${nodeCount} | Meshes: ${meshCount} | Triangles: ${totalTriangles} | Vertices: ${totalVertices}`,
);
