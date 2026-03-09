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
}

export enum DependencyType {
  Required = "required",
  Optional = "optional",
  Incompatible = "incompatible",
  Embedded = "embedded",
  Tool = "tool",
  Include = "include",
}

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
};

export const worldTagList = {
  Modrinth: [],
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
};

export const sortByLists = {
  Modrinth: ["relevance", "downloads", "follows", "updated", "newest"],
};
