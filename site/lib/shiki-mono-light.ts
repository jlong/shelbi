import type { RawThemeSetting, ThemeRegistrationRaw } from "shiki"

/**
 * Light-mode counterpart to `shelbi-mono-dark`. Same syntax token rules,
 * inverted value ramp: white background, black foreground, descending grays
 * for strings/comments/punctuation. Differentiation is still by *value*
 * (and weight), not hue, so the strict-mono commitment holds in both modes.
 *
 * Background matches `--color-gray-1` (light: `#FAFAFA`) so the pre's inline
 * `background-color` lines up with the CodeBlock chrome.
 */
const FG = "#000000"
const STRING = "#333333"
const COMMENT = "#666666"
const PUNCT = "#666666"
const MUTED = "#333333"
const BG = "#FAFAFA"

// Token rules — shared by `settings` and `tokenColors`. See the dark theme
// for why both fields are populated.
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

export const shelbiMonoLight: ThemeRegistrationRaw = {
  name: "shelbi-mono-light",
  type: "light",
  colors: {
    "editor.background": BG,
    "editor.foreground": FG,
  },
  fg: FG,
  bg: BG,
  settings: TOKEN_RULES,
  tokenColors: TOKEN_RULES,
}
