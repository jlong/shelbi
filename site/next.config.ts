import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  turbopack: {
    root: import.meta.dirname,
  },
  async rewrites() {
    return [
      // Per-page markdown: appending `.md` to any docs URL serves the page's
      // clean markdown source. `:slug(.*)` captures the full multi-segment
      // path (including slashes) before the literal `.md`, mapping to the
      // `app/docs-md/[...slug]` route handler. See `lib/llms.ts`.
      {
        source: "/docs/:slug(.*).md",
        destination: "/docs-md/:slug",
      },
    ];
  },
  async redirects() {
    // Getting Started moved under Guides and the Workflows overview moved out
    // of Concepts into that guide. Redirect the old public URLs so bookmarks,
    // external links, and the `.md` variants keep resolving.
    const gettingStartedPages = [
      "install",
      "first-project",
      "first-task",
      "multi-workspace",
      "enable-zen-mode",
      "custom-workflow",
    ];
    return [
      {
        source: "/docs/concepts/columns",
        destination: "/docs/guides/getting-started/workflows",
        permanent: true,
      },
      {
        source: "/docs/concepts/workflows",
        destination: "/docs/guides/getting-started/workflows",
        permanent: true,
      },
      ...gettingStartedPages.map((page) => ({
        source: `/docs/getting-started/${page}`,
        destination: `/docs/guides/getting-started/${page}`,
        permanent: true,
      })),
      {
        source: "/docs/getting-started",
        destination: "/docs/guides/getting-started",
        permanent: true,
      },
      {
        source: "/docs/features/zen-mode",
        destination: "/docs/concepts/zen-mode",
        permanent: true,
      },
      {
        source: "/docs/features/events-log",
        destination: "/docs/concepts/events-log",
        permanent: true,
      },
    ];
  },
};

export default nextConfig;
