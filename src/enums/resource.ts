export enum OtherResourceType {
  Mod = "mod",
  World = "world",
  ResourcePack = "resourcepack",
  ShaderPack = "shader",
  DataPack = "datapack",
  ModPack = "modpack",
}

export enum OtherResourceSource {
  Modrinth = "Modrinth",
  CurseForge = "CurseForge",
}

export enum DependencyType {
  Required = "required",
  Optional = "optional",
  Incompatible = "incompatible",
  Embedded = "embedded",
  Tool = "tool",
  Include = "include",
}

/// Tag vocabulary for the CurseForge channel. Keys mirror CurseForge category
/// names, which is also how they are translated and how the backend resolves
/// them to category ids at query time, so a category that disappears upstream is
/// simply skipped instead of breaking the search.
export const curseForgeModTagList: Record<string, string[]> = {
  All: [],
  Addons: [],
  "Adventure and RPG": [],
  "API and Library": [],
  "Applied Energistics 2": [],
  "Armor, Tools, and Weapons": [],
  Automation: [],
  Biomes: [],
  "Blood Magic": [],
  Buildcraft: [],
  "Bug Fixes": [],
  Cosmetic: [],
  CraftTweaker: [],
  Create: [],
  Dimensions: [],
  Education: [],
  Energy: [],
  "Energy, Fluid, and Item Transport": [],
  Farming: [],
  Food: [],
  Forestry: [],
  Galacticraft: [],
  Genetics: [],
  "Industrial Craft": [],
  "Integrated Dynamics": [],
  KubeJS: [],
  Magic: [],
  "Map and Information": [],
  MCreator: [],
  Miscellaneous: [],
  Mobs: [],
  "Ores and Resources": [],
  Performance: [],
  "Player Transport": [],
  Processing: [],
  Redstone: [],
  "Server Utility": [],
  Skyblock: [],
  Storage: [],
  Structures: [],
  Technology: [],
  Thaumcraft: [],
  "Thermal Expansion": [],
  "Tinker's Construct": [],
  "Twilight Forest": [],
  "Twitch Integration": [],
  "Utility & QoL": [],
  "World Gen": [],
};

export const curseForgeWorldTagList: Record<string, string[]> = {
  All: [],
  Types: [
    "Adventure",
    "Creation",
    "Game Map",
    "Modded World",
    "Parkour",
    "Puzzle",
    "Survival",
  ],
};

export const curseForgeResourcePackTagList: Record<string, string[]> = {
  All: [],
  Resolution: ["16x", "32x", "64x", "128x", "256x", "512x and Higher"],
  Styles: [
    "Animated",
    "Data Packs",
    "Font Packs",
    "Medieval",
    "Miscellaneous",
    "Mod Support",
    "Modern",
    "Photo Realistic",
    "Steampunk",
    "Traditional",
  ],
};

export const curseForgeShaderPackTagList: Record<string, string[]> = {
  All: [],
  Styles: ["Fantasy", "Realistic", "Vanilla"],
};

export const curseForgeModpackTagList: Record<string, string[]> = {
  All: [],
  Styles: [
    "Adventure and RPG",
    "Combat / PvP",
    "Exploration",
    "Extra Large",
    "FTB Official Pack",
    "Hardcore",
    "Horror",
    "Magic",
    "Map Based",
    "Mini Game",
    "Multiplayer",
    "Quests",
    "Sci-Fi",
    "Skyblock",
    "Small / Light",
    "Tech",
    "Vanilla+",
  ],
};

export const curseForgeDatapackTagList: Record<string, string[]> = {
  All: [],
  Styles: [
    "Magic",
    "Miscellaneous",
    "Fantasy",
    "Mod Support",
    "Tech",
    "Library",
    "Utility",
    "Adventure",
  ],
};

export const modTagList = {
  Modrinth: {
    All: ["All"],
    adventure: ["equipment", "cursed", "mobs", "magic"],
    utility: [
      "decoration",
      "economy",
      "food",
      "game-mechanics",
      "library",
      "management",
      "minigame",
      "optimization",
      "social",
    ],
    technology: ["worldgen", "storage", "transportation"],
  },
  CurseForge: curseForgeModTagList,
};

export const worldTagList = {
  Modrinth: [],
  CurseForge: curseForgeWorldTagList,
};

export const resourcePackTagList = {
  Modrinth: {
    All: ["All"],
    Resolution: ["8x-", "16x", "32x", "64x", "128x", "256x", "512x+"],
    Styles: [
      "audio",
      "blocks",
      "core-shaders",
      "entities",
      "environment",
      "equipment",
      "fonts",
      "gui",
      "items",
      "locale",
      "models",
      "combat",
      "cursed",
      "decoration",
      "modded",
      "realistic",
      "simplistic",
      "themed",
      "tweaks",
      "utility",
      "vanilla-like",
    ],
  },
  CurseForge: curseForgeResourcePackTagList,
};

export const shaderPackTagList = {
  Modrinth: {
    All: ["All"],
    Styles: [
      "cartoon",
      "cursed",
      "fantasy",
      "realistic",
      "semi-realistic",
      "vanilla-like",
      "atmosphere",
      "bloom",
      "colored-lighting",
      "foliage",
      "path-tracing",
      "pbr",
      "reflections",
      "shadows",
      "potato",
    ],
    performance: ["low", "medium", "high", "screenshot"],
  },
  CurseForge: curseForgeShaderPackTagList,
};

export const datapackTagList = {
  Modrinth: {
    All: ["All"],
    styles: [
      "adventure",
      "cursed",
      "decoration",
      "economy",
      "equipment",
      "food",
      "game-mechanics",
      "library",
      "magic",
      "management",
      "minigame",
      "mobs",
      "optimization",
      "social",
      "storage",
      "technology",
      "transportation",
      "utility",
      "worldgen",
    ],
  },
  CurseForge: curseForgeDatapackTagList,
};

export const modpackTagList = {
  Modrinth: {
    All: ["All"],
    styles: [
      "adventure",
      "challenging",
      "combat",
      "kitchen-sink",
      "lightweight",
      "magic",
      "multiplayer",
      "optimization",
      "quests",
      "technology",
    ],
  },
  CurseForge: curseForgeModpackTagList,
};

export const sortByLists = {
  Modrinth: ["relevance", "downloads", "follows", "updated", "newest"],
  CurseForge: [
    "Popularity",
    "Latest update",
    "Creation date",
    "Total downloads",
    "A-Z",
  ],
};
