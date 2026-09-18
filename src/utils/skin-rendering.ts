import * as skinview3d from "skinview3d";
import {
  CircleGeometry,
  DirectionalLight,
  HemisphereLight,
  Mesh,
  MeshStandardMaterial,
  NoToneMapping,
  PCFSoftShadowMap,
  ShadowMaterial,
} from "three";
import { SSAOPass } from "three/addons/postprocessing/SSAOPass.js";

const forEachPlayerMaterial = (
  viewer: skinview3d.SkinViewer,
  callback: (material: MeshStandardMaterial) => void
) => {
  viewer.playerObject.traverse((object) => {
    if (!(object instanceof Mesh)) return;

    object.castShadow = true;
    object.receiveShadow = true;
    const materials = Array.isArray(object.material)
      ? object.material
      : [object.material];
    materials.forEach((material) => {
      if (material instanceof MeshStandardMaterial) callback(material);
    });
  });
};

export const refreshEnhancedSkinMaterials = (viewer: skinview3d.SkinViewer) => {
  forEachPlayerMaterial(viewer, (material) => {
    material.roughness = 0.78;
    material.metalness = 0;
    material.needsUpdate = true;
  });
};

export const configureEnhancedSkinRendering = (
  viewer: skinview3d.SkinViewer
) => {
  const { renderer, scene, composer } = viewer;

  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = PCFSoftShadowMap;
  renderer.toneMapping = NoToneMapping;
  renderer.toneMappingExposure = 1;

  viewer.globalLight.intensity = 2.05;
  viewer.cameraLight.intensity = 1.05;
  viewer.cameraLight.position.set(2, 4, 8);

  const hemisphereLight = new HemisphereLight(0xffffff, 0x555555, 0.5);

  const keyLight = new DirectionalLight(0xffffff, 1.25);
  keyLight.position.set(-18, 26, 24);
  keyLight.castShadow = true;
  keyLight.shadow.mapSize.set(512, 512);
  keyLight.shadow.camera.near = 1;
  keyLight.shadow.camera.far = 90;
  keyLight.shadow.camera.left = -22;
  keyLight.shadow.camera.right = 22;
  keyLight.shadow.camera.top = 22;
  keyLight.shadow.camera.bottom = -22;
  keyLight.shadow.bias = -0.0006;
  keyLight.shadow.normalBias = 0.035;

  const rimLight = new DirectionalLight(0xffffff, 0.25);
  rimLight.position.set(18, 10, -20);

  const groundGeometry = new CircleGeometry(10, 32);
  const groundMaterial = new ShadowMaterial({
    color: 0x222222,
    opacity: 0.14,
    transparent: true,
    depthWrite: false,
  });
  const ground = new Mesh(groundGeometry, groundMaterial);
  ground.rotation.x = -Math.PI / 2;
  ground.position.y = -16.15;
  ground.receiveShadow = true;

  scene.add(hemisphereLight, keyLight, keyLight.target, rimLight, ground);

  const ssaoPass = new SSAOPass(
    scene,
    viewer.camera,
    viewer.width,
    viewer.height,
    8
  );
  ssaoPass.kernelRadius = 2.5;
  ssaoPass.minDistance = 0.002;
  ssaoPass.maxDistance = 0.04;
  composer.insertPass(ssaoPass, 1);

  refreshEnhancedSkinMaterials(viewer);

  return () => {
    composer.removePass(ssaoPass);
    (
      ssaoPass as SSAOPass & {
        dispose: () => void;
      }
    ).dispose();
    scene.remove(hemisphereLight, keyLight, keyLight.target, rimLight, ground);
    groundGeometry.dispose();
    groundMaterial.dispose();
  };
};
