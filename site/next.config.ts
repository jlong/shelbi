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
    ];
  },
};

export default nextConfig;
