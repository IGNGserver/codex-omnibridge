import * as THREE from 'three';
import type { ModelRoutePreset, RoutePathKind } from '../data/productData';

interface SignalLane {
  key: string;
  curve: THREE.CatmullRomCurve3;
  tubeMesh: THREE.Mesh<THREE.TubeGeometry, THREE.MeshStandardMaterial>;
  baseColor: THREE.Color;
  targetActive: number;
  currentActive: number;
}

const PACKET_COUNT = 16;

/**
 * Renders the 3D routing signal tracks and sliding data packets.
 * Uses crisp opaque PBR tubes (no additive fog overlap!) so every lane reads
 * clearly on both Light (#eaf6f8) and Dark (#111c1e) M3 surfaces.
 */
export class OpticalWaveguideSystem {
  public readonly group = new THREE.Group();
  private readonly lanes: Record<string, SignalLane> = {};
  private readonly packetInstanced: THREE.InstancedMesh<THREE.SphereGeometry, THREE.MeshStandardMaterial>;
  private readonly packetOffsets: Float32Array;
  private readonly packetSpeeds: Float32Array;
  private readonly dummy = new THREE.Object3D();
  private readonly tmpPos = new THREE.Vector3();

  private activeKind: RoutePathKind = 'official';
  private activeAccent = new THREE.Color('#00a6b8');
  private standbyColor = new THREE.Color('#9db3b7');
  private standbyEmissive = new THREE.Color('#000000');

  constructor() {
    this.group.name = 'Router_Signal_Tracks';

    // 1. Unified Ingress Track (Left input -> Catalog -> Central Crossbar)
    this.createLane(
      'ingress',
      [
        new THREE.Vector3(-3.05, 0.11, 0.0),
        new THREE.Vector3(-2.45, 0.11, 0.0),
        new THREE.Vector3(-1.48, 0.11, 0.0),
        new THREE.Vector3(-0.36, 0.12, 0.0),
      ],
      0.030,
      '#008899',
    );

    // 2. Upper Lane: Official OpenAI Direct Bypass (Z = -1.12)
    this.createLane(
      'official',
      [
        new THREE.Vector3(-0.36, 0.12, 0.0),
        new THREE.Vector3(-0.36, 0.12, -1.12),
        new THREE.Vector3(0.65, 0.13, -1.12),
        new THREE.Vector3(1.85, 0.13, -1.12),
        new THREE.Vector3(2.85, 0.11, -1.12),
      ],
      0.028,
      '#008899',
    );

    // 3. Middle Lane: Protocol Translator Bay (Z = 0.0)
    this.createLane(
      'bridge',
      [
        new THREE.Vector3(-0.36, 0.12, 0.0),
        new THREE.Vector3(0.55, 0.12, 0.0),
        new THREE.Vector3(1.06, 0.12, 0.0),
        new THREE.Vector3(1.62, 0.12, 0.0),
        new THREE.Vector3(2.85, 0.11, 0.0),
      ],
      0.029,
      '#9c56a6',
    );

    // 4. Keychain Credential Injection Conduit (Vault -> Middle Protocol Bridge)
    this.createLane(
      'vault',
      [
        new THREE.Vector3(-0.02, 0.25, 1.24),
        new THREE.Vector3(0.24, 0.19, 0.84),
        new THREE.Vector3(0.48, 0.13, 0.34),
        new THREE.Vector3(0.64, 0.12, 0.04),
      ],
      0.019,
      '#c78008',
    );

    // 5. Lower Lane: Local Offline Loopback (Z = +1.12 -> Loop Ring -> Egress)
    this.createLane(
      'local',
      [
        new THREE.Vector3(-0.36, 0.12, 0.0),
        new THREE.Vector3(-0.36, 0.12, 1.12),
        new THREE.Vector3(1.18, 0.12, 1.18),
        new THREE.Vector3(1.46, 0.12, 1.46),
        new THREE.Vector3(1.74, 0.12, 1.18),
        new THREE.Vector3(1.46, 0.12, 0.90),
        new THREE.Vector3(2.15, 0.12, 1.12),
        new THREE.Vector3(2.85, 0.11, 1.12),
      ],
      0.027,
      '#35883f',
    );

    // Crisp opaque signal beads traveling smoothly along the active path
    const packetGeom = new THREE.SphereGeometry(0.056, 14, 14);
    const packetMat = new THREE.MeshStandardMaterial({
      color: new THREE.Color('#00a6b8'),
      emissive: new THREE.Color('#006874'),
      emissiveIntensity: 0.65,
      roughness: 0.22,
      metalness: 0.10,
    });
    this.packetInstanced = new THREE.InstancedMesh(packetGeom, packetMat, PACKET_COUNT);
    this.packetInstanced.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
    this.packetOffsets = new Float32Array(PACKET_COUNT);
    this.packetSpeeds = new Float32Array(PACKET_COUNT);
    for (let i = 0; i < PACKET_COUNT; i++) {
      this.packetOffsets[i] = i / PACKET_COUNT;
      this.packetSpeeds[i] = 0.24 + (i % 4) * 0.035;
    }
    this.group.add(this.packetInstanced);

    this.lanes.ingress.currentActive = 1;
    this.lanes.ingress.targetActive = 1;
    this.lanes.official.currentActive = 1;
    this.lanes.official.targetActive = 1;
  }

  private createLane(key: string, points: THREE.Vector3[], radius: number, hex: string) {
    const curve = new THREE.CatmullRomCurve3(points, false, 'catmullrom', 0.22);
    const geom = new THREE.TubeGeometry(curve, 84, radius, 12, false);
    const mat = new THREE.MeshStandardMaterial({
      color: new THREE.Color(hex),
      emissive: new THREE.Color(hex),
      emissiveIntensity: 0.25,
      roughness: 0.34,
      metalness: 0.18,
    });
    const tubeMesh = new THREE.Mesh(geom, mat);
    this.group.add(tubeMesh);

    this.lanes[key] = {
      key,
      curve,
      tubeMesh,
      baseColor: new THREE.Color(hex),
      targetActive: 0,
      currentActive: 0,
    };
  }

  public setTheme(theme: 'light' | 'dark') {
    if (theme === 'light') {
      this.standbyColor.set('#9ab0b4');
    } else {
      this.standbyColor.set('#2c3f43');
    }
    const packetMat = this.packetInstanced.material;
    packetMat.emissiveIntensity = theme === 'dark' ? 0.95 : 0.45;
  }

  public applyPreset(preset: ModelRoutePreset) {
    this.activeKind = preset.routeKind;
    this.activeAccent.set(preset.waveHex);

    this.lanes.ingress.targetActive = 1.0;
    this.lanes.official.targetActive = preset.routeKind === 'official' ? 1.0 : 0.0;
    this.lanes.bridge.targetActive = preset.routeKind === 'bridge' ? 1.0 : 0.0;
    this.lanes.vault.targetActive = preset.vaultActive ? 1.0 : 0.0;
    this.lanes.local.targetActive = preset.routeKind === 'local' ? 1.0 : 0.0;

    this.lanes.ingress.baseColor.set(preset.waveHex);
    if (preset.routeKind === 'bridge') {
      this.lanes.bridge.baseColor.set(preset.waveHex);
    }

    const packetMat = this.packetInstanced.material;
    packetMat.color.set(preset.waveHex);
    packetMat.emissive.set(preset.waveHex);
  }

  public triggerSyncPulse() {
    // Brief speed/scale emphasis handled smoothly by update loop
    for (let i = 0; i < PACKET_COUNT; i++) {
      this.packetOffsets[i] = (this.packetOffsets[i] + 0.05) % 1.0;
    }
  }

  public update(elapsed: number, dt: number, reducedMotion: boolean) {
    const lerpRate = reducedMotion ? 1.0 : Math.min(1.0, dt * 8.0);

    for (const lane of Object.values(this.lanes)) {
      lane.currentActive += (lane.targetActive - lane.currentActive) * lerpRate;
      const mat = lane.tubeMesh.material;
      mat.color.copy(this.standbyColor).lerp(lane.baseColor, lane.currentActive);
      mat.emissive.copy(this.standbyEmissive).lerp(lane.baseColor, lane.currentActive);
      mat.emissiveIntensity = lane.currentActive * 0.42;
      const radialScale = 0.72 + lane.currentActive * 0.42;
      lane.tubeMesh.scale.set(1, radialScale, radialScale);
    }

    // Update animated data packet beads along ingress + active output track
    const ingressCurve = this.lanes.ingress.curve;
    const egressCurve = this.lanes[this.activeKind].curve;

    for (let i = 0; i < PACKET_COUNT; i++) {
      const t = reducedMotion
        ? this.packetOffsets[i]
        : (this.packetOffsets[i] + elapsed * this.packetSpeeds[i]) % 1.0;

      if (t < 0.36) {
        ingressCurve.getPointAt(t / 0.36, this.tmpPos);
      } else {
        egressCurve.getPointAt((t - 0.36) / 0.64, this.tmpPos);
      }

      const endFade = Math.min(1, t / 0.08, (1 - t) / 0.08);
      const s = 0.75 + 0.35 * endFade;
      this.dummy.position.copy(this.tmpPos);
      this.dummy.scale.setScalar(s);
      this.dummy.updateMatrix();
      this.packetInstanced.setMatrixAt(i, this.dummy.matrix);
    }
    this.packetInstanced.instanceMatrix.needsUpdate = true;
  }
}
