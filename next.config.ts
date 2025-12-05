import type { NextConfig } from 'next';

const nextConfig: NextConfig = {
  output: 'export',
  images: {
    unoptimized: true,
  },
  // Tauri needs static export for production build
  distDir: 'dist',
};

export default nextConfig;
