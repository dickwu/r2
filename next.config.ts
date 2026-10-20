import type { NextConfig } from 'next';

const nextConfig: NextConfig = {
  output: 'export',
  images: {
    unoptimized: true,
  },
  // Tauri needs static export for production build
  distDir: 'dist',
  turbopack: {
    root: './',
    // Dead AMD branches in @file-viewer/capability-mermaid; see emptyModule.ts.
    resolveAlias: {
      'cose-base': './src/app/lib/emptyModule.ts',
      'layout-base': './src/app/lib/emptyModule.ts',
    },
  },
};

export default nextConfig;
