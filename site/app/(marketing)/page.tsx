import {
  ArrowRightIcon,
  BoltIcon,
  CheckCircleIcon,
  ClipboardDocumentListIcon,
  CommandLineIcon,
  CpuChipIcon,
  EyeIcon,
  MoonIcon,
  ServerStackIcon,
  ViewColumnsIcon,
} from '@heroicons/react/24/outline'
import Link from 'next/link'
import { Hero } from '@/components/Hero'
import { InstallCloser } from '@/components/InstallCloser'
import { KanbanMockup } from '@/components/KanbanMockup'

const pitch = [
  {
    icon: ClipboardDocumentListIcon,
    title: 'Plan on the board',
    body: 'Tell the orchestrator what you want in plain English. It drops cards into the backlog. Promote what is ready into todo, set priorities, and pin a task to a specific workspace or machine when it matters.',
    href: '/docs/guides/getting-started/first-task',
  },
  {
    icon: CpuChipIcon,
    title: 'Workspaces run them in parallel',
    body: 'The orchestrator watches the board and hands ready tasks to free workspaces. Each holds one task at a time, in its own persistent git worktree, on the machine you assigned.',
    href: '/docs/concepts/workspaces',
  },
  {
    icon: CheckCircleIcon,
    title: 'Review — or Zen Mode auto-merges',
    body: 'Finished work lands in the review column. Inspect the diff, send it back, or squash-merge on your terms — or flip on Zen Mode and the orchestrator lands anything that clears your bar.',
    href: '/docs/concepts/zen-mode',
  },
]

const features = [
  {
    icon: ViewColumnsIcon,
    label: 'Kanban task board',
    body: 'Backlog → Todo → In Progress → Review → Done ships as the default workflow. Add more workflows — QA, research, security review — with their own statuses and gates.',
  },
  {
    icon: ServerStackIcon,
    label: 'Workspace pool',
    body: 'A named pool declared once in project YAML — hub-local, remote over SSH, or both. Each workspace is one persistent git worktree pinned to a machine.',
  },
  {
    icon: BoltIcon,
    label: 'Orchestrator agent',
    body: 'The scheduler is a prompt you edit — a plain markdown file per project. Retune dispatch rules, add per-project policy, or swap the whole agent without touching Rust.',
  },
  {
    icon: MoonIcon,
    label: 'Zen Mode',
    body: 'Turn on hands-off auto-merge. Cleared branches land without you — local checks pass, CI green, no danger paths matched. Anything ambiguous still lands in review.',
  },
  {
    icon: EyeIcon,
    label: 'Review column',
    body: 'A finished task checks its branch out into a dedicated review pane with a fresh agent pointed at the diff. Approve, send back, or squash-merge into main.',
  },
  {
    icon: CommandLineIcon,
    label: 'tmux-based TUI',
    body: 'Sidebar, task board, orchestrator chat, and workspace panes all live in one tmux session. Sessions survive SSH drops and client restarts.',
  },
]

export default function HomePage() {
  return (
    <div className="flex min-h-screen flex-col">
      <main className="flex flex-1 flex-col">
        <Hero />

        <KanbanMockup />

        <section>
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
    </div>
  )
}
