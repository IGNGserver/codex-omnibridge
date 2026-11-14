import * as THREE from 'three';
import type { ModelRoutePreset, RoutePathKind } from '../data/productData';

const WAVEGUIDE_VERT = /* glsl */ `
  varying vec2 vUv;
  varying vec3 vNormalView;
  varying vec3 vViewPos;

  void main() {
    vUv = uv;
    vec4 mvPosition = modelViewMatrix * vec4(position, 1.0);
    vViewPos = -mvPosition.xyz;
    vNormalView = normalize(normalMatrix * normal);
    gl_Position = projectionMatrix * mvPosition;
  }
`;

const WAVEGUIDE_FRAG = /* glsl */ `
  uniform float uTime;
  uniform float uActive;
  uniform float uSurge;
  uniform float uSpeed;
  uniform float uDensity;
  uniform vec3 uColor;
  uniform vec3 uSecondaryColor;

  varying vec2 vUv;
  varying vec3 vNormalView;
  varying vec3 vViewPos;

  void main() {
    vec3 viewDir = normalize(vViewPos);
    float fresnel = pow(1.0 - abs(dot(viewDir, normalize(vNormalView))), 1.6);
    float coreProfile = pow(max(0.0, abs(dot(viewDir, normalize(vNormalView)))), 1.35);

    // Traveling optical soliton packets along vUv.x (0 = ingress, 1 = egress)
    float phase1 = fract(vUv.x * uDensity - uTime * uSpeed);
    float packet1 = smoothstep(0.62, 0.98, phase1) * (1.0 - smoothstep(0.98, 1.0, phase1));
    float trail1 = pow(phase1, 4.5) * 0.65;

    float phase2 = fract(vUv.x * (uDensity * 2.0) - uTime * (uSpeed * 1.45) + 0.37);
    float packet2 = pow(phase2, 8.0) * 0.55;

    // Route-switch shockwave sweeping from 0 -> 1
    float surgeFront = 1.0 - smoothstep(0.0, 0.22, abs(vUv.x - uSurge));
    float surgeGlow = surgeFront * step(0.001, uSurge) * (1.0 - step(0.995, uSurge)) * 2.1;

    // Base standby sapphire guide vs active laser waveguide
    float baseIntensity = mix(0.14, 0.82, uActive);
    float pulseIntensity = (packet1 * 2.25 + trail1 * 1.35 + packet2 + surgeGlow) * uActive;

    vec3 coreCol = mix(uColor, uSecondaryColor, clamp(packet1 + surgeGlow * 0.6, 0.0, 1.0));
    vec3 finalRgb = coreCol * (baseIntensity * (0.58 + fresnel * 1.05) + pulseIntensity * (0.68 + coreProfile * 0.95));

    // Taper smoothly at extreme ends to blend inside optical ports
    float endFade = smoothstep(0.0, 0.04, vUv.x) * (1.0 - smoothstep(0.96, 1.0, vUv.x));
    float alpha = clamp((mix(0.20, 0.96, uActive) + pulseIntensity * 0.55) * endFade, 0.0, 1.0);

    gl_FragColor = vec4(finalRgb, alpha);
  }
`;

export interface WaveguideChannel {
  key: string;
  curve: THREE.CatmullRomCurve3;
  mesh: THREE.Mesh<THREE.TubeGeometry, THREE.ShaderMaterial>;
  haloMesh: THREE.Mesh<THREE.TubeGeometry, THREE.MeshBasicMaterial>;
  targetActive: number;
  currentActive: number;
}

export class OpticalWaveguideSystem {
  public readonly group = new THREE.Group();
  private readonly channels: Record<string, WaveguideChannel> = {};
  private readonly photonPoints: THREE.Points<THREE.BufferGeometry, THREE.PointsMaterial>;
  private readonly photonOffsets: Float32Array;
  private readonly photonSpeeds: Float32Array;
  private activeKind: RoutePathKind = 'official';
  private surgeProgress = 1.0;
  private activeAccent = new THREE.Color('#00daf2');
  private activeSecondary = new THREE.Color('#96f0ff');

  constructor() {
    this.group.name = 'Optical_Waveguide_Splines';

    // 1. Unified Ingress Path: Codex CLI -> Ingress Collimator -> Registry Wheels -> Crossbar Core
    this.createChannel(
      'ingress',
      [
        new THREE.Vector3(-3.35, 0.10, 0.0),
        new THREE.Vector3(-2.55, 0.10, 0.0),
        new THREE.Vector3(-1.52, 0.10, 0.0),
        new THREE.Vector3(-0.36, 0.14, 0.0),
      ],
      0.032,
      '#00daf2',
      '#ffffff',
      3.0,
      1.25,
    );

    // 2. Path Alpha: Official OpenAI Zero-Translation Bypass (-Z sapphire vacuum rail)
    this.createChannel(
      'official',
      [
        new THREE.Vector3(-0.36, 0.14, 0.0),
        new THREE.Vector3(-0.36, 0.14, -1.12),
        new THREE.Vector3(0.65, 0.14, -1.12),
        new THREE.Vector3(1.85, 0.14, -1.12),
        new THREE.Vector3(3.18, 0.14, -1.12),
      ],
      0.030,
      '#00daf2',
      '#96f0ff',
      4.0,
      1.55,
    );

    // 3. Path Beta: Protocol Bridge Dispersion Prism Train (Z = 0 center optical axis)
    this.createChannel(
      'bridge',
      [
        new THREE.Vector3(-0.36, 0.14, 0.0),
        new THREE.Vector3(0.58, 0.13, -0.08),
        new THREE.Vector3(1.08, 0.13, 0.09),
        new THREE.Vector3(1.58, 0.13, -0.06),
        new THREE.Vector3(1.92, 0.14, 0.0),
        new THREE.Vector3(3.18, 0.14, 0.0),
      ],
      0.032,
      '#d97bff',
      '#ffffff',
      4.5,
      1.32,
    );

    // 4. Keychain Capability Injection Conduit (Vault -> Protocol Bridge)
    this.createChannel(
      'vault',
      [
        new THREE.Vector3(-0.14, 0.28, 1.34),
        new THREE.Vector3(0.18, 0.20, 0.92),
        new THREE.Vector3(0.56, 0.14, 0.34),
        new THREE.Vector3(0.86, 0.13, 0.06),
      ],
      0.020,
      '#feb958',
      '#fff0c2',
      3.0,
      1.15,
    );

    // 5. Path Gamma: Local Loopback Toroidal Resonator (+Z local 127.0.0.1 closed loop)
    this.createChannel(
      'local',
      [
        new THREE.Vector3(-0.36, 0.14, 0.0),
        new THREE.Vector3(-0.36, 0.14, 1.12),
        new THREE.Vector3(1.14, 0.12, 1.26),
        new THREE.Vector3(1.46, 0.12, 1.58),
        new THREE.Vector3(1.78, 0.12, 1.26),
        new THREE.Vector3(1.46, 0.12, 0.94),
        new THREE.Vector3(2.25, 0.13, 1.12),
        new THREE.Vector3(3.18, 0.14, 1.12),
      ],
      0.029,
      '#4ef09c',
      '#d8ffea',
      5.0,
      1.20,
    );

    // Quantized traveling photon particles riding along the active route spline
    const count = 42;
    const positions = new Float32Array(count * 3);
    this.photonOffsets = new Float32Array(count);
    this.photonSpeeds = new Float32Array(count);
    for (let i = 0; i < count; i++) {
      this.photonOffsets[i] = i / count;
      this.photonSpeeds[i] = 0.32 + (i % 7) * 0.045;
    }
    const pGeom = new THREE.BufferGeometry();
    pGeom.setAttribute('position', new THREE.BufferAttribute(positions, 3));

    const pMat = new THREE.PointsMaterial({
      color: 0x96f0ff,
      size: 0.095,
      transparent: true,
      opacity: 0.92,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
    });
    this.photonPoints = new THREE.Points(pGeom, pMat);
    this.group.add(this.photonPoints);

    // Default initial state
    this.channels.ingress.currentActive = 1;
    this.channels.ingress.targetActive = 1;
    this.channels.official.currentActive = 1;
    this.channels.official.targetActive = 1;
  }

  private createChannel(
    key: string,
    points: THREE.Vector3[],
    radius: number,
    hex: string,
    secondaryHex: string,
    density: number,
    speed: number,
  ) {
    const curve = new THREE.CatmullRomCurve3(points, false, 'catmullrom', 0.22);
    const geom = new THREE.TubeGeometry(curve, 96, radius, 12, false);
    const mat = new THREE.ShaderMaterial({
      vertexShader: WAVEGUIDE_VERT,
      fragmentShader: WAVEGUIDE_FRAG,
      transparent: true,
      depthWrite: false,
      blending: THREE.AdditiveBlending,
      uniforms: {
        uTime: { value: 0 },
        uActive: { value: 0 },
        uSurge: { value: 1.0 },
        uSpeed: { value: speed },
        uDensity: { value: density },
        uColor: { value: new THREE.Color(hex) },
        uSecondaryColor: { value: new THREE.Color(secondaryHex) },
      },
    });

    const mesh = new THREE.Mesh(geom, mat);
    mesh.renderOrder = 8;

    const haloGeom = new THREE.TubeGeometry(curve, 64, radius * 2.85, 8, false);
    const haloMat = new THREE.MeshBasicMaterial({
      color: new THREE.Color(hex),
      transparent: true,
      opacity: 0.0,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
    });
    const haloMesh = new THREE.Mesh(haloGeom, haloMat);
    haloMesh.renderOrder = 7;

    this.group.add(haloMesh);
    this.group.add(mesh);

    this.channels[key] = {
      key,
      curve,
      mesh,
      haloMesh,
      targetActive: 0,
      currentActive: 0,
    };
  }

  public applyPreset(preset: ModelRoutePreset) {
    this.activeKind = preset.routeKind;
    this.surgeProgress = 0.002;
    this.activeAccent.set(preset.accentHex);
    this.activeSecondary.set(preset.secondaryHex);

    this.channels.ingress.targetActive = 1.0;
    this.channels.official.targetActive = preset.routeKind === 'official' ? 1.0 : 0.0;
    this.channels.bridge.targetActive = preset.routeKind === 'bridge' ? 1.0 : 0.0;
    this.channels.vault.targetActive = preset.vaultActive ? 1.0 : 0.0;
    this.channels.local.targetActive = preset.routeKind === 'local' ? 1.0 : 0.0;

    // Tint ingress and active branch dynamically to match selected model's optical spectrum
    this.channels.ingress.mesh.material.uniforms.uColor.value.set(preset.accentHex);
    this.channels.ingress.mesh.material.uniforms.uSecondaryColor.value.set(preset.secondaryHex);
    this.channels.ingress.haloMesh.material.color.set(preset.accentHex);

    if (preset.routeKind === 'bridge') {
      this.channels.bridge.mesh.material.uniforms.uColor.value.set(preset.accentHex);
      this.channels.bridge.mesh.material.uniforms.uSecondaryColor.value.set(preset.secondaryHex);
      this.channels.bridge.haloMesh.material.color.set(preset.accentHex);
    }

    this.photonPoints.material.color.set(preset.secondaryHex);
  }

  public triggerSyncPulse() {
    this.surgeProgress = 0.002;
  }

  public update(elapsed: number, dt: number, reducedMotion: boolean) {
    const timeVal = reducedMotion ? 1.2 : elapsed;

    if (!reducedMotion && this.surgeProgress < 1.0) {
      this.surgeProgress = Math.min(1.0, this.surgeProgress + dt * 1.85);
    } else if (reducedMotion) {
      this.surgeProgress = 1.0;
    }

    const lerpRate = reducedMotion ? 1.0 : Math.min(1.0, dt * 8.5);
    for (const ch of Object.values(this.channels)) {
      ch.currentActive += (ch.targetActive - ch.currentActive) * lerpRate;
      ch.mesh.material.uniforms.uTime.value = timeVal;
      ch.mesh.material.uniforms.uActive.value = ch.currentActive;
      ch.mesh.material.uniforms.uSurge.value = this.surgeProgress;
      ch.haloMesh.material.opacity = ch.currentActive * 0.24;
    }

    // Update discrete quantized photons along ingress + active egress curve
    const ingressCurve = this.channels.ingress.curve;
    const egressCurve = this.channels[this.activeKind].curve;
    const posAttr = this.photonPoints.geometry.getAttribute('position') as THREE.BufferAttribute;
    const tmp = new THREE.Vector3();

    for (let i = 0; i < this.photonOffsets.length; i++) {
      const t = reducedMotion
        ? this.photonOffsets[i]
        : (this.photonOffsets[i] + elapsed * this.photonSpeeds[i]) % 1.0;
      if (t < 0.36) {
        ingressCurve.getPointAt(t / 0.36, tmp);
      } else {
        egressCurve.getPointAt((t - 0.36) / 0.64, tmp);
      }
      posAttr.setXYZ(i, tmp.x, tmp.y, tmp.z);
    }
    posAttr.needsUpdate = true;
  }
}
