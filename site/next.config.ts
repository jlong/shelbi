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
    return [
      {
        source: "/docs/concepts/columns",
        destination: "/docs/concepts/workflows",
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
