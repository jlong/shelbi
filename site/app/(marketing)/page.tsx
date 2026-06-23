import {
  ArrowRightIcon,
  BoltIcon,
  CheckCircleIcon,
  ClipboardDocumentListIcon,
  CommandLineIcon,
  CpuChipIcon,
  EyeIcon,
  ListBulletIcon,
  ServerStackIcon,
  ViewColumnsIcon,
} from '@heroicons/react/24/outline'
import Link from 'next/link'
import { Footer } from '@/components/Footer'
import { InstallCloser } from '@/components/InstallCloser'

const pitch = [
  {
    icon: ClipboardDocumentListIcon,
    title: 'Plan tasks',
    body: 'Capture work as small, well-scoped tasks on a Kanban board. Triage them from backlog into todo when they are ready, set priorities, and route work to specific machines or workers.',
    href: '/docs/getting-started/first-task',
  },
  {
    icon: CpuChipIcon,
    title: 'Workers do them',
    body: 'A fixed pool of long-lived workers picks up ready tasks automatically. Each worker holds one task at a time, in its own persistent git worktree, on the machine you assigned.',
    href: '/docs/concepts/workers',
  },
  {
    icon: CheckCircleIcon,
    title: 'You review and merge',
    body: 'When a worker reports done its card lands in review. Inspect the branch in the review pane, accept or send back for changes, then merge into main on your terms.',
    href: '/docs/concepts/columns',
  },
]

const features = [
  {
    icon: ViewColumnsIcon,
    label: 'Kanban board',
    body: 'Five columns — backlog, todo, in_progress, review, done — operated from a built-in TUI or the CLI.',
  },
  {
    icon: ServerStackIcon,
    label: 'Multi-machine workers',
    body: 'Declare workers across as many machines as you have; each one runs in its own persistent git worktree.',
  },
  {
    icon: BoltIcon,
    label: 'Orchestrator auto-dispatch',
    body: 'An orchestrator agent watches the events log and assigns ready tasks to free workers without prompting.',
  },
  {
    icon: EyeIcon,
    label: 'Review flow',
    body: 'Finished work surfaces as a review card; inspect the branch, accept it, send it back, or merge on your terms.',
  },
  {
    icon: ListBulletIcon,
    label: 'Events log',
    body: 'Every state change emits one line into ~/.shelbi/events.log — tail it, grep it, audit it after the fact.',
  },
  {
    icon: CommandLineIcon,
    label: 'tmux-native',
    body: 'Workers live in tmux panes — sessions persist across SSH drops and survive client restarts without losing context.',
  },
]

export default function HomePage() {
  return (
    <div className="flex min-h-screen flex-col">
      <main className="flex flex-1 flex-col">
        <section className="flex flex-1 items-center justify-center p-3">
          <div className="space-y-2">
            <h1 className="font-mono text-3xl font-semibold text-fg">shelbi</h1>
            <p className="text-gray-7">
              A Kanban board for orchestrating fleets of coding agents.
            </p>
          </div>
        </section>

        <section className="border-t border-gray-4">
          <div className="grid grid-cols-1 md:grid-cols-3 gap-px bg-gray-4">
            {pitch.map((item) => (
              <Link
                key={item.title}
                href={item.href}
                className="group flex flex-col gap-2 bg-bg p-4 outline outline-1 -outline-offset-1 outline-transparent transition-[outline-color] hover:outline-fg"
              >
                <item.icon className="w-3 h-3 text-fg" aria-hidden="true" />
                <h3 className="text-xl font-semibold text-fg">{item.title}</h3>
                <p className="text-gray-7 leading-relaxed">{item.body}</p>
                <span className="mt-auto inline-flex items-center gap-1 pt-2 text-sm text-gray-7 group-hover:font-semibold group-hover:text-fg">
                  Learn more
                  <ArrowRightIcon className="w-2 h-2" aria-hidden="true" />
                </span>
              </Link>
            ))}
          </div>
        </section>

        <section className="border-t border-b border-gray-4">
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-px bg-gray-4">
            {features.map((feature) => (
              <div
                key={feature.label}
                className="flex flex-col gap-1 bg-bg p-3"
              >
                <feature.icon className="w-3 h-3 text-fg" aria-hidden="true" />
                <h3 className="mt-1 text-base font-semibold text-fg">
                  {feature.label}
                </h3>
                <p className="text-sm leading-relaxed text-gray-7">{feature.body}</p>
              </div>
            ))}
          </div>
        </section>

        <InstallCloser />
      </main>
      <Footer />
    </div>
  )
}
