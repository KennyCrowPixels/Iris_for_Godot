export type ThemeName = "Black" | "Blue" | "Pink" | "Purple" | "Silver" | "Grey" | "Custom";

const accents: Record<Exclude<ThemeName, "Custom">, string> = {
  Black: "#64d6b3",
  Blue: "#75b2ff",
  Pink: "#f49fc3",
  Purple: "#b7a4fa",
  Silver: "#becedb",
  Grey: "#b4c9bc",
};

const darkBases: Record<Exclude<ThemeName, "Custom">, string> = {
  Black: "#25282d",
  Blue: "#293b50",
  Pink: "#3e303b",
  Purple: "#37324a",
  Silver: "#3b4650",
  Grey: "#363a38",
};

function mix(first: string, second: string, ratio: number): string {
  const channels = [1, 3, 5].map((position) => {
    const left = parseInt(first.slice(position, position + 2), 16);
    const right = parseInt(second.slice(position, position + 2), 16);
    return Math.round(left + (right - left) * ratio).toString(16).padStart(2, "0");
  });
  return `#${channels.join("")}`;
}

function luminance(color: string): number {
  const channels = [1, 3, 5].map((offset) => parseInt(color.slice(offset, offset + 2), 16) / 255)
    .map((channel) => channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);
  return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
}

export function themeTokens(theme: ThemeName, custom: string, light: boolean): Record<string, string> {
  const seed = theme === "Custom" ? (/^#[\da-f]{6}$/i.test(custom) ? custom : "#64d6b3") : accents[theme];
  const accent = light ? mix(seed, "#0c1820", 0.54) : mix(seed, "#ffffff", 0.12);
  const tint = theme === "Black" ? "#90949a" : theme === "Silver" ? "#94b4ca" : theme === "Grey" ? "#a4b0a7" : seed;
  const background = light ? mix("#eef1f4", tint, 0.18)
    : theme === "Custom" ? mix("#242a30", seed, 0.14) : darkBases[theme];
  const surface = mix(background, "#ffffff", light ? 0.45 : 0.045);
  const panel = mix(background, light ? "#ffffff" : tint, light ? 0.22 : 0.04);
  const accentRgb = [1, 3, 5].map((position) => parseInt(accent.slice(position, position + 2), 16)).join(", ");
  return {
    "--app-bg": background,
    "--app-gradient-start": mix(background, tint, light ? 0.18 : 0.20),
    "--app-gradient-end": mix(background, light ? "#d9e4ed" : "#191c24", 0.35),
    "--app-surface": surface,
    "--app-panel-bg": panel,
    "--app-border": mix(background, light ? "#425065" : "#d6dde4", light ? 0.24 : 0.20),
    "--app-input-bg": mix(background, light ? "#ffffff" : "#111519", light ? 0.6 : 0.16),
    "--app-tab-bg": "transparent",
    "--app-tab-active-bg": surface,
    "--app-user-bubble": light ? mix(surface, tint, 0.12) : mix(surface, tint, 0.08),
    "--app-text": light ? "#202a34" : "#f0f3f6",
    "--app-muted": light ? "#414e5b" : "#cbd2d9",
    "--app-accent": accent,
    "--app-accent-rgb": accentRgb,
    "--app-accent-ink": luminance(accent) > 0.179 ? "#000000" : "#ffffff",
    "--app-glass": `${panel}f2`,
    "--app-shadow": light ? "0 16px 48px rgba(25,35,48,0.13)" : "0 16px 48px rgba(0,0,0,0.32)",
  };
}