#!/usr/bin/env node
/**
 * Parametric CAD-Grade Binary glTF 2.0 (.glb) Synthesizer
 * Target: Codex OmniBridge Photonic Switching Matrix Mk.IV
 *
 * Generates `public/models/omnibridge-photonic-matrix.glb` with named,
 * pivot-accurate sub-assemblies mapped 1-to-1 to Codex OmniBridge's Rust crates:
 *   - Chassis_Base           : Machined anodized titanium optical breadboard & heatsinks
 *   - Canopy_Port / Starboard: Retractable borosilicate quartz optical covers
 *   - Module_Ingress         : Stock Codex `/v1/responses` unified collimator
 *   - Module_Registry        : Model catalog & capability encoder wheels (crates/catalog)
 *   - Module_Crossbar        : Per-turn MEMS gimbal beam-director (crates/router)
 *   - Channel_Official_Bypass: Zero-translation official OpenAI sapphire rail
 *   - Module_ProtocolBridge  : Brewster dispersion prism stack (crates/protocol-bridge)
 *   - Module_KeychainVault   : OS Keychain Faraday vault & auth barrier (crates/credentials)
 *   - Module_LocalLoop       : Toroidal local inference resonator (127.0.0.1 loopback)
 *   - Manifold_Egress        : Tri-port output manifold (Official / Cloud Bridge / Local)
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as THREE from 'three';
import { mergeGeometries, mergeVertices } from 'three/examples/jsm/utils/BufferGeometryUtils.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_OUT = path.resolve(__dirname, '../public/models/omnibridge-photonic-matrix.glb');
const OUT_PATH = process.argv[2] ? path.resolve(process.argv[2]) : DEFAULT_OUT;

/* -------------------------------------------------------------------------- */
/*  Material Palette (PBR Metallic-Roughness + Emissive + Alpha Blend)        */
/* -------------------------------------------------------------------------- */

const MATERIALS = [
  {
    name: 'Titanium_Obsidian',
    baseColorFactor: [0.055, 0.078, 0.09, 1.0],
    metallicFactor: 0.88,
    roughnessFactor: 0.28,
  },
  {
    name: 'Titanium_Brushed_Gunmetal',
    baseColorFactor: [0.14, 0.185, 0.205, 1.0],
    metallicFactor: 0.92,
    roughnessFactor: 0.22,
  },
  {
    name: 'Titanium_Champagne_Hardware',
    baseColorFactor: [0.76, 0.66, 0.50, 1.0],
    metallicFactor: 0.95,
    roughnessFactor: 0.18,
  },
  {
    name: 'Copper_Coil_Anodized',
    baseColorFactor: [0.85, 0.46, 0.22, 1.0],
    metallicFactor: 0.94,
    roughnessFactor: 0.20,
  },
  {
    name: 'Borosilicate_Quartz_Shell',
    baseColorFactor: [0.62, 0.88, 0.96, 0.18],
    metallicFactor: 0.05,
    roughnessFactor: 0.06,
    alphaMode: 'BLEND',
    doubleSided: true,
  },
  {
    name: 'Dichroic_Prism_Crystal',
    baseColorFactor: [0.52, 0.92, 1.0, 0.46],
    metallicFactor: 0.10,
    roughnessFactor: 0.04,
    emissiveFactor: [0.02, 0.14, 0.20],
    alphaMode: 'BLEND',
    doubleSided: true,
  },
  {
    name: 'Emitter_Cyan_Primary',
    baseColorFactor: [0.0, 0.86, 0.95, 1.0],
    metallicFactor: 0.1,
    roughnessFactor: 0.15,
    emissiveFactor: [0.0, 0.86, 0.96],
  },
  {
    name: 'Emitter_Violet_Bridge',
    baseColorFactor: [0.85, 0.48, 1.0, 1.0],
    metallicFactor: 0.1,
    roughnessFactor: 0.15,
    emissiveFactor: [0.72, 0.30, 0.96],
  },
  {
    name: 'Emitter_Amber_Vault',
    baseColorFactor: [1.0, 0.72, 0.32, 1.0],
    metallicFactor: 0.1,
    roughnessFactor: 0.15,
    emissiveFactor: [0.95, 0.62, 0.16],
  },
  {
    name: 'Emitter_Emerald_Local',
    baseColorFactor: [0.30, 0.95, 0.68, 1.0],
    metallicFactor: 0.1,
    roughnessFactor: 0.15,
    emissiveFactor: [0.18, 0.86, 0.56],
  },
];

const MAT = Object.fromEntries(MATERIALS.map((m, idx) => [m.name, idx]));

/* -------------------------------------------------------------------------- */
/*  Geometry Helpers                                                          */
/* -------------------------------------------------------------------------- */

function prep(geom, transform) {
  let g = geom.index ? geom.toNonIndexed() : geom.clone();
  // Ensure clean POSITION and NORMAL only so mergeGeometries never mismatches attributes
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

function chamferedPlate(w, h, d, bevel = 0.18) {
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
  // Orient extrusion along +Y centered at Y=0
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

/* -------------------------------------------------------------------------- */
/*  Scene Graph Builder                                                       */
/* -------------------------------------------------------------------------- */

function buildPhotonicMatrixHierarchy() {
  const nodes = [];

  function addNode({ name, translation = [0, 0, 0], rotation = [0, 0, 0, 1], scale = [1, 1, 1], primitives = [], children = [] }) {
    const idx = nodes.length;
    nodes.push({ name, translation, rotation, scale, primitives, children });
    return idx;
  }

  // 1. Chassis_Base: Precision Optical Breadboard + Heatsinks + Rails + Standoffs
  {
    const darkParts = [];
    const gunmetalParts = [];
    const goldParts = [];
    const cyanParts = [];

    // Main optical baseplate & recessed sub-frame
    darkParts.push(prep(chamferedPlate(6.6, 0.22, 4.1, 0.36), trs(0, -0.52, 0)));
    gunmetalParts.push(prep(chamferedPlate(6.28, 0.08, 3.78, 0.28), trs(0, -0.36, 0)));
    darkParts.push(prep(chamferedPlate(5.86, 0.05, 3.36, 0.22), trs(0, -0.30, 0)));

    // 22 underside heatsink fins
    for (let i = -10; i <= 10; i++) {
      const x = i * 0.27;
      darkParts.push(prep(new THREE.BoxGeometry(0.055, 0.22, 3.64), trs(x, -0.72, 0)));
    }

    // 4 shock-absorbing corner feet + gold collar rings
    for (const sx of [-2.75, 2.75]) {
      for (const sz of [-1.62, 1.62]) {
        gunmetalParts.push(prep(new THREE.CylinderGeometry(0.22, 0.26, 0.14, 24), trs(sx, -0.84, sz)));
        goldParts.push(prep(new THREE.TorusGeometry(0.23, 0.018, 10, 24), trs(sx, -0.78, sz, Math.PI / 2, 0, 0)));
      }
    }

    // Precision longitudinal optical bench guide rails (along X)
    for (const z of [-1.52, -1.08, 0.0, 1.08, 1.58]) {
      const railW = Math.abs(z) > 1.4 ? 5.9 : 5.2;
      const railH = Math.abs(z) > 1.4 ? 0.07 : 0.035;
      gunmetalParts.push(prep(new THREE.BoxGeometry(railW, railH, 0.07), trs(0, -0.26, z)));
    }

    // Perimeter M3 socket cap bolts & corner pillars
    for (const sx of [-2.96, -1.48, 0, 1.48, 2.96]) {
      for (const sz of [-1.76, 1.76]) {
        goldParts.push(prep(new THREE.CylinderGeometry(0.042, 0.042, 0.05, 6), trs(sx, -0.30, sz)));
      }
    }
    for (const [cx, cz] of [[-2.92, -1.72], [2.92, -1.72], [-2.92, 1.72], [2.92, 1.72]]) {
      gunmetalParts.push(prep(new THREE.CylinderGeometry(0.08, 0.09, 1.06, 16), trs(cx, 0.18, cz)));
      goldParts.push(prep(new THREE.CylinderGeometry(0.105, 0.105, 0.06, 6), trs(cx, 0.72, cz)));
    }

    // Edge status light strips embedded in optical bench
    cyanParts.push(prep(new THREE.BoxGeometry(5.4, 0.018, 0.022), trs(0, -0.31, -1.89)));
    cyanParts.push(prep(new THREE.BoxGeometry(5.4, 0.018, 0.022), trs(0, -0.31, 1.89)));

    addNode({
      name: 'Chassis_Base',
      primitives: [
        { geom: mergeList(darkParts), material: MAT.Titanium_Obsidian },
        { geom: mergeList(gunmetalParts), material: MAT.Titanium_Brushed_Gunmetal },
        { geom: mergeList(goldParts), material: MAT.Titanium_Champagne_Hardware },
        { geom: mergeList(cyanParts), material: MAT.Emitter_Cyan_Primary },
      ],
    });
  }

  // 2. Canopy_Port (-Z half) & Canopy_Starboard (+Z half): Split Retractable Quartz Enclosure
  for (const side of [-1, 1]) {
    const glassParts = [];
    const frameParts = [];
    const latchParts = [];

    const zCenter = side * 0.92;
    // Top quartz plate (half-width)
    glassParts.push(prep(chamferedPlate(5.96, 0.045, 1.78, 0.18), trs(0, 0.76, zCenter)));
    // Angled side quartz wall
    glassParts.push(prep(new THREE.BoxGeometry(5.84, 0.92, 0.038), trs(0, 0.26, side * 1.80)));

    // Tension ribs & frame rails
    frameParts.push(prep(new THREE.BoxGeometry(6.02, 0.055, 0.08), trs(0, 0.76, side * 0.05)));
    frameParts.push(prep(new THREE.BoxGeometry(6.02, 0.065, 0.09), trs(0, 0.75, side * 1.78)));
    for (const rx of [-2.2, -0.75, 0.75, 2.2]) {
      frameParts.push(prep(new THREE.BoxGeometry(0.06, 0.055, 1.76), trs(rx, 0.77, zCenter)));
      latchParts.push(prep(new THREE.BoxGeometry(0.11, 0.08, 0.06), trs(rx, 0.76, side * 1.82)));
    }

    addNode({
      name: side < 0 ? 'Canopy_Port' : 'Canopy_Starboard',
      primitives: [
        { geom: mergeList(frameParts), material: MAT.Titanium_Brushed_Gunmetal },
        { geom: mergeList(latchParts), material: MAT.Titanium_Champagne_Hardware },
        { geom: mergeList(glassParts), material: MAT.Borosilicate_Quartz_Shell },
      ],
    });
  }

  // 3. Module_Ingress: Stock Codex `/v1/responses` Optical Collimator (X = -2.62, Y = 0.08, Z = 0)
  {
    const body = [];
    const accents = [];
    const lens = [];
    const glow = [];

    // Multi-stepped collimator barrel along X axis (Cylinder default is Y, so rotateZ = PI/2)
    body.push(prep(new THREE.BoxGeometry(0.46, 0.68, 0.82), trs(-0.18, -0.10, 0)));
    body.push(prep(new THREE.CylinderGeometry(0.34, 0.38, 0.56, 32), trs(0.08, 0.04, 0, 0, 0, Math.PI / 2)));
    body.push(prep(new THREE.CylinderGeometry(0.26, 0.30, 0.36, 32), trs(0.44, 0.04, 0, 0, 0, Math.PI / 2)));

    // Knurled focus collar & SMA coax sync connectors
    for (let k = 0; k < 16; k++) {
      const a = (k / 16) * Math.PI * 2;
      accents.push(
        prep(
          new THREE.BoxGeometry(0.24, 0.025, 0.025),
          trs(0.12, 0.04 + Math.cos(a) * 0.355, Math.sin(a) * 0.355, a, 0, 0),
        ),
      );
    }
    accents.push(prep(new THREE.CylinderGeometry(0.07, 0.07, 0.24, 16), trs(-0.48, 0.16, -0.24, 0, 0, Math.PI / 2)));
    accents.push(prep(new THREE.CylinderGeometry(0.07, 0.07, 0.24, 16), trs(-0.48, 0.16, 0.24, 0, 0, Math.PI / 2)));

    // Objective crystal & cyan emitter ring
    lens.push(prep(new THREE.CylinderGeometry(0.21, 0.21, 0.08, 28), trs(0.61, 0.04, 0, 0, 0, Math.PI / 2)));
    glow.push(prep(new THREE.TorusGeometry(0.25, 0.022, 12, 32), trs(0.62, 0.04, 0, 0, Math.PI / 2, 0)));
    glow.push(prep(new THREE.TorusGeometry(0.32, 0.018, 12, 32), trs(-0.44, 0.04, 0, 0, Math.PI / 2, 0)));

    addNode({
      name: 'Module_Ingress',
      translation: [-2.62, 0.06, 0],
      primitives: [
        { geom: mergeList(body), material: MAT.Titanium_Brushed_Gunmetal },
        { geom: mergeList(accents), material: MAT.Titanium_Champagne_Hardware },
        { geom: mergeList(lens), material: MAT.Dichroic_Prism_Crystal },
        { geom: mergeList(glow), material: MAT.Emitter_Cyan_Primary },
      ],
    });
  }

  // 4. Module_Registry: Model Catalog & Capability Encoder Wheels (`crates/catalog`)
  {
    // Child Wheel A (outer encoder ring rotating around X axis)
    const wheelAGeom = [];
    const wheelAGlow = [];
    wheelAGeom.push(prep(hollowRing(0.56, 0.43, 0.07, 44), trs(0, 0, 0, 0, Math.PI / 2, 0)));
    for (let i = 0; i < 12; i++) {
      const a = (i / 12) * Math.PI * 2;
      wheelAGeom.push(
        prep(
          new THREE.BoxGeometry(0.09, 0.05, 0.035),
          trs(0, Math.cos(a) * 0.58, Math.sin(a) * 0.58, a, 0, 0),
        ),
      );
    }
    wheelAGlow.push(prep(new THREE.TorusGeometry(0.49, 0.014, 10, 40), trs(0.042, 0, 0, 0, Math.PI / 2, 0)));

    const wheelAIdx = addNode({
      name: 'Registry_Wheel_Outer',
      translation: [-0.08, 0.08, 0],
      primitives: [
        { geom: mergeList(wheelAGeom), material: MAT.Titanium_Champagne_Hardware },
        { geom: mergeList(wheelAGlow), material: MAT.Emitter_Cyan_Primary },
      ],
    });

    // Child Wheel B (inner counter-rotating sapphire optical filter disk)
    const wheelBFrame = [];
    const wheelBCrystal = [];
    wheelBFrame.push(prep(hollowRing(0.39, 0.25, 0.06, 36), trs(0, 0, 0, 0, Math.PI / 2, 0)));
    for (let i = 0; i < 6; i++) {
      const a = (i / 6) * Math.PI * 2;
      wheelBCrystal.push(
        prep(
          new THREE.CylinderGeometry(0.065, 0.065, 0.05, 18),
          trs(0, Math.cos(a) * 0.32, Math.sin(a) * 0.32, 0, 0, Math.PI / 2),
        ),
      );
    }
    wheelBCrystal.push(prep(new THREE.CylinderGeometry(0.21, 0.21, 0.05, 24), trs(0, 0, 0, 0, 0, Math.PI / 2)));

    const wheelBIdx = addNode({
      name: 'Registry_Wheel_Inner',
      translation: [0.09, 0.08, 0],
      primitives: [
        { geom: mergeList(wheelBFrame), material: MAT.Titanium_Brushed_Gunmetal },
        { geom: mergeList(wheelBCrystal), material: MAT.Dichroic_Prism_Crystal },
      ],
    });

    // Static mount cradle for Registry
    const mountParts = [];
    mountParts.push(prep(new THREE.BoxGeometry(0.44, 0.16, 1.32), trs(0, -0.26, 0)));
    mountParts.push(prep(new THREE.BoxGeometry(0.34, 0.72, 0.10), trs(0, 0.04, -0.66)));
    mountParts.push(prep(new THREE.BoxGeometry(0.34, 0.72, 0.10), trs(0, 0.04, 0.66)));

    addNode({
      name: 'Module_Registry',
      translation: [-1.52, 0.02, 0],
      primitives: [{ geom: mergeList(mountParts), material: MAT.Titanium_Obsidian }],
      children: [wheelAIdx, wheelBIdx],
    });
  }

  // 5. Module_Crossbar: Per-Turn MEMS Optical Gimbal & Beam Director (`crates/router`)
  {
    // Innermost mirror & dichroic beam-director cube (Tilts on Pitch / Z)
    const coreMetal = [];
    const coreCrystal = [];
    const coreGlow = [];
    coreMetal.push(prep(hollowRing(0.34, 0.27, 0.06, 32), trs(0, 0, 0)));
    // Octagonal precision beam-splitter prism
    coreCrystal.push(prep(new THREE.CylinderGeometry(0.23, 0.23, 0.32, 8), trs(0, 0, 0)));
    coreCrystal.push(prep(new THREE.BoxGeometry(0.30, 0.30, 0.035), trs(0, 0, 0, 0, Math.PI / 4, 0)));
    coreGlow.push(prep(new THREE.TorusGeometry(0.24, 0.015, 10, 32), trs(0, 0.17, 0, Math.PI / 2, 0, 0)));
    coreGlow.push(prep(new THREE.TorusGeometry(0.24, 0.015, 10, 32), trs(0, -0.17, 0, Math.PI / 2, 0, 0)));

    const pitchIdx = addNode({
      name: 'Crossbar_Gimbal_Pitch',
      translation: [0, 0, 0],
      primitives: [
        { geom: mergeList(coreMetal), material: MAT.Titanium_Champagne_Hardware },
        { geom: mergeList(coreCrystal), material: MAT.Dichroic_Prism_Crystal },
        { geom: mergeList(coreGlow), material: MAT.Emitter_Cyan_Primary },
      ],
    });

    // Outer Yaw Gimbal Ring (Rotates around Y to steer beam between -Z Official, 0 Bridge, +Z Local)
    const yawMetal = [];
    yawMetal.push(prep(hollowRing(0.48, 0.40, 0.075, 40), trs(0, 0, 0, Math.PI / 2, 0, 0)));
    yawMetal.push(prep(new THREE.CylinderGeometry(0.03, 0.03, 0.98, 12), trs(0, 0, 0, Math.PI / 2, 0, 0)));

    const yawIdx = addNode({
      name: 'Crossbar_Gimbal_Yaw',
      translation: [0, 0.12, 0],
      primitives: [{ geom: mergeList(yawMetal), material: MAT.Titanium_Brushed_Gunmetal }],
      children: [pitchIdx],
    });

    // Crossbar Pedestal & 4 Corner MEMS Coil Actuators
    const baseParts = [];
    const copperParts = [];
    baseParts.push(prep(chamferedPlate(1.08, 0.14, 1.18, 0.16), trs(0, -0.26, 0)));
    baseParts.push(prep(new THREE.CylinderGeometry(0.54, 0.60, 0.10, 32), trs(0, -0.16, 0)));
    // Overhead bridge arch holding yaw spindle
    baseParts.push(prep(new THREE.BoxGeometry(0.14, 0.68, 0.12), trs(0, 0.14, -0.54)));
    baseParts.push(prep(new THREE.BoxGeometry(0.14, 0.68, 0.12), trs(0, 0.14, 0.54)));
    baseParts.push(prep(new THREE.BoxGeometry(0.16, 0.08, 1.20), trs(0, 0.50, 0)));

    for (const [ax, az] of [[-0.40, -0.42], [0.40, -0.42], [-0.40, 0.42], [0.40, 0.42]]) {
      copperParts.push(prep(new THREE.CylinderGeometry(0.085, 0.085, 0.22, 20), trs(ax, -0.06, az)));
    }

    addNode({
      name: 'Module_Crossbar',
      translation: [-0.36, 0.02, 0],
      primitives: [
        { geom: mergeList(baseParts), material: MAT.Titanium_Obsidian },
        { geom: mergeList(copperParts), material: MAT.Copper_Coil_Anodized },
      ],
      children: [yawIdx],
    });
  }

  // 6. Channel_Official_Bypass: Zero-Translation Sapphire Vacuum Waveguide (Z = -1.12)
  {
    const posts = [];
    const sapphireTube = [];
    const cyanRings = [];

    // Runs cleanly from X = -0.05 to X = +2.25 along Z = -1.12
    sapphireTube.push(
      prep(new THREE.CylinderGeometry(0.095, 0.095, 2.28, 24), trs(1.10, 0.14, -1.12, 0, 0, Math.PI / 2)),
    );
    // Turning mirror relay at (-0.36, 0.14, -1.12) catching the -Z deflected beam from Crossbar!
    posts.push(prep(new THREE.CylinderGeometry(0.16, 0.19, 0.42, 20), trs(-0.36, -0.08, -1.12)));
    sapphireTube.push(
      prep(new THREE.BoxGeometry(0.28, 0.26, 0.045), trs(-0.36, 0.14, -1.12, 0, Math.PI / 4, 0)),
    );

    for (const px of [0.25, 1.10, 1.95]) {
      posts.push(prep(new THREE.BoxGeometry(0.12, 0.36, 0.26), trs(px, -0.12, -1.12)));
      cyanRings.push(prep(new THREE.TorusGeometry(0.125, 0.018, 10, 24), trs(px, 0.14, -1.12, 0, Math.PI / 2, 0)));
    }

    addNode({
      name: 'Channel_Official_Bypass',
      primitives: [
        { geom: mergeList(posts), material: MAT.Titanium_Brushed_Gunmetal },
        { geom: mergeList(sapphireTube), material: MAT.Dichroic_Prism_Crystal },
        { geom: mergeList(cyanRings), material: MAT.Emitter_Cyan_Primary },
      ],
    });
  }

  // 7. Module_ProtocolBridge: 3-Element Brewster Dispersion Prism Stack (`crates/protocol-bridge`)
  {
    const stageMetal = [];
    const goldScrews = [];
    const prisms = [];
    const bridgeGlow = [];

    stageMetal.push(prep(chamferedPlate(1.68, 0.12, 0.96, 0.14), trs(0, -0.26, 0)));

    // 3 Equilateral triangular Brewster prisms on micro-stages
    const prismConfigs = [
      { x: -0.50, z: -0.08, ry: 0.22 },
      { x: 0.0, z: 0.09, ry: -0.26 + Math.PI },
      { x: 0.50, z: -0.06, ry: 0.18 },
    ];
    for (const p of prismConfigs) {
      stageMetal.push(prep(new THREE.CylinderGeometry(0.23, 0.26, 0.10, 24), trs(p.x, -0.16, p.z)));
      goldScrews.push(prep(new THREE.CylinderGeometry(0.032, 0.032, 0.16, 12), trs(p.x, -0.15, p.z + 0.29, Math.PI / 2, 0, 0)));
      // 3-sided optical dispersion prism
      prisms.push(prep(new THREE.CylinderGeometry(0.24, 0.24, 0.44, 3), trs(p.x, 0.11, p.z, 0, p.ry, 0)));
      bridgeGlow.push(prep(new THREE.TorusGeometry(0.22, 0.013, 8, 24), trs(p.x, -0.10, p.z, Math.PI / 2, 0, 0)));
    }

    // Waveplate canonicalizer gate at exit of bridge
    stageMetal.push(prep(hollowRing(0.24, 0.17, 0.05, 28), trs(0.80, 0.12, 0, 0, Math.PI / 2, 0)));
    prisms.push(prep(new THREE.CylinderGeometry(0.17, 0.17, 0.03, 24), trs(0.80, 0.12, 0, 0, 0, Math.PI / 2)));

    addNode({
      name: 'Module_ProtocolBridge',
      translation: [1.08, 0.02, 0],
      primitives: [
        { geom: mergeList(stageMetal), material: MAT.Titanium_Brushed_Gunmetal },
        { geom: mergeList(goldScrews), material: MAT.Titanium_Champagne_Hardware },
        { geom: mergeList(prisms), material: MAT.Dichroic_Prism_Crystal },
        { geom: mergeList(bridgeGlow), material: MAT.Emitter_Violet_Bridge },
      ],
    });
  }

  // 8. Module_KeychainVault & Auth_Isolation_Barrier (`crates/credentials`)
  {
    // Rotating 6-lug Vault Breach Dial
    const rotorMetal = [];
    const rotorGlow = [];
    rotorMetal.push(prep(hollowRing(0.32, 0.12, 0.06, 30), trs(0, 0, 0, Math.PI / 2, 0, 0)));
    for (let i = 0; i < 6; i++) {
      const a = (i / 6) * Math.PI * 2;
      rotorMetal.push(
        prep(
          new THREE.BoxGeometry(0.065, 0.055, 0.14),
          trs(Math.cos(a) * 0.24, 0.02, Math.sin(a) * 0.24, 0, -a, 0),
        ),
      );
    }
    rotorGlow.push(prep(new THREE.TorusGeometry(0.19, 0.016, 10, 28), trs(0, 0.035, 0, Math.PI / 2, 0, 0)));

    const vaultRotorIdx = addNode({
      name: 'Vault_LockDial',
      translation: [-0.36, 0.29, 0],
      primitives: [
        { geom: mergeList(rotorMetal), material: MAT.Titanium_Champagne_Hardware },
        { geom: mergeList(rotorGlow), material: MAT.Emitter_Amber_Vault },
      ],
    });

    // Armored Faraday Cryo-Vault Body + Titanium Auth Isolation Bulkhead
    const vaultBody = [];
    const vaultRibs = [];
    const vaultGlow = [];

    vaultBody.push(prep(chamferedPlate(1.06, 0.14, 0.78, 0.12), trs(-0.36, -0.26, 0)));
    vaultBody.push(prep(new THREE.CylinderGeometry(0.38, 0.40, 0.46, 32), trs(-0.36, 0.02, 0)));
    for (let r = -2; r <= 2; r++) {
      vaultRibs.push(prep(new THREE.TorusGeometry(0.40, 0.018, 10, 32), trs(-0.36, 0.02 + r * 0.08, 0, Math.PI / 2, 0, 0)));
    }
    // Physical Air-Gap Bulkhead between Official OAuth lane (Z = -1.12) and Third-Party lane (Z = 0)
    // Placed in world around Z = -0.62 -> relative to Vault (Z = +1.34) is Z = -1.96
    vaultBody.push(prep(new THREE.BoxGeometry(2.65, 0.46, 0.055), trs(0.48, -0.04, -1.96)));
    vaultRibs.push(prep(new THREE.BoxGeometry(2.68, 0.04, 0.085), trs(0.48, 0.20, -1.96)));
    vaultGlow.push(prep(new THREE.BoxGeometry(2.56, 0.018, 0.068), trs(0.48, 0.14, -1.96)));

    addNode({
      name: 'Module_KeychainVault',
      translation: [0.22, 0.02, 1.34],
      primitives: [
        { geom: mergeList(vaultBody), material: MAT.Titanium_Obsidian },
        { geom: mergeList(vaultRibs), material: MAT.Titanium_Brushed_Gunmetal },
        { geom: mergeList(vaultGlow), material: MAT.Emitter_Amber_Vault },
      ],
      children: [vaultRotorIdx],
    });
  }

  // 9. Module_LocalLoop: Toroidal Loopback Resonator (`127.0.0.1` Local Inference)
  {
    const loopMount = [];
    const copperCoils = [];
    const emeraldGlow = [];
    const crystalCore = [];

    loopMount.push(prep(chamferedPlate(1.08, 0.12, 0.84, 0.14), trs(0, -0.26, 0)));
    // Toroidal waveguide ring
    crystalCore.push(prep(new THREE.TorusGeometry(0.32, 0.068, 18, 40), trs(0, 0.08, 0, Math.PI / 2, 0, 0)));
    // 12 copper toroidal windings around the resonator
    for (let i = 0; i < 12; i++) {
      const a = (i / 12) * Math.PI * 2;
      copperCoils.push(
        prep(
          new THREE.TorusGeometry(0.082, 0.016, 8, 18),
          trs(Math.cos(a) * 0.32, 0.08, Math.sin(a) * 0.32, 0, -a, 0),
        ),
      );
    }
    emeraldGlow.push(prep(new THREE.TorusGeometry(0.32, 0.020, 10, 40), trs(0, 0.15, 0, Math.PI / 2, 0, 0)));

    // Relay mirror at Crossbar Z = +1.12 lane
    loopMount.push(prep(new THREE.CylinderGeometry(0.15, 0.18, 0.38, 18), trs(-1.82, -0.09, -0.14)));
    crystalCore.push(prep(new THREE.BoxGeometry(0.26, 0.24, 0.04), trs(-1.82, 0.12, -0.14, 0, -Math.PI / 4, 0)));

    addNode({
      name: 'Module_LocalLoop',
      translation: [1.46, 0.02, 1.26],
      primitives: [
        { geom: mergeList(loopMount), material: MAT.Titanium_Brushed_Gunmetal },
        { geom: mergeList(copperCoils), material: MAT.Copper_Coil_Anodized },
        { geom: mergeList(crystalCore), material: MAT.Dichroic_Prism_Crystal },
        { geom: mergeList(emeraldGlow), material: MAT.Emitter_Emerald_Local },
      ],
    });
  }

  // 10. Manifold_Egress: Tri-Channel Output Optical Terminators (X = +2.66)
  {
    const body = [];
    const hardware = [];
    const lenses = [];
    const portOfficialGlow = [];
    const portBridgeGlow = [];
    const portLocalGlow = [];

    // Manifold spine
    body.push(prep(new THREE.BoxGeometry(0.38, 0.64, 3.06), trs(-0.12, -0.04, 0)));

    const ports = [
      { z: -1.12, glowArr: portOfficialGlow },
      { z: 0.0, glowArr: portBridgeGlow },
      { z: 1.12, glowArr: portLocalGlow },
    ];
    for (const p of ports) {
      body.push(prep(new THREE.CylinderGeometry(0.26, 0.30, 0.44, 28), trs(0.14, 0.12, p.z, 0, 0, Math.PI / 2)));
      hardware.push(prep(new THREE.CylinderGeometry(0.21, 0.23, 0.22, 28), trs(0.42, 0.12, p.z, 0, 0, Math.PI / 2)));
      lenses.push(prep(new THREE.CylinderGeometry(0.16, 0.16, 0.06, 24), trs(0.52, 0.12, p.z, 0, 0, Math.PI / 2)));
      p.glowArr.push(prep(new THREE.TorusGeometry(0.19, 0.020, 12, 28), trs(0.53, 0.12, p.z, 0, Math.PI / 2, 0)));
    }

    addNode({
      name: 'Manifold_Egress',
      translation: [2.64, 0.02, 0],
      primitives: [
        { geom: mergeList(body), material: MAT.Titanium_Brushed_Gunmetal },
        { geom: mergeList(hardware), material: MAT.Titanium_Champagne_Hardware },
        { geom: mergeList(lenses), material: MAT.Dichroic_Prism_Crystal },
        { geom: mergeList(portOfficialGlow), material: MAT.Emitter_Cyan_Primary },
        { geom: mergeList(portBridgeGlow), material: MAT.Emitter_Violet_Bridge },
        { geom: mergeList(portLocalGlow), material: MAT.Emitter_Emerald_Local },
      ],
    });
  }

  // Root node collecting top-level assemblies (those not already referenced as children)
  const childSet = new Set();
  for (const n of nodes) {
    for (const c of n.children) childSet.add(c);
  }
  const topChildren = [];
  for (let i = 0; i < nodes.length; i++) {
    if (!childSet.has(i)) topChildren.push(i);
  }

  const rootIdx = addNode({
    name: 'OmniBridge_Photonic_Matrix_MkIV',
    children: topChildren,
  });

  return { nodes, rootIdx };
}

/* -------------------------------------------------------------------------- */
/*  Spec-Compliant Binary glTF 2.0 (.glb) Writer                              */
/* -------------------------------------------------------------------------- */

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

          let nx = normAttr.getX(i);
          let ny = normAttr.getY(i);
          let nz = normAttr.getZ(i);
          const len = Math.hypot(nx, ny, nz) || 1;
          normArr[i * 3] = nx / len;
          normArr[i * 3 + 1] = ny / len;
          normArr[i * 3 + 2] = nz / len;
        }

        const posView = pushChunk(posArr, 34962); // ARRAY_BUFFER
        const posAcc = accessors.length;
        accessors.push({
          bufferView: posView,
          componentType: 5126, // FLOAT
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
          const idxView = pushChunk(idxArr, 34963); // ELEMENT_ARRAY_BUFFER
          indicesAcc = accessors.length;
          accessors.push({
            bufferView: idxView,
            componentType: useU32 ? 5125 : 5123, // UNSIGNED_INT : UNSIGNED_SHORT
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
          mode: 4, // TRIANGLES
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
      generator: 'Codex OmniBridge Parametric CAD GLB Synthesizer',
      copyright: 'MIT License - Codex OmniBridge',
    },
    scene: 0,
    scenes: [
      {
        name: 'PhotonicSwitchingMatrixScene',
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
    jsonBytes = Buffer.concat([jsonBytes, Buffer.alloc(jsonPad, 0x20)]); // pad JSON with spaces
  }

  const totalLength = 12 + 8 + jsonBytes.length + 8 + binBuffer.length;
  const header = Buffer.alloc(12);
  header.writeUInt32LE(0x46546c67, 0); // 'glTF'
  header.writeUInt32LE(2, 4); // version 2
  header.writeUInt32LE(totalLength, 8);

  const jsonChunkHeader = Buffer.alloc(8);
  jsonChunkHeader.writeUInt32LE(jsonBytes.length, 0);
  jsonChunkHeader.writeUInt32LE(0x4e4f534a, 4); // 'JSON'

  const binChunkHeader = Buffer.alloc(8);
  binChunkHeader.writeUInt32LE(binBuffer.length, 0);
  binChunkHeader.writeUInt32LE(0x004e4942, 4); // 'BIN\0'

  const glb = Buffer.concat([header, jsonChunkHeader, jsonBytes, binChunkHeader, binBuffer]);
  return { glb, totalVertices, totalTriangles, nodeCount: gltfNodes.length, meshCount: meshes.length };
}

const hierarchy = buildPhotonicMatrixHierarchy();
const { glb, totalVertices, totalTriangles, nodeCount, meshCount } = exportToGLB(hierarchy);

fs.mkdirSync(path.dirname(OUT_PATH), { recursive: true });
fs.writeFileSync(OUT_PATH, glb);

console.log(
  `[build-photonic-glb] Generated ${OUT_PATH}\n` +
    `  Size: ${(glb.length / 1024).toFixed(1)} KB | Nodes: ${nodeCount} | Meshes: ${meshCount} | Triangles: ${totalTriangles} | Vertices: ${totalVertices}`,
);
