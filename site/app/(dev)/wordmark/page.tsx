import { BlockDivider, BlockMark, Wordmark, type WordmarkSize } from "@/components/Wordmark"

const SIZES: { size: WordmarkSize; height: number; label: string }[] = [
  { size: "hero", height: 120, label: "hero — 120px (landing hero)" },
  { size: "lg", height: 80, label: "lg — 80px (section dividers)" },
  { size: "md", height: 48, label: "md — 48px (default)" },
  { size: "sm", height: 28, label: "sm — 28px (small nav / inline)" },
]

export default function WordmarkPreviewPage() {
  return (
    <main className="min-h-screen bg-bg text-fg font-mono">
      <div className="mx-auto max-w-7xl px-3 py-6 space-y-8">
        <header className="space-y-2">
          <h1 className="text-2xl font-semibold">Wordmark</h1>
          <p className="text-gray-7 font-sans">
            Block-character SHELBI wordmark + reusable block-motif assets.
            Identity-carrier for the strict-mono palette.
          </p>
        </header>

        <BlockDivider />

        <section className="space-y-4">
          <h2 className="text-sm uppercase tracking-widest text-gray-6">Sizes</h2>
          <div className="space-y-6">
            {SIZES.map(({ size, label }) => (
              <div key={size} className="space-y-2">
                <div className="text-xs text-gray-6 font-sans">{label}</div>
                <Wordmark size={size} />
              </div>
            ))}
          </div>
        </section>

        <BlockDivider />

        <section className="space-y-4">
          <h2 className="text-sm uppercase tracking-widest text-gray-6">
            On inverted surface
          </h2>
          <p className="text-xs text-gray-6 font-sans">
            Wordmark uses <code>currentColor</code>, so it inherits the text
            color of its container. On white, set the wrapper to{" "}
            <code>text-bg</code>.
          </p>
          <div className="bg-fg text-bg p-4 rounded-sm">
            <Wordmark size="md" />
          </div>
        </section>

        <BlockDivider />

        <section className="space-y-4">
          <h2 className="text-sm uppercase tracking-widest text-gray-6">
            Semantic element
          </h2>
          <p className="text-xs text-gray-6 font-sans">
            Pass <code>as=&quot;h1&quot;</code> for the landing hero — the
            visible mark is decorative, the <code>title</code> prop provides
            the accessible name via an <code>sr-only</code> span.
          </p>
          <Wordmark as="h1" size="hero" title="shelbi" />
        </section>

        <BlockDivider />

        <section className="space-y-4">
          <h2 className="text-sm uppercase tracking-widest text-gray-6">
            BlockMark (square favicon glyph)
          </h2>
          <div className="flex items-end gap-4">
            <div className="space-y-1">
              <BlockMark size={16} />
              <div className="text-[10px] text-gray-6 font-sans">16</div>
            </div>
            <div className="space-y-1">
              <BlockMark size={32} />
              <div className="text-[10px] text-gray-6 font-sans">32</div>
            </div>
            <div className="space-y-1">
              <BlockMark size={64} />
              <div className="text-[10px] text-gray-6 font-sans">64</div>
            </div>
            <div className="space-y-1">
              <BlockMark size={128} />
              <div className="text-[10px] text-gray-6 font-sans">128</div>
            </div>
          </div>
        </section>

        <BlockDivider />

        <section className="space-y-4">
          <h2 className="text-sm uppercase tracking-widest text-gray-6">
            BlockDivider variants
          </h2>
          <div className="space-y-6">
            <div className="space-y-2">
              <div className="text-xs text-gray-6 font-sans">
                default — 32 cells, 8px tall, 0.5 gap
              </div>
              <BlockDivider />
            </div>
            <div className="space-y-2">
              <div className="text-xs text-gray-6 font-sans">
                dense — 64 cells, 4px tall, 0.25 gap
              </div>
              <BlockDivider cells={64} height={4} gapRatio={0.25} />
            </div>
            <div className="space-y-2">
              <div className="text-xs text-gray-6 font-sans">
                bold — 12 cells, 16px tall, 1.0 gap
              </div>
              <BlockDivider cells={12} height={16} gapRatio={1} />
            </div>
            <div className="space-y-2">
              <div className="text-xs text-gray-6 font-sans">
                muted color via text-gray-5
              </div>
              <div className="text-gray-5">
                <BlockDivider />
              </div>
            </div>
          </div>
        </section>
      </div>
    </main>
  )
}
