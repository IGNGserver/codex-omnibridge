import * as THREE from 'three';
import { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';
import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
import { RoomEnvironment } from 'three/examples/jsm/environments/RoomEnvironment.js';
import { EffectComposer } from 'three/examples/jsm/postprocessing/EffectComposer.js';
import { RenderPass } from 'three/examples/jsm/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/examples/jsm/postprocessing/UnrealBloomPass.js';
import { OutputPass } from 'three/examples/jsm/postprocessing/OutputPass.js';
import {
  INTERNAL_MODULE_STAGES,
  MODEL_ROUTE_PRESETS,
  type ModelRoutePreset,
} from '../data/productData';
import { OpticalWaveguideSystem } from './OpticalWaveguides';

export interface CalloutPinProjection {
  id: string;
  label: string;
  sublabel: string;
  x: number;
  y: number;
  visible: boolean;
  highlighted: boolean;
}

export interface StageCameraPose {
  pos: [number, number, number];
  lookAt: [number, number, number];
  canopyOpen: number;
  StageOpacity: number;
}

const CALLOUT_ANCHORS: Array<{
  id: string;
  nodeName: string;
  label: string;
  sublabel: string;
  world: [number, number, number];
}> = [
  {
    id: 'pin-ingress',
    nodeName: 'Module_Ingress',
    label: 'OMNIBRIDGE INGRESS',
    sublabel: '127.0.0.1:8787/v1/responses',
    world: [-2.62, 0.56, 0.0],
  },
  {
    id: 'pin-registry',
    nodeName: 'Module_Registry',
    label: 'CATALOG MERGER',
    sublabel: 'crates/catalog · bundled schema',
    world: [-1.52, 0.66, 0.0],
  },
  {
    id: 'pin-crossbar',
    nodeName: 'Module_Crossbar',
    label: 'MEMS CROSSBAR',
    sublabel: 'crates/router · per-turn dispatch',
    world: [-0.36, 0.68, 0.0],
  },
  {
    id: 'pin-bridge',
    nodeName: 'Module_ProtocolBridge',
    label: 'PROTOCOL PRISMS',
    sublabel: 'Responses ⇄ Chat SSE',
    world: [1.08, 0.60, 0.0],
  },
  {
    id: 'pin-vault',
    nodeName: 'Module_KeychainVault',
    label: 'KEYCHAIN VAULT',
    sublabel: 'OS Keyring · OAuth Bulkhead',
    world: [-0.14, 0.58, 1.34],
  },
  {
    id: 'pin-egress',
    nodeName: 'Manifold_Egress',
    label: 'TRI-PORT EGRESS',
    sublabel: 'Official / Bridge / Loopback',
    world: [2.72, 0.58, 0.0],
  },
];

export class PhotonicStage {
  private readonly container: HTMLElement;
  private renderer: THREE.WebGLRenderer | null = null;
  private composer: EffectComposer | null = null;
  private bloomPass: UnrealBloomPass | null = null;
  private readonly scene = new THREE.Scene();
  private readonly camera: THREE.PerspectiveCamera;
  private controls: OrbitControls | null = null;

  private readonly rootPivot = new THREE.Group();
  private waveguides: OpticalWaveguideSystem | null = null;
  private keySpotLight: THREE.SpotLight | null = null;
  private corePointLight: THREE.PointLight | null = null;

  // Named mechanical nodes loaded from `omnibridge-photonic-matrix.glb`
  private canopyPort: THREE.Object3D | null = null;
  private canopyStarboard: THREE.Object3D | null = null;
  private registryWheelOuter: THREE.Object3D | null = null;
  private registryWheelInner: THREE.Object3D | null = null;
  private crossbarYaw: THREE.Object3D | null = null;
  private crossbarPitch: THREE.Object3D | null = null;
  private vaultLockDial: THREE.Object3D | null = null;
  private namedModules: Map<string, THREE.Object3D> = new Map();

  // Spring physics states for mechanical parts
  private targetGimbalYaw = -0.44;
  private currentGimbalYaw = -0.44;
  private velGimbalYaw = 0;

  private targetGimbalPitch = 0.08;
  private currentGimbalPitch = 0.08;
  private velGimbalPitch = 0;

  private targetVaultAngle = 0;
  private currentVaultAngle = 0;
  private wheelSpinBoost = 0;

  // Scroll & interactive camera states
  private targetCamPos = new THREE.Vector3(3.35, 2.35, 4.65);
  private currentCamPos = new THREE.Vector3(3.35, 2.35, 4.65);
  private targetLookAt = new THREE.Vector3(0.05, 0.04, 0.0);
  private currentLookAt = new THREE.Vector3(0.05, 0.04, 0.0);
  private targetCanopyOpen = 0.0;
  private currentCanopyOpen = 0.0;
  private manualCanopyOverride: boolean | null = null;

  private pointerNdc = new THREE.Vector2(0, 0);
  private smoothPointer = new THREE.Vector2(0, 0);
  private freeInspectMode = false;
  private pinsEnabledInSection = true;
  private highlightedModuleNode: string | null = null;
  private activePreset: ModelRoutePreset = MODEL_ROUTE_PRESETS[0];
  private reducedMotion = false;
  private isCompactViewport = false;
  private isSoftwareGl = false;
  private webglReady = false;
  private modelLoaded = false;
  private rafId = 0;
  private lastTimestamp = 0;
  private lastRenderMs = 0;

  private onPinsUpdate?: (pins: CalloutPinProjection[]) => void;
  private onModuleSelect?: (nodeName: string) => void;

  constructor(
    container: HTMLElement,
    callbacks?: {
      onPinsUpdate?: (pins: CalloutPinProjection[]) => void;
      onModuleSelect?: (nodeName: string) => void;
    },
  ) {
    this.container = container;
    this.onPinsUpdate = callbacks?.onPinsUpdate;
    this.onModuleSelect = callbacks?.onModuleSelect;

    const w = Math.max(1, container.clientWidth || window.innerWidth);
    const h = Math.max(1, container.clientHeight || window.innerHeight);
    this.isCompactViewport = w < 860;
    this.reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    this.camera = new THREE.PerspectiveCamera(this.isCompactViewport ? 44 : 36, w / h, 0.1, 60);
    this.camera.position.copy(this.currentCamPos);
    this.camera.lookAt(this.currentLookAt);

    this.initRendererOrFallback(w, h);
  }

  private initRendererOrFallback(w: number, h: number) {
    try {
      const isAutomated = typeof navigator !== 'undefined' && navigator.webdriver === true;
      const renderer = new THREE.WebGLRenderer({
        antialias: !this.isCompactViewport && !isAutomated,
        alpha: true,
        powerPreference: 'high-performance',
        preserveDrawingBuffer: isAutomated,
      });

      const gl = renderer.getContext();
      const dbg = gl.getExtension('WEBGL_debug_renderer_info');
      const unmaskedRenderer = dbg ? String(gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL)) : '';
      this.isSoftwareGl =
        isAutomated || /swiftshader|llvmpipe|software|basic render/i.test(unmaskedRenderer);

      const maxDpr = this.isSoftwareGl ? 1.0 : this.isCompactViewport ? 1.35 : 1.75;
      renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, maxDpr));
      renderer.setSize(w, h);
      renderer.toneMapping = THREE.ACESFilmicToneMapping;
      renderer.toneMappingExposure = 1.16;
      renderer.outputColorSpace = THREE.SRGBColorSpace;
      renderer.domElement.className = 'photonic-webgl-canvas';
      renderer.domElement.setAttribute('aria-hidden', 'true');

      this.container.innerHTML = '';
      this.container.appendChild(renderer.domElement);
      this.renderer = renderer;
      this.webglReady = true;

      this.setupStudioEnvironment(renderer);
      this.setupLightingAndStage();
      this.setupPostProcessing(renderer, w, h);
      this.setupControls(renderer.domElement);
      this.loadHardwareGLB();
      this.bindEvents();
      this.startLoop();
    } catch (err) {
      console.warn('[PhotonicStage] WebGL unavailable, engaging precision SVG fallback:', err);
      this.webglReady = false;
      this.renderSvgFallback();
    }
  }

  private setupStudioEnvironment(renderer: THREE.WebGLRenderer) {
    const pmrem = new THREE.PMREMGenerator(renderer);
    const roomEnv = new RoomEnvironment();
    const envRT = pmrem.fromScene(roomEnv, 0.04);
    this.scene.environment = envRT.texture;
    this.scene.environmentIntensity = 0.82;
    roomEnv.dispose();
    pmrem.dispose();
  }

  private setupLightingAndStage() {
    this.scene.add(this.rootPivot);

    // Subtle precision laboratory optical-bench reference grid
    const grid = new THREE.GridHelper(18, 36, 0x006874, 0x132629);
    grid.position.y = -0.92;
    const gridMat = grid.material as THREE.Material;
    gridMat.transparent = true;
    gridMat.opacity = 0.34;
    this.scene.add(grid);

    // Studio three-point + under-chassis optical lighting
    const hemi = new THREE.HemisphereLight(0xbcebf3, 0x050c0e, 0.65);
    this.scene.add(hemi);

    const keyDir = new THREE.DirectionalLight(0xe6fbff, 2.2);
    keyDir.position.set(4.5, 6.5, 4.0);
    this.scene.add(keyDir);

    const rimDir = new THREE.DirectionalLight(0x00daf2, 1.45);
    rimDir.position.set(-5.0, 2.8, -4.2);
    this.scene.add(rimDir);

    const warmRim = new THREE.DirectionalLight(0xfeb958, 0.72);
    warmRim.position.set(2.5, -1.5, 4.8);
    this.scene.add(warmRim);

    this.keySpotLight = new THREE.SpotLight(0x00daf2, 6.0, 12, Math.PI / 5, 0.65, 1.4);
    this.keySpotLight.position.set(0, 3.8, 0.8);
    this.scene.add(this.keySpotLight);

    this.corePointLight = new THREE.PointLight(0x00daf2, 2.8, 5.5, 1.6);
    this.corePointLight.position.set(-0.2, 0.25, 0);
    this.rootPivot.add(this.corePointLight);

    // Add 3D Custom GLSL Optical Waveguides
    this.waveguides = new OpticalWaveguideSystem();
    this.rootPivot.add(this.waveguides.group);
    this.waveguides.applyPreset(this.activePreset);
  }

  private setupPostProcessing(renderer: THREE.WebGLRenderer, w: number, h: number) {
    if (this.isCompactViewport || this.isSoftwareGl) {
      // Direct single-pass render on mobile & software GL for crisp 60fps & battery efficiency
      this.composer = null;
      this.bloomPass = null;
      return;
    }
    const composer = new EffectComposer(renderer);
    const renderPass = new RenderPass(this.scene, this.camera);
    composer.addPass(renderPass);

    const bloom = new UnrealBloomPass(
      new THREE.Vector2(Math.max(1, Math.round(w * 0.5)), Math.max(1, Math.round(h * 0.5))),
      0.44,
      0.36,
      0.76,
    );
    composer.addPass(bloom);

    const outputPass = new OutputPass();
    composer.addPass(outputPass);

    this.composer = composer;
    this.bloomPass = bloom;
  }

  private setupControls(domElement: HTMLCanvasElement) {
    const controls = new OrbitControls(this.camera, domElement);
    controls.enableDamping = true;
    controls.dampingFactor = 0.06;
    controls.enablePan = false;
    controls.minDistance = 2.1;
    controls.maxDistance = 9.2;
    controls.maxPolarAngle = Math.PI * 0.56;
    controls.minPolarAngle = 0.18;
    controls.enabled = false;
    this.controls = controls;
  }

  private loadHardwareGLB() {
    const loader = new GLTFLoader();
    loader.load(
      './models/omnibridge-photonic-matrix.glb',
      (gltf) => {
        const model = gltf.scene;
        model.traverse((obj) => {
          if (obj.name) {
            this.namedModules.set(obj.name, obj);
          }
          if ((obj as THREE.Mesh).isMesh) {
            const mesh = obj as THREE.Mesh;
            const mat = mesh.material as THREE.MeshStandardMaterial;
            if (mat && mat.name === 'Borosilicate_Quartz_Shell') {
              mesh.material = new THREE.MeshPhysicalMaterial({
                name: 'Borosilicate_Quartz_Upgraded',
                color: new THREE.Color(0x9ceeff),
                emissive: new THREE.Color(0x042630),
                emissiveIntensity: 0.28,
                metalness: 0.08,
                roughness: 0.05,
                ior: 1.52,
                reflectivity: 1.0,
                envMapIntensity: 2.1,
                clearcoat: 1.0,
                clearcoatRoughness: 0.04,
                transparent: true,
                opacity: 0.28,
                depthWrite: false,
                side: THREE.DoubleSide,
              });
              mesh.renderOrder = 6;
            } else if (mat && mat.name === 'Dichroic_Prism_Crystal') {
              mesh.material = new THREE.MeshPhysicalMaterial({
                name: 'Dichroic_Prism_Upgraded',
                color: new THREE.Color(0xdcfbff),
                emissive: new THREE.Color(0x183c54),
                emissiveIntensity: 0.85,
                metalness: 0.14,
                roughness: 0.04,
                ior: 1.78,
                reflectivity: 1.0,
                envMapIntensity: 2.4,
                clearcoat: 1.0,
                clearcoatRoughness: 0.03,
                transparent: true,
                opacity: 0.68,
                depthWrite: false,
                side: THREE.DoubleSide,
              });
              mesh.renderOrder = 5;
            } else if (mat && mat.name?.startsWith('Emitter_')) {
              mat.emissiveIntensity = 1.65;
            }
          }
        });

        this.canopyPort = this.namedModules.get('Canopy_Port') || null;
        this.canopyStarboard = this.namedModules.get('Canopy_Starboard') || null;
        this.registryWheelOuter = this.namedModules.get('Registry_Wheel_Outer') || null;
        this.registryWheelInner = this.namedModules.get('Registry_Wheel_Inner') || null;
        this.crossbarYaw = this.namedModules.get('Crossbar_Gimbal_Yaw') || null;
        this.crossbarPitch = this.namedModules.get('Crossbar_Gimbal_Pitch') || null;
        this.vaultLockDial = this.namedModules.get('Vault_LockDial') || null;

        this.rootPivot.add(model);
        this.modelLoaded = true;
        this.container.dataset.glbLoaded = 'true';
      },
      undefined,
      (err) => {
        console.error('[PhotonicStage] Failed to load GLB asset:', err);
      },
    );
  }

  private bindEvents() {
    window.addEventListener('resize', this.handleResize, { passive: true });
    window.addEventListener('pointermove', this.handlePointerMove, { passive: true });

    this.renderer?.domElement.addEventListener('click', (ev) => {
      if (!this.renderer || !this.modelLoaded) return;
      const rect = this.renderer.domElement.getBoundingClientRect();
      const ndc = new THREE.Vector2(
        ((ev.clientX - rect.left) / rect.width) * 2 - 1,
        -((ev.clientY - rect.top) / rect.height) * 2 + 1,
      );
      const raycaster = new THREE.Raycaster();
      raycaster.setFromCamera(ndc, this.camera);
      const hits = raycaster.intersectObjects(this.rootPivot.children, true);
      for (const hit of hits) {
        let cur: THREE.Object3D | null = hit.object;
        while (cur) {
          if (
            cur.name === 'Module_Ingress' ||
            cur.name === 'Module_Registry' ||
            cur.name === 'Module_Crossbar' ||
            cur.name === 'Module_ProtocolBridge' ||
            cur.name === 'Module_KeychainVault' ||
            cur.name === 'Manifold_Egress'
          ) {
            this.onModuleSelect?.(cur.name);
            return;
          }
          cur = cur.parent;
        }
      }
    });
  }

  private handlePointerMove = (e: PointerEvent) => {
    this.pointerNdc.x = (e.clientX / Math.max(1, window.innerWidth)) * 2 - 1;
    this.pointerNdc.y = -((e.clientY / Math.max(1, window.innerHeight)) * 2 - 1);
  };

  private handleResize = () => {
    const w = Math.max(1, this.container.clientWidth || window.innerWidth);
    const h = Math.max(1, this.container.clientHeight || window.innerHeight);
    const wasCompact = this.isCompactViewport;
    this.isCompactViewport = w < 860;

    this.camera.fov = this.isCompactViewport ? 45 : 36;
    this.camera.aspect = w / h;
    this.camera.updateProjectionMatrix();

    if (this.renderer) {
      const maxDpr = this.isSoftwareGl ? 1.0 : this.isCompactViewport ? 1.35 : 1.75;
      this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, maxDpr));
      this.renderer.setSize(w, h);
      if (wasCompact !== this.isCompactViewport) {
        this.setupPostProcessing(this.renderer, w, h);
      } else if (this.composer) {
        this.composer.setSize(w, h);
        this.bloomPass?.setSize(Math.max(1, Math.round(w * 0.5)), Math.max(1, Math.round(h * 0.5)));
      }
    }
  };

  /* ------------------------------------------------------------------------ */
  /*  Public Controls for Model Switching, Scroll Stages & Free Orbit         */
  /* ------------------------------------------------------------------------ */

  public setModelPreset(preset: ModelRoutePreset) {
    this.activePreset = preset;
    this.targetGimbalYaw = preset.gimbalYawRad;
    this.targetGimbalPitch = preset.gimbalPitchRad;
    this.targetVaultAngle = preset.vaultActive ? Math.PI / 3 : 0;
    this.wheelSpinBoost = 3.2;

    if (this.corePointLight) {
      this.corePointLight.color.set(preset.accentHex);
    }
    if (this.keySpotLight) {
      this.keySpotLight.color.set(preset.secondaryHex);
    }
    this.waveguides?.applyPreset(preset);

    if (!this.webglReady) {
      this.renderSvgFallback();
    }
  }

  public triggerCatalogSyncEffect() {
    this.wheelSpinBoost = 5.5;
    this.waveguides?.triggerSyncPulse();
  }

  public setFreeInspectMode(enabled: boolean) {
    this.freeInspectMode = enabled;
    if (this.controls) {
      this.controls.enabled = enabled;
      if (enabled) {
        this.controls.target.copy(this.currentLookAt);
        this.controls.update();
      }
    }
  }

  public toggleCanopyManual(): boolean {
    const next = !(this.manualCanopyOverride ?? this.targetCanopyOpen > 0.5);
    this.manualCanopyOverride = next;
    return next;
  }

  public clearManualCanopy() {
    this.manualCanopyOverride = null;
  }

  public setHighlightedModule(nodeName: string | null) {
    this.highlightedModuleNode = nodeName;
  }

  /**
   * Choreographs the 3D camera & quartz canopy across the entire document scroll:
   * - Hero section (`heroProgress` 0..1): Wide precision 3/4 view shifted slightly right on desktop
   * - Inside Matrix section (`insideProgress` 0..1): Unlatches & parts quartz canopy, flies through 4 internal modules
   * - Thread Lab & Lower sections (`afterInsideProgress` 0..1): Elevated isometric telemetry overview
   */
  public updateScrollChoreography(params: {
    heroProgress: number;
    insideActive: boolean;
    insideProgress: number;
    threadActive: boolean;
  }) {
    if (this.freeInspectMode) return;

    const compact = this.isCompactViewport;

    if (!params.insideActive && !params.threadActive && params.heroProgress < 1.0) {
      // ACT I: Hero 3/4 Angle (shifted cleanly to starboard on desktop so left editorial copy stays unobstructed)
      const t = params.heroProgress;
      const basePos = compact ? [0.0, 2.95, 6.25] : [-0.45, 2.38, 5.15];
      const look = compact ? [0.0, -0.25, 0.0] : [-1.18, 0.02, 0.0];
      this.targetCamPos.set(
        basePos[0] + t * 0.45,
        basePos[1] - t * 0.25,
        basePos[2] - t * 0.55,
      );
      this.targetLookAt.set(look[0] + t * 0.5, look[1], look[2]);
      this.targetCanopyOpen = t > 0.72 ? ((t - 0.72) / 0.28) * 0.45 : 0.0;
      this.highlightedModuleNode = null;
      this.pinsEnabledInSection = true;
      return;
    }

    if (params.insideActive) {
      // ACT II: Dive Inside the Matrix — interpolate smoothly across the 4 module stations
      this.pinsEnabledInSection = true;
      const stages = INTERNAL_MODULE_STAGES;
      const scaled = Math.max(0, Math.min(1, params.insideProgress)) * (stages.length - 1);
      const idxA = Math.min(stages.length - 1, Math.floor(scaled));
      const idxB = Math.min(stages.length - 1, idxA + 1);
      const frac = scaled - idxA;
      // Smoothstep blend between stations so camera lingers at each optical module
      const s = frac * frac * (3 - 2 * frac);

      const stA = stages[idxA];
      const stB = stages[idxB];

      const zPullback = compact ? 1.35 : 0.0;
      const xOffset = compact ? 0.0 : -0.62; // Keep module visible on right half while cards sit on left

      this.targetCamPos.set(
        THREE.MathUtils.lerp(stA.cameraPos[0], stB.cameraPos[0], s) + xOffset * 0.35,
        THREE.MathUtils.lerp(stA.cameraPos[1], stB.cameraPos[1], s) + (compact ? 0.55 : 0),
        THREE.MathUtils.lerp(stA.cameraPos[2], stB.cameraPos[2], s) + zPullback,
      );
      this.targetLookAt.set(
        THREE.MathUtils.lerp(stA.cameraLookAt[0], stB.cameraLookAt[0], s) + xOffset,
        THREE.MathUtils.lerp(stA.cameraLookAt[1], stB.cameraLookAt[1], s),
        THREE.MathUtils.lerp(stA.cameraLookAt[2], stB.cameraLookAt[2], s),
      );
      this.targetCanopyOpen = THREE.MathUtils.lerp(stA.canopyOpenFactor, stB.canopyOpenFactor, s);
      this.highlightedModuleNode = frac < 0.5 ? stA.nodeName : stB.nodeName;
      return;
    }

    // Below Inside Matrix: hide floating 3D callout reticles so lower interactive consoles stay 100% clean
    this.pinsEnabledInSection = false;

    if (params.threadActive) {
      // ACT III: Top-down 45° tactical router overview during Same-Thread Live Replay
      this.targetCamPos.set(compact ? 0.0 : 0.2, compact ? 4.35 : 3.55, compact ? 4.8 : 3.45);
      this.targetLookAt.set(compact ? 0.0 : -0.75, 0.0, 0.0);
      this.targetCanopyOpen = 0.65;
      this.highlightedModuleNode = 'Module_Crossbar';
      return;
    }

    // Ambient background framing for lower sections
    this.targetCamPos.set(0.0, 3.8, 5.6);
    this.targetLookAt.set(0.0, 0.0, 0.0);
    this.targetCanopyOpen = 0.25;
    this.highlightedModuleNode = null;
  }

  /* ------------------------------------------------------------------------ */
  /*  60fps Frame Loop                                                        */
  /* ------------------------------------------------------------------------ */

  private startLoop() {
    const tick = (nowMs: number) => {
      this.rafId = requestAnimationFrame(tick);
      if (document.hidden) return;

      // Yield ample main-thread budget on CPU software WebGL (SwiftShader / llvmpipe)
      if (this.isSoftwareGl && nowMs - this.lastRenderMs < 48) {
        return;
      }
      this.lastRenderMs = nowMs;

      const elapsed = nowMs * 0.001;
      const dt = this.lastTimestamp ? Math.min(0.06, (nowMs - this.lastTimestamp) * 0.001) : 0.016;
      this.lastTimestamp = nowMs;

      this.stepSimulation(elapsed, dt);

      if (this.composer) {
        this.composer.render();
      } else if (this.renderer) {
        this.renderer.render(this.scene, this.camera);
      }

      this.projectCalloutPins();
    };
    this.rafId = requestAnimationFrame(tick);
  }

  private stepSimulation(elapsed: number, dt: number) {
    // 1. Damped harmonic spring for MEMS Crossbar Gimbal
    const stiffness = 135;
    const damping = 16;
    if (this.reducedMotion) {
      this.currentGimbalYaw = this.targetGimbalYaw;
      this.currentGimbalPitch = this.targetGimbalPitch;
    } else {
      const ay = (this.targetGimbalYaw - this.currentGimbalYaw) * stiffness - this.velGimbalYaw * damping;
      this.velGimbalYaw += ay * dt;
      this.currentGimbalYaw += this.velGimbalYaw * dt;

      const ap = (this.targetGimbalPitch - this.currentGimbalPitch) * stiffness - this.velGimbalPitch * damping;
      this.velGimbalPitch += ap * dt;
      this.currentGimbalPitch += this.velGimbalPitch * dt;
    }

    if (this.crossbarYaw) {
      this.crossbarYaw.rotation.y = this.currentGimbalYaw;
    }
    if (this.crossbarPitch) {
      this.crossbarPitch.rotation.z = this.currentGimbalPitch;
    }

    // 2. Registry Code-Wheels rotation
    this.wheelSpinBoost = Math.max(0, this.wheelSpinBoost - dt * 2.8);
    if (!this.reducedMotion) {
      const spinSpeed = 0.28 + this.wheelSpinBoost;
      if (this.registryWheelOuter) {
        this.registryWheelOuter.rotation.x += dt * spinSpeed;
      }
      if (this.registryWheelInner) {
        this.registryWheelInner.rotation.x -= dt * (spinSpeed * 1.35);
      }
    }

    // 3. Keychain Vault Breach Lock Dial
    const vaultLerp = this.reducedMotion ? 1.0 : Math.min(1.0, dt * 7.5);
    this.currentVaultAngle += (this.targetVaultAngle - this.currentVaultAngle) * vaultLerp;
    if (this.vaultLockDial) {
      this.vaultLockDial.rotation.y = this.currentVaultAngle;
    }

    // 4. Retractable Borosilicate Quartz Canopy (lifts & parts smoothly)
    const effectiveCanopyTarget =
      this.manualCanopyOverride !== null
        ? this.manualCanopyOverride
          ? 1.0
          : 0.0
        : this.targetCanopyOpen;
    const canopyLerp = this.reducedMotion ? 1.0 : Math.min(1.0, dt * 5.2);
    this.currentCanopyOpen += (effectiveCanopyTarget - this.currentCanopyOpen) * canopyLerp;

    if (this.canopyPort) {
      this.canopyPort.position.y = this.currentCanopyOpen * 0.34;
      this.canopyPort.position.z = -this.currentCanopyOpen * 0.68;
    }
    if (this.canopyStarboard) {
      this.canopyStarboard.position.y = this.currentCanopyOpen * 0.34;
      this.canopyStarboard.position.z = this.currentCanopyOpen * 0.68;
    }

    // 5. Subtle floating breath + pointer parallax on rootPivot when not in Orbit mode
    this.smoothPointer.lerp(this.pointerNdc, Math.min(1.0, dt * 4.5));
    if (!this.freeInspectMode) {
      const camLerp = this.reducedMotion ? 1.0 : Math.min(1.0, dt * 4.8);
      this.currentCamPos.lerp(this.targetCamPos, camLerp);
      this.currentLookAt.lerp(this.targetLookAt, camLerp);
      this.camera.position.copy(this.currentCamPos);
      this.camera.lookAt(this.currentLookAt);

      if (!this.reducedMotion) {
        this.rootPivot.rotation.y = this.smoothPointer.x * 0.09;
        this.rootPivot.rotation.x = -this.smoothPointer.y * 0.045;
        this.rootPivot.position.y = Math.sin(elapsed * 1.15) * 0.025;
      } else {
        this.rootPivot.rotation.set(0, 0, 0);
        this.rootPivot.position.set(0, 0, 0);
      }
    } else {
      this.rootPivot.rotation.y *= 0.9;
      this.rootPivot.rotation.x *= 0.9;
      this.controls?.update();
    }

    // 6. Update Custom GLSL Waveguides
    this.waveguides?.update(elapsed, dt, this.reducedMotion);
  }

  private projectCalloutPins() {
    if (!this.onPinsUpdate || !this.renderer) return;
    const w = this.container.clientWidth || window.innerWidth;
    const h = this.container.clientHeight || window.innerHeight;
    const v = new THREE.Vector3();

    const pins: CalloutPinProjection[] = CALLOUT_ANCHORS.map((a) => {
      v.set(a.world[0], a.world[1], a.world[2]);
      this.rootPivot.localToWorld(v);
      v.project(this.camera);

      const sx = (v.x * 0.5 + 0.5) * w;
      const sy = (-v.y * 0.5 + 0.5) * h;
      const inFrustum = v.z > 0 && v.z < 1 && Math.abs(v.x) <= 0.94 && Math.abs(v.y) <= 0.88;
      // Avoid overlapping left editorial copy, top nav bar, or bottom console dock when not in free inspect
      const clearOfCopy =
        this.freeInspectMode ||
        this.isCompactViewport ||
        (sx >= w * 0.45 && sy >= 96 && sy <= h - 220);

      return {
        id: a.id,
        label: a.label,
        sublabel: a.sublabel,
        x: sx,
        y: sy,
        visible: inFrustum && clearOfCopy && (this.freeInspectMode || this.pinsEnabledInSection),
        highlighted: this.highlightedModuleNode === a.nodeName,
      };
    });

    this.onPinsUpdate(pins);
  }

  private renderSvgFallback() {
    const p = this.activePreset;
    const isOfficial = p.routeKind === 'official';
    const isBridge = p.routeKind === 'bridge';
    const isLocal = p.routeKind === 'local';

    this.container.dataset.glbLoaded = 'fallback';
    this.container.innerHTML = `
      <div class="photonic-svg-fallback" aria-hidden="true">
        <svg viewBox="0 0 960 540" fill="none" xmlns="http://www.w3.org/2000/svg">
          <rect x="90" y="70" width="780" height="400" rx="28" stroke="${p.accentHex}" stroke-opacity="0.35" stroke-width="1.5" fill="#071214" fill-opacity="0.86"/>
          <path d="M120 270 H390" stroke="${p.accentHex}" stroke-width="4"/>
          <path d="M390 270 L460 150 H830" stroke="#00daf2" stroke-width="${isOfficial ? 4.5 : 1.2}" stroke-opacity="${isOfficial ? 1 : 0.22}"/>
          <path d="M390 270 H830" stroke="${p.accentHex}" stroke-width="${isBridge ? 4.5 : 1.2}" stroke-opacity="${isBridge ? 1 : 0.22}"/>
          <path d="M390 270 L460 390 H830" stroke="#4ef09c" stroke-width="${isLocal ? 4.5 : 1.2}" stroke-opacity="${isLocal ? 1 : 0.22}"/>
          <circle cx="240" cy="270" r="34" stroke="#96f0ff" stroke-width="2" fill="#0c1d20"/>
          <circle cx="390" cy="270" r="42" stroke="${p.accentHex}" stroke-width="2.5" fill="#0c1d20"/>
          <polygon points="560,242 592,298 528,298" stroke="#d97bff" stroke-width="2" fill="#171224"/>
          <polygon points="635,298 667,242 603,242" stroke="#d97bff" stroke-width="2" fill="#171224"/>
          <rect x="465" y="340" width="120" height="72" rx="14" stroke="#feb958" stroke-width="1.8" fill="#19160d"/>
        </svg>
      </div>
    `;
  }

  public dispose() {
    cancelAnimationFrame(this.rafId);
    window.removeEventListener('resize', this.handleResize);
    window.removeEventListener('pointermove', this.handlePointerMove);
    this.renderer?.dispose();
  }
}
