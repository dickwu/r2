import type { Metadata } from 'next';
import { preload } from 'react-dom';
import Providers from '@/app/providers';
import '@/app/fonts.css';
import '@/app/globals.css';

export const metadata: Metadata = {
  title: 'R2 Uploader',
  description: 'Cloudflare R2 file uploader',
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  // Preload the latin subset of each self-hosted family (see fonts.css). preload() puts one
  // <link rel="preload"> in <head>; a <link> element rendered here would be emitted twice.
  preload('/fonts/inter-latin.woff2', { as: 'font', type: 'font/woff2' });
  preload('/fonts/inter-tight-latin.woff2', { as: 'font', type: 'font/woff2' });
  preload('/fonts/jetbrains-mono-latin.woff2', { as: 'font', type: 'font/woff2' });

  return (
    <html lang="en" suppressHydrationWarning>
      <body>
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
