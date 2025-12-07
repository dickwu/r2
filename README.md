# Cloudflare R2 Client

A desktop application for managing Cloudflare R2 storage buckets. Built with Tauri, Next.js, and React.

## Features

- Browse and manage files in Cloudflare R2 buckets
- Upload files with drag-and-drop support
- Preview images and videos
- Video thumbnail generation (via ffmpeg)
- Copy file URLs to clipboard

## Prerequisites

### Required

- [Node.js](https://nodejs.org/) (v18+)
- [Bun](https://bun.sh/) - Package manager
- [Rust](https://www.rust-lang.org/tools/install) - For Tauri backend

### Optional (for video thumbnails)

- [ffmpeg](https://ffmpeg.org/) - Required for video thumbnail generation

```bash
# macOS
brew install ffmpeg

# Ubuntu/Debian
sudo apt install ffmpeg

# Windows (via Chocolatey)
choco install ffmpeg
```

## macOS Installation

On macOS, you need to manually allow the app to run:

1. Open **System Preferences** → **Privacy & Security**
2. Click **"Open Anyway"** next to the blocked app message

If the app doesn't appear in Privacy & Security settings, run:

```bash
sudo xattr -d com.apple.quarantine /Applications/r2.app/
```

## Development

```bash
# Install dependencies
bun install

# Run development server
bun run tauri dev
```

## Build

```bash
# Build for production
bun run tauri build
```

## Configuration

On first launch, configure your Cloudflare R2 credentials:

1. Click the settings icon
2. Enter your R2 credentials:
   - **Account ID** - Your Cloudflare account ID
   - **Access Key ID** - R2 API token access key
   - **Secret Access Key** - R2 API token secret key
   - **Bucket Name** - Your R2 bucket name
   - **Public URL** (optional) - Custom domain for public access

## Tech Stack

- **Frontend**: Next.js, React, Ant Design
- **Backend**: Tauri (Rust)
- **Storage**: Cloudflare R2 (S3-compatible)
- **State**: TanStack Query

## IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
