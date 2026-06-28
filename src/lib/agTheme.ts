import {
  themeQuartz,
  themeBalham,
  themeMaterial,
  colorSchemeDark,
  type Theme,
} from "ag-grid-community";

export type ThemeId = "quartz" | "balham" | "material";

export const THEME_OPTIONS: { id: ThemeId; label: string }[] = [
  { id: "quartz", label: "Quartz（モダン・既定）" },
  { id: "balham", label: "Balham（密・表計算風）" },
  { id: "material", label: "Material" },
];

const BASES: Record<ThemeId, Theme> = {
  quartz: themeQuartz,
  balham: themeBalham,
  material: themeMaterial,
};

const cache = new Map<string, Theme>();

/** Resolve a built-in base theme + light/dark, with our brand params applied. Cached. */
export function resolveTheme(id: ThemeId, dark: boolean): Theme {
  const key = `${id}:${dark ? "dark" : "light"}`;
  const cached = cache.get(key);
  if (cached) return cached;

  let theme = BASES[id] ?? themeQuartz;
  if (dark) theme = theme.withPart(colorSchemeDark);
  theme = theme.withParams({
    accentColor: "#0b62d6",
    spacing: 6,
    fontSize: 13,
    headerFontWeight: 600,
    ...(dark ? {} : { oddRowBackgroundColor: "#f8f9fb" }),
  });

  cache.set(key, theme);
  return theme;
}
