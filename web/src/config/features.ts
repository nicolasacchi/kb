// Feature flags. Updated per milestone — flip true when the feature
// lands. v0.1 had comments off; v0.2 turns it on (B4); v0.3 flips
// atlasUmap (E5: AtlasView reads atlas_x/y/cluster from the daemon).
export const features = {
  comments: true,
  atlasUmap: true,
  multiCardVariants: false,
  multiChromeVariants: false,
} as const;

export type FeatureFlag = keyof typeof features;
