import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  turbopack: {
    root: import.meta.dirname,
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
