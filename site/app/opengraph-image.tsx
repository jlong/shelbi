import { ImageResponse } from "next/og"
import { readFile } from "node:fs/promises"
import { join } from "node:path"
import { OG_CARD_SIZE, OgCard } from "@/components/OgCard"

export const alt =
  "Do more with your agents — an open source, multi-machine orchestrator built on tmux"
export const size = OG_CARD_SIZE
export const contentType = "image/png"

const TITLE = "Do more with your agents"

export default async function Image() {
  const [geistRegular, geistSemiBold, sourceCodeProRegular] = await Promise.all([
    readFile(
      join(process.cwd(), "node_modules/geist/dist/fonts/geist-sans/Geist-Regular.ttf"),
    ),
    readFile(
      join(process.cwd(), "node_modules/geist/dist/fonts/geist-sans/Geist-SemiBold.ttf"),
    ),
    readFile(join(process.cwd(), "public/fonts/SourceCodePro-Regular.ttf")),
  ])

  return new ImageResponse(<OgCard title={TITLE} />, {
    ...size,
    fonts: [
      { name: "Geist", data: geistRegular, style: "normal", weight: 400 },
      { name: "Geist", data: geistSemiBold, style: "normal", weight: 600 },
      { name: "Source Code Pro", data: sourceCodeProRegular, style: "normal", weight: 400 },
    ],
  })
}
