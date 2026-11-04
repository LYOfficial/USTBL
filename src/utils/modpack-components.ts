/** Names of the component uids a pack chain commonly carries; they are not translated. */
const COMPONENT_NAMES: Record<string, string> = {
  "net.minecraft": "Minecraft",
  "net.minecraftforge": "Forge",
  "net.neoforged": "NeoForge",
  "net.fabricmc.fabric-loader": "Fabric",
  "org.quiltmc.quilt-loader": "Quilt",
  "net.fabricmc.intermediary": "Intermediary Mappings",
  "org.quiltmc.hashed": "Hashed Mappings",
  "org.lwjgl": "LWJGL 2",
  "org.lwjgl3": "LWJGL 3",
  "me.eigenraven.lwjgl3ify.forgepatches": "lwjgl3ify",
};

/** The name shown for a component uid; an unknown uid is shown as it is. */
export const componentDisplayName = (uid: string) =>
  COMPONENT_NAMES[uid] ?? uid;
