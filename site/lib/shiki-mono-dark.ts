import type { RawThemeSetting, ThemeRegistrationRaw } from "shiki"

/**
 * Strict-monochrome Shiki theme that maps syntax tokens onto the site's gray
 * ramp from `app/globals.css`. Differentiation is by *value* (and weight), not
 * hue — see `Shelbi/Plans/shelbi-website.md` §3. Vesper highlighted bash with
 * vivid cyan-teal (`#99FFE4`) and warm peach (`#FFC799`); both read as accent
 * colors on a no-hue surface, so we ship our own.
 *
 * Background matches `--color-gray-1` (the CodeBlock wrapper surface) so the
 * pre's inline `background-color` lines up with the chrome around it.
 */
const FG = "#FFFFFF"
const STRING = "#999999"
const COMMENT = "#666666"
const PUNCT = "#666666"
const MUTED = "#999999"
const BG = "#0A0A0A"

// Token rules — shared by `settings` (the Shiki-native field) and
// `tokenColors` (the VSCode-JSON alias). Both are populated because
// `rehype-pretty-code`'s `isJSONTheme` heuristic only treats a theme as a
// single JSON theme when `tokenColors` is present; without it the theme
// object is mis-detected as a multi-theme map and the cold Vercel build
// fails with "Theme `shelbi-mono-dark` is not included in this bundle".
// Shiki's `normalizeTheme` reads `settings` first, so behavior is unchanged.
const TOKEN_RULES: RawThemeSetting[] = [
  { settings: { foreground: FG } },
  {
    scope: ["comment", "string.quoted.docstring.multi", "punctuation.definition.comment"],
    settings: { foreground: COMMENT, fontStyle: "italic" },
  },
  {
    scope: [
      "string",
      "string.quoted",
      "string.template",
      "string.interpolated",
      "string.regexp",
      "string.unquoted",
      "markup.fenced_code",
      "markup.inline",
    ],
    settings: { foreground: STRING },
  },
  {
    scope: [
      "keyword.control",
      "keyword.other",
      "keyword.operator.new",
      "storage.type",
      "storage.modifier",
      "support.function.builtin",
    ],
    settings: { foreground: FG, fontStyle: "bold" },
  },
  {
    scope: [
      "entity.name.function",
      "support.function",
      "meta.function-call",
      "entity.name.type",
      "entity.other.inherited-class",
    ],
    settings: { foreground: FG, fontStyle: "bold" },
  },
  {
    scope: [
      "constant.numeric",
      "constant.language",
      "constant.character",
      "constant.other",
      "variable.language",
    ],
    settings: { foreground: FG },
  },
  {
    scope: [
      "variable",
      "variable.parameter",
      "variable.other",
      "meta.property-name",
      "support.variable",
    ],
    settings: { foreground: FG },
  },
  {
    scope: [
      "punctuation",
      "punctuation.separator",
      "punctuation.terminator",
      "punctuation.definition",
      "keyword.operator",
      "meta.brace",
    ],
    // `fontStyle: ""` resets weight in case a broader scope rule above set
    // bold — `keyword.operator.pipe.shell` matches both `keyword.other` and
    // `keyword.operator` and textmate stacks settings unless we clear them.
    settings: { foreground: PUNCT, fontStyle: "" },
  },
  {
    scope: ["entity.name.tag.yaml", "entity.name.tag", "support.type.property-name"],
    settings: { foreground: FG, fontStyle: "bold" },
  },
  {
    scope: ["entity.other.attribute-name"],
    settings: { foreground: MUTED },
  },
  {
    scope: ["markup.bold", "markup.heading"],
    settings: { foreground: FG, fontStyle: "bold" },
  },
  {
    scope: ["markup.italic"],
    settings: { fontStyle: "italic" },
  },
  {
    scope: ["markup.underline.link", "string.other.link"],
    settings: { foreground: FG, fontStyle: "underline" },
  },
  {
    scope: ["markup.inserted"],
    settings: { foreground: FG, fontStyle: "bold" },
  },
  {
    scope: ["markup.deleted"],
    settings: { foreground: COMMENT, fontStyle: "italic" },
  },
  {
    scope: ["invalid", "invalid.illegal"],
    settings: { foreground: FG, fontStyle: "underline" },
  },
]

export const shelbiMonoDark: ThemeRegistrationRaw = {
  name: "shelbi-mono-dark",
  type: "dark",
  colors: {
    "editor.background": BG,
    "editor.foreground": FG,
  },
  fg: FG,
  bg: BG,
  settings: TOKEN_RULES,
  tokenColors: TOKEN_RULES,
}
