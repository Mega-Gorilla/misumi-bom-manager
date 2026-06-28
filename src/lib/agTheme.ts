import { themeQuartz } from "ag-grid-community";

// Compact, branded theme for the BOM grid (Theming API, AG Grid v33+).
export const bomTheme = themeQuartz.withParams({
  accentColor: "#0b62d6",
  spacing: 6, // denser rows than the default
  fontSize: 13,
  headerFontWeight: 600,
  oddRowBackgroundColor: "#f8f9fb", // zebra striping
});
