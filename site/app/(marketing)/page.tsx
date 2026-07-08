import { FeatureGrid } from '@/components/FeatureGrid'
import { Hero } from '@/components/Hero'
import { HeroAnimation } from '@/components/HeroAnimation'
import { InstallCloser } from '@/components/InstallCloser'
import { ProblemSection } from '@/components/ProblemSection'
import { SolutionIntro } from '@/components/SolutionIntro'
import { ValueProps } from '@/components/ValueProps'

export default function HomePage() {
  return (
    <div className="flex min-h-screen flex-col">
      <main className="flex flex-1 flex-col">
        <Hero />
        <HeroAnimation />
        <ProblemSection />
        <SolutionIntro />
        <ValueProps />
        <FeatureGrid />
        <InstallCloser />
      </main>
    </div>
  )
}
