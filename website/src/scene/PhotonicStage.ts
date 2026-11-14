import * as THREE from 'three';
import { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';
import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
import { RoomEnvironment } from 'three/examples/jsm/environments/RoomEnvironment.js';
import { MODEL_ROUTE_PRESETS, type ModelRoutePreset } from '../data/productData';
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
    label: '统一入口 :8787',
    sublabel: '127.0.0.1:8787',
    world: [-2.05, 0.72, -0.35],
  },
  {
    id: 'pin-crossbar',
    nodeName: 'Module_Crossbar',
    label: '按轮次分流',
    sublabel: '官方 · 云端 · 本机',
    world: [-0.36, 1.05, -0.55],
  },
  {
    id: 'pin-bridge',
    nodeName: 'Module_ProtocolBridge',
    label: '协议自动转换',
    sublabel: 'Responses ⇄ Chat',
    world: [1.32, 0.78, -0.15],
  },
  {
    id: 'pin-vault',
    nodeName: 'Module_KeychainVault',
    label: '钥匙串隔离',
    sublabel: 'OAuth 与 API Key 分离',
    world: [-0.02, 0.36, 1.38],
  },
];

const DEFAULT_CAM_POS = new THREE.Vector3(-0.35, 3.05, 5.10);
const DEFAULT_LOOK_AT = new THREE.Vector3(0.02, -0.05, 0.0);

export class PhotonicStage {
  private readonly container: HTMLElement;
  private renderer: THREE.WebGLRenderer | null = null;
  private readonly scene = new THREE.Scene();
  private readonly camera: THREE.PerspectiveCamera;
  private controls: OrbitControls | null = null;

  private readonly rootPivot = new THREE.Group();
  private waveguides: OpticalWaveguideSystem | null = null;
  private gridHelper: THREE.GridHelper | null = null;
  private hemiLight: THREE.HemisphereLight | null = null;
  private keyLight: THREE.DirectionalLight | null = null;
  private fillLight: THREE.DirectionalLight | null = null;
  private corePointLight: THREE.PointLight | null = null;

  // Theme-reactive GLB material references
  private platformCeramicMat: THREE.MeshStandardMaterial | null = null;
  private chassisSlateMat: THREE.MeshStandardMaterial | null = null;

  // Named mechanical nodes loaded from GLB
  private registryWheelOuter: THREE.Object3D | null = null;
  private registryWheelInner: THREE.Object3D | null = null;
  private crossbarYaw: THREE.Object3D | null = null;
  private crossbarPitch: THREE.Object3D | null = null;
  private vaultLockDial: THREE.Object3D | null = null;
  private namedModules: Map<string, THREE.Object3D> = new Map();

  // Spring physics for gimbal rotary director
  private targetGimbalYaw = -0.42;
  private currentGimbalYaw = -0.42;
  private velGimbalYaw = 0;

  private targetGimbalPitch = 0.06;
  private currentGimbalPitch = 0.06;
  private velGimbalPitch = 0;

  private targetVaultAngle = 0;
  private currentVaultAngle = 0;
  private wheelSpinBoost = 0;

  // Pointer & orbit states
  private pointerNdc = new THREE.Vector2(0, 0);
  private smoothPointer = new THREE.Vector2(0, 0);
  private freeInspectMode = false;
  private activePreset: ModelRoutePreset = MODEL_ROUTE_PRESETS[0];
  private siteTheme: 'light' | 'dark' = 'light';
  private reducedMotion = false;
  private isCompactViewport = false;
  private isSoftwareGl = false;
  private webglReady = false;
  private modelLoaded = false;
  private isInViewport = true;
  private rafId = 0;
  private lastTimestamp = 0;
  private lastRenderMs = 0;
  private intersectionObserver: IntersectionObserver | null = null;

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

    const w = Math.max(1, container.clientWidth || 640);
    const h = Math.max(1, container.clientHeight || 380);
    this.isCompactViewport = w < 620;
    this.reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    this.camera = new THREE.PerspectiveCamera(this.isCompactViewport ? 44 : 36, w / h, 0.1, 50);
    this.camera.position.copy(DEFAULT_CAM_POS);
    this.camera.lookAt(DEFAULT_LOOK_AT);

    this.initRendererOrFallback(w, h);
  }

  private initRendererOrFallback(w: number, h: number) {
    try {
      const isAutomated = typeof navigator !== 'undefined' && navigator.webdriver === true;
      // Direct MSAA + true alpha transparency (no UnrealBloomPass washout!)
      const renderer = new THREE.WebGLRenderer({
        antialias: true,
        alpha: true,
        powerPreference: 'high-performance',
        preserveDrawingBuffer: isAutomated,
      });

      const gl = renderer.getContext();
      const dbg = gl.getExtension('WEBGL_debug_renderer_info');
      const unmaskedRenderer = dbg ? String(gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL)) : '';
      this.isSoftwareGl =
        isAutomated || /swiftshader|llvmpipe|software|basic render/i.test(unmaskedRenderer);

      const maxDpr = this.isSoftwareGl ? 1.0 : this.isCompactViewport ? 1.5 : 2.0;
      renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, maxDpr));
      renderer.setSize(w, h);
      renderer.toneMapping = THREE.ACESFilmicToneMapping;
      renderer.toneMappingExposure = 1.02;
      renderer.outputColorSpace = THREE.SRGBColorSpace;
      renderer.domElement.className = 'photonic-webgl-canvas';
      renderer.domElement.setAttribute('aria-hidden', 'true');

      this.container.innerHTML = '';
      this.container.appendChild(renderer.domElement);
      this.renderer = renderer;
      this.webglReady = true;

      this.setupStudioEnvironment(renderer);
      this.setupLightingAndStage();
      this.setupControls(renderer.domElement);
      this.loadHardwareGLB();
      this.bindEvents();
      this.applySiteThemeColors();
      this.startLoop();
    } catch (err) {
      console.warn('[PhotonicStage] WebGL unavailable, rendering M3 SVG schematic fallback:', err);
      this.webglReady = false;
      this.renderSvgFallback();
    }
  }

  private setupStudioEnvironment(renderer: THREE.WebGLRenderer) {
    const pmrem = new THREE.PMREMGenerator(renderer);
    const roomEnv = new RoomEnvironment();
    const envRT = pmrem.fromScene(roomEnv, 0.04);
    this.scene.environment = envRT.texture;
    // Moderate environment reflections prevent any metallic glare or milky glass
    this.scene.environmentIntensity = 0.42;
    roomEnv.dispose();
    pmrem.dispose();
  }

  private setupLightingAndStage() {
    this.scene.add(this.rootPivot);

    const grid = new THREE.GridHelper(14, 28, 0x7a969b, 0xc3d6da);
    grid.position.y = -0.66;
    const gridMat = grid.material as THREE.Material;
    gridMat.transparent = true;
    gridMat.opacity = 0.28;
    this.scene.add(grid);
    this.gridHelper = grid;

    this.hemiLight = new THREE.HemisphereLight(0xf6feff, 0xa8c2c7, 0.95);
    this.scene.add(this.hemiLight);

    this.keyLight = new THREE.DirectionalLight(0xffffff, 1.55);
    this.keyLight.position.set(4.2, 6.4, 4.6);
    this.scene.add(this.keyLight);

    this.fillLight = new THREE.DirectionalLight(0x96f0ff, 0.72);
    this.fillLight.position.set(-4.8, 3.0, -3.5);
    this.scene.add(this.fillLight);

    this.corePointLight = new THREE.PointLight(0x006874, 1.2, 4.8, 1.8);
    this.corePointLight.position.set(-0.2, 0.45, 0);
    this.rootPivot.add(this.corePointLight);

    this.waveguides = new OpticalWaveguideSystem();
    this.rootPivot.add(this.waveguides.group);
    this.waveguides.applyPreset(this.activePreset);
  }

  private setupControls(domElement: HTMLCanvasElement) {
    const controls = new OrbitControls(this.camera, domElement);
    controls.enableDamping = true;
    controls.dampingFactor = 0.08;
    controls.enablePan = false;
    controls.minDistance = 2.6;
    controls.maxDistance = 8.4;
    controls.maxPolarAngle = Math.PI * 0.48;
    controls.minPolarAngle = 0.20;
    controls.target.copy(DEFAULT_LOOK_AT);
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
            if (!mat) return;

            if (mat.name === 'Platform_Ceramic') {
              this.platformCeramicMat = mat;
            } else if (mat.name === 'Chassis_Slate') {
              this.chassisSlateMat = mat;
            } else if (mat.name === 'Crystal_Lens') {
              // Crisp, zero-emissive physical lens: never turns milky white!
              mesh.material = new THREE.MeshPhysicalMaterial({
                name: 'Crystal_Lens_Clean',
                color: new THREE.Color(0xc8f0f6),
                emissive: new THREE.Color(0x000000),
                emissiveIntensity: 0.0,
                metalness: 0.0,
                roughness: 0.08,
                ior: 1.48,
                reflectivity: 0.45,
                envMapIntensity: 0.42,
                transparent: true,
                opacity: 0.34,
                depthWrite: false,
                side: THREE.FrontSide,
              });
              mesh.renderOrder = 3;
            } else if (mat.name.startsWith('Indicator_')) {
              mat.emissiveIntensity = this.siteTheme === 'dark' ? 0.95 : 0.35;
            }
          }
        });

        this.registryWheelOuter = this.namedModules.get('Registry_Wheel_Outer') || null;
        this.registryWheelInner = this.namedModules.get('Registry_Wheel_Inner') || null;
        this.crossbarYaw = this.namedModules.get('Crossbar_Gimbal_Yaw') || null;
        this.crossbarPitch = this.namedModules.get('Crossbar_Gimbal_Pitch') || null;
        this.vaultLockDial = this.namedModules.get('Vault_LockDial') || null;

        this.rootPivot.add(model);
        this.applySiteThemeColors();
        this.modelLoaded = true;
        this.container.dataset.glbLoaded = 'true';
      },
      undefined,
      (err) => {
        console.error('[PhotonicStage] Failed to load GLB asset:', err);
        this.webglReady = false;
        this.renderSvgFallback();
      },
    );
  }

  private applySiteThemeColors() {
    if (!this.webglReady || !this.renderer) return;

    const isLight = this.siteTheme === 'light';
    this.scene.environmentIntensity = isLight ? 0.38 : 0.62;
    this.renderer.toneMappingExposure = isLight ? 1.00 : 1.12;

    if (this.hemiLight) {
      this.hemiLight.color.setHex(isLight ? 0xf6feff : 0xbcebf3);
      this.hemiLight.groundColor.setHex(isLight ? 0x9cb9bf : 0x0a1416);
      this.hemiLight.intensity = isLight ? 1.05 : 0.70;
    }
    if (this.keyLight) {
      this.keyLight.intensity = isLight ? 1.45 : 1.85;
    }
    if (this.fillLight) {
      this.fillLight.intensity = isLight ? 0.65 : 1.05;
    }
    if (this.corePointLight) {
      this.corePointLight.intensity = isLight ? 0.85 : 2.1;
    }
    if (this.platformCeramicMat) {
      this.platformCeramicMat.color.setHex(isLight ? 0xd8e8eb : 0x1b2b2e);
      this.platformCeramicMat.roughness = isLight ? 0.44 : 0.36;
    }
    if (this.chassisSlateMat) {
      this.chassisSlateMat.color.setHex(isLight ? 0x304549 : 0x26393d);
    }
    if (this.gridHelper) {
      const mats = Array.isArray(this.gridHelper.material)
        ? this.gridHelper.material
        : [this.gridHelper.material];
      mats.forEach((m) => {
        m.opacity = isLight ? 0.22 : 0.30;
      });
    }
    this.waveguides?.setTheme(this.siteTheme);
  }

  private bindEvents() {
    window.addEventListener('resize', this.handleResize, { passive: true });
    this.container.addEventListener('pointermove', this.handlePointerMove, { passive: true });
    this.container.addEventListener('pointerleave', this.handlePointerLeave, { passive: true });

    if (typeof IntersectionObserver !== 'undefined') {
      this.intersectionObserver = new IntersectionObserver(
        (entries) => {
          const entry = entries[0];
          this.isInViewport = Boolean(entry && entry.isIntersecting);
        },
        { threshold: 0.05 },
      );
      this.intersectionObserver.observe(this.container);
    }

    this.renderer?.domElement.addEventListener('click', (ev) => {
      if (!this.renderer || !this.modelLoaded || this.freeInspectMode) return;
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
            cur.name === 'Module_KeychainVault'
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
    const rect = this.container.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    this.pointerNdc.x = ((e.clientX - rect.left) / rect.width) * 2 - 1;
    this.pointerNdc.y = -(((e.clientY - rect.top) / rect.height) * 2 - 1);
  };

  private handlePointerLeave = () => {
    this.pointerNdc.set(0, 0);
  };

  private handleResize = () => {
    const w = Math.max(1, this.container.clientWidth || 640);
    const h = Math.max(1, this.container.clientHeight || 380);
    this.isCompactViewport = w < 620;

    this.camera.fov = this.isCompactViewport ? 44 : 36;
    this.camera.aspect = w / h;
    this.camera.updateProjectionMatrix();

    if (this.renderer) {
      const maxDpr = this.isSoftwareGl ? 1.0 : this.isCompactViewport ? 1.5 : 2.0;
      this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, maxDpr));
      this.renderer.setSize(w, h);
    }
  };

  /* ------------------------------------------------------------------------ */
  /*  Public Controls                                                          */
  /* ------------------------------------------------------------------------ */

  public setSiteTheme(theme: 'light' | 'dark') {
    this.siteTheme = theme;
    if (this.webglReady) {
      this.applySiteThemeColors();
    } else {
      this.renderSvgFallback();
    }
  }

  public setModelPreset(preset: ModelRoutePreset) {
    this.activePreset = preset;
    this.targetGimbalYaw = preset.gimbalYawRad;
    this.targetGimbalPitch = preset.gimbalPitchRad;
    this.targetVaultAngle = preset.vaultActive ? Math.PI / 3 : 0;
    this.wheelSpinBoost = 2.8;

    if (this.corePointLight) {
      this.corePointLight.color.set(preset.waveHex);
    }
    this.waveguides?.applyPreset(preset);

    if (!this.webglReady) {
      this.renderSvgFallback();
    }
  }

  public triggerCatalogSyncEffect() {
    this.wheelSpinBoost = 4.6;
    this.waveguides?.triggerSyncPulse();
  }

  public setFreeInspectMode(enabled: boolean) {
    this.freeInspectMode = enabled;
    if (this.controls) {
      this.controls.enabled = enabled;
      if (enabled) {
        this.controls.target.copy(DEFAULT_LOOK_AT);
        this.controls.update();
      }
    }
  }

  public resetCameraView() {
    this.camera.position.copy(DEFAULT_CAM_POS);
    this.camera.lookAt(DEFAULT_LOOK_AT);
    if (this.controls) {
      this.controls.target.copy(DEFAULT_LOOK_AT);
      this.controls.update();
    }
  }

  /* ------------------------------------------------------------------------ */
  /*  Animation Loop                                                          */
  /* ------------------------------------------------------------------------ */

  private startLoop() {
    const tick = (nowMs: number) => {
      this.rafId = requestAnimationFrame(tick);
      if (document.hidden || !this.isInViewport) return;

      if (this.isSoftwareGl && nowMs - this.lastRenderMs < 48) {
        return;
      }
      this.lastRenderMs = nowMs;

      const elapsed = nowMs * 0.001;
      const dt = this.lastTimestamp ? Math.min(0.06, (nowMs - this.lastTimestamp) * 0.001) : 0.016;
      this.lastTimestamp = nowMs;

      this.stepSimulation(elapsed, dt);

      if (this.renderer) {
        this.renderer.render(this.scene, this.camera);
      }

      this.projectCalloutPins();
    };
    this.rafId = requestAnimationFrame(tick);
  }

  private stepSimulation(elapsed: number, dt: number) {
    // 1. Damped harmonic spring for Rotary Router Switch
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

    // 2. Catalog rings rotation
    this.wheelSpinBoost = Math.max(0, this.wheelSpinBoost - dt * 2.8);
    if (!this.reducedMotion) {
      const spinSpeed = 0.25 + this.wheelSpinBoost;
      if (this.registryWheelOuter) {
        this.registryWheelOuter.rotation.x += dt * spinSpeed;
      }
      if (this.registryWheelInner) {
        this.registryWheelInner.rotation.x -= dt * (spinSpeed * 1.3);
      }
    }

    // 3. Keychain Vault lock dial
    const vaultLerp = this.reducedMotion ? 1.0 : Math.min(1.0, dt * 7.5);
    this.currentVaultAngle += (this.targetVaultAngle - this.currentVaultAngle) * vaultLerp;
    if (this.vaultLockDial) {
      this.vaultLockDial.rotation.y = this.currentVaultAngle;
    }

    // 4. Subtle pointer tilt or OrbitControls damping
    this.smoothPointer.lerp(this.pointerNdc, Math.min(1.0, dt * 4.5));
    if (!this.freeInspectMode) {
      this.camera.position.lerp(DEFAULT_CAM_POS, Math.min(1.0, dt * 4.5));
      this.camera.lookAt(DEFAULT_LOOK_AT);
      if (!this.reducedMotion) {
        this.rootPivot.rotation.y = this.smoothPointer.x * 0.07;
        this.rootPivot.rotation.x = -this.smoothPointer.y * 0.035;
        this.rootPivot.position.y = Math.sin(elapsed * 1.1) * 0.015;
      } else {
        this.rootPivot.rotation.set(0, 0, 0);
        this.rootPivot.position.set(0, 0, 0);
      }
    } else {
      this.rootPivot.rotation.y *= 0.9;
      this.rootPivot.rotation.x *= 0.9;
      this.controls?.update();
    }

    // 5. Update 3D signal tracks & data beads
    this.waveguides?.update(elapsed, dt, this.reducedMotion);
  }

  private projectCalloutPins() {
    if (!this.onPinsUpdate || !this.renderer) return;
    const w = this.container.clientWidth || 640;
    const h = this.container.clientHeight || 380;
    const v = new THREE.Vector3();

    const pins: CalloutPinProjection[] = CALLOUT_ANCHORS.map((a) => {
      v.set(a.world[0], a.world[1], a.world[2]);
      this.rootPivot.localToWorld(v);
      v.project(this.camera);

      const sx = (v.x * 0.5 + 0.5) * w;
      const sy = (-v.y * 0.5 + 0.5) * h;
      const inBounds =
        v.z > 0 &&
        v.z < 1 &&
        sx >= 42 &&
        sx <= w - 42 &&
        sy >= 28 &&
        sy <= h - 28;

      const highlighted =
        (this.activePreset.routeKind === 'bridge' && a.id === 'pin-bridge') ||
        (this.activePreset.vaultActive && a.id === 'pin-vault') ||
        (!this.activePreset.vaultActive && a.id === 'pin-crossbar');

      return {
        id: a.id,
        label: a.label,
        sublabel: a.sublabel,
        x: sx,
        y: sy,
        visible: inBounds && !this.isCompactViewport,
        highlighted,
      };
    });

    this.onPinsUpdate(pins);
  }

  private renderSvgFallback() {
    const p = this.activePreset;
    const isLight = this.siteTheme === 'light';
    const isOfficial = p.routeKind === 'official';
    const isBridge = p.routeKind === 'bridge';
    const isLocal = p.routeKind === 'local';
    const bg = isLight ? '#eaf6f8' : '#131d1f';
    const strokeMuted = isLight ? '#9eb6ba' : '#354b4f';
    const nodeFill = isLight ? '#ffffff' : '#1b282b';
    const textFill = isLight ? '#151d1e' : '#d9e4e6';
    const accent = isLight ? p.accentLight : p.accentDark;

    this.container.dataset.glbLoaded = 'fallback';
    this.container.innerHTML = `
      <div class="photonic-svg-fallback" aria-hidden="true">
        <svg viewBox="0 0 720 340" fill="none" xmlns="http://www.w3.org/2000/svg">
          <rect x="20" y="20" width="680" height="300" rx="22" fill="${bg}" stroke="${strokeMuted}" stroke-width="1.5"/>
          <path d="M76 170 H275" stroke="${accent}" stroke-width="4" stroke-linecap="round"/>
          <path d="M275 170 L355 86 H640" stroke="${isOfficial ? accent : strokeMuted}" stroke-width="${isOfficial ? 4 : 2}" stroke-opacity="${isOfficial ? 1 : 0.45}"/>
          <path d="M275 170 H640" stroke="${isBridge ? accent : strokeMuted}" stroke-width="${isBridge ? 4 : 2}" stroke-opacity="${isBridge ? 1 : 0.45}"/>
          <path d="M275 170 L355 254 H640" stroke="${isLocal ? accent : strokeMuted}" stroke-width="${isLocal ? 4 : 2}" stroke-opacity="${isLocal ? 1 : 0.45}"/>
          <circle cx="155" cy="170" r="24" fill="${nodeFill}" stroke="${accent}" stroke-width="2"/>
          <circle cx="275" cy="170" r="30" fill="${nodeFill}" stroke="${accent}" stroke-width="2.5"/>
          <rect x="405" y="146" width="110" height="48" rx="12" fill="${nodeFill}" stroke="${isBridge ? accent : strokeMuted}" stroke-width="1.8"/>
          <text x="155" y="216" text-anchor="middle" font-size="12" fill="${textFill}">Codex 入口</text>
          <text x="275" y="220" text-anchor="middle" font-size="12" fill="${textFill}">按轮分流</text>
          <text x="580" y="76" text-anchor="middle" font-size="12" fill="${textFill}">官方直连</text>
          <text x="580" y="160" text-anchor="middle" font-size="12" fill="${textFill}">协议转换</text>
          <text x="580" y="244" text-anchor="middle" font-size="12" fill="${textFill}">本机离线</text>
        </svg>
      </div>
    `;
  }

  public dispose() {
    cancelAnimationFrame(this.rafId);
    this.intersectionObserver?.disconnect();
    window.removeEventListener('resize', this.handleResize);
    this.container.removeEventListener('pointermove', this.handlePointerMove);
    this.container.removeEventListener('pointerleave', this.handlePointerLeave);
    this.renderer?.dispose();
  }
}
