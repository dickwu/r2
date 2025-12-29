# Cloudflare R2 Client

A desktop application for managing Cloudflare R2 storage buckets. Built with Tauri, Next.js, and React.

## Features

- **Multi-Account Support** - Manage multiple Cloudflare accounts in one app
- **Multiple Tokens per Account** - Each account can have multiple API tokens with different bucket access
- Browse and manage files in Cloudflare R2 buckets
- Upload files and folders with resumable multipart uploads
- Preview images and videos directly in the app
- Video thumbnail generation (via ffmpeg)
- Copy signed or public URLs to clipboard
- Dark mode support
- Auto-updates

## Screenshots

![Prerequisites](./images/create-account.png)

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

### Cross-compiling for Windows (from macOS/Linux)

To build Windows binaries from macOS or Linux, you need additional tools:

```bash
# Install required dependencies
# macOS
brew install cmake ninja llvm nsis

# Ubuntu/Debian
sudo apt-get install cmake ninja-build llvm nsis

# Install cargo-xwin
cargo install --locked cargo-xwin

# Setup Windows toolchain
cargo xwin setup

# Add Windows target
rustup target add x86_64-pc-windows-msvc

# Build for Windows
bun run tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc
```

**Note:** Cross-compilation is experimental. For production builds, consider using GitHub Actions or a Windows VM.

## Configuration

### Adding Your First Account

1. On first launch, the "Add Account" dialog will open automatically
2. Enter your Cloudflare credentials:
   - **Account ID** - Your Cloudflare account ID (found in dashboard URL)
   - **Display Name** (optional) - Friendly name for the account
   - **Token Name** (optional) - Name to identify this token (e.g., "Production", "Staging")
   - **API Token** - Cloudflare API token with R2 read/write permissions
   - **Access Key ID** - S3-compatible Access Key ID
   - **Secret Access Key** - S3-compatible Secret Access Key
3. Click "Load" to fetch available buckets, or manually add bucket names
4. Configure public domain for each bucket (optional)

### Managing Multiple Accounts

- **Sidebar** - Shows all configured accounts with token/bucket counts
- **Click account** - Opens drawer with all tokens and buckets for that account
- **Click bucket** - Switches to that bucket
- **Context menu** - Edit or delete accounts/tokens
- **Collapse sidebar** - Click the collapse icon to minimize to icon-only view

### Getting API Credentials

1. Go to [Cloudflare Dashboard](https://dash.cloudflare.com/)
2. Navigate to **R2** → **Overview** → **Manage R2 API Tokens**
3. Create a token with appropriate permissions
4. Note the **Access Key ID** and **Secret Access Key** (shown only once)

## Data Storage

Account configurations are stored locally in a SQLite database:

- **macOS**: `~/Library/Application Support/r2/uploads-turso.db`
- **Windows**: `%APPDATA%\r2\uploads-turso.db`
- **Linux**: `~/.local/share/r2/uploads-turso.db`

The app automatically migrates from the old `uploads.db` (rusqlite) format on first launch.

## Tech Stack

- **Frontend**: Next.js, React, Ant Design, Zustand
- **Backend**: Tauri (Rust)
- **Storage**: Cloudflare R2 (S3-compatible)
- **Database**: SQLite (via Turso - Rust-based SQLite-compatible database)
- **State**: TanStack Query, Zustand

## IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
