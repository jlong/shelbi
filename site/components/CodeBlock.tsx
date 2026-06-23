import { codeToHtml } from "shiki"
import { shelbiMonoDark } from "@/lib/shiki-mono-dark"
import { shelbiMonoLight } from "@/lib/shiki-mono-light"
import { CopyButton } from "./CopyButton"

type CodeBlockProps = {
  code: string
  lang?: string
}

export async function CodeBlock({ code, lang = "bash" }: CodeBlockProps) {
  // Dual-theme highlight. Shiki emits inline styles for the `dark` theme
  // (our brand default) and CSS variables (`--shiki-light*`) for `light`;
  // the swap to light values when `.dark` is absent is handled in
  // `app/globals.css`.
  const html = await codeToHtml(code, {
    lang,
    themes: {
      dark: shelbiMonoDark,
      light: shelbiMonoLight,
    },
    defaultColor: "dark",
  })

  return (
    <div className="group relative overflow-hidden rounded-md border border-gray-4 bg-gray-1">
      <div
        className="overflow-x-auto px-3 py-3 font-mono text-sm leading-relaxed [&_pre]:!bg-transparent [&_pre]:!p-0 [&_code]:font-mono"
        dangerouslySetInnerHTML={{ __html: html }}
      />
      <CopyButton text={code} />
    </div>
  )
}
