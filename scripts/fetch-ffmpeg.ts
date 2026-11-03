/**
 * Fetches the static ffmpeg the app bundles as its `r2-ffmpeg` sidecar
 * (`bundle.externalBin` in src-tauri/tauri.conf.json) into
 * src-tauri/binaries/r2-ffmpeg-<target-triple>[.exe].
 *
 *   bun scripts/fetch-ffmpeg.ts                 # host triple
 *   bun scripts/fetch-ffmpeg.ts --target x86_64-apple-darwin
 *
 * Every build is pinned to an exact upstream release and checked against its
 * sha256 before it is unpacked. tauri-build refuses to compile without the
 * sidecar for the triple being built, so `tauri:dev`, `tauri build`
 * (beforeBuildCommand) and CI run this first; an up-to-date binary is kept.
 *
 * These are GPLv3 builds of FFmpeg 9.0.2 run as a separate program; see
 * src-tauri/licenses/FFMPEG-NOTICE.md (shipped in the app) for sources.
 */
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join } from 'node:path';

export interface FfmpegBuild {
  /** FFmpeg version the build reports. */
  version: string;
  url: string;
  sha256: string;
  /** Path of the ffmpeg executable inside the archive. */
  member: string;
}

export const FFMPEG_BUILDS: Record<string, FfmpegBuild> = {
  // Martin Riedl's static macOS builds (Developer ID signed), release 9.0.2.
  'aarch64-apple-darwin': {
    version: '9.0.2',
    url: 'https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip',
    sha256: 'c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924',
    member: 'ffmpeg',
  },
  'x86_64-apple-darwin': {
    version: '9.0.2',
    url: 'https://ffmpeg.martin-riedl.de/download/macos/amd64/1789931006_9.0.2/ffmpeg.zip',
    sha256: '7c6b4125b191cbf773832dc51f424cf2b6bb7da43007d1e066f95909e47cacd4',
    member: 'ffmpeg',
  },
  // BtbN/FFmpeg-Builds, 9.0 release branch, GPL static. The last build of a
  // month is kept for two years (their retention policy).
  'x86_64-unknown-linux-gnu': {
    version: 'n9.0.2-39-g7adfbe40dc',
    url: 'https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-31-12-59/ffmpeg-n9.0.2-39-g7adfbe40dc-linux64-gpl-9.0.tar.xz',
    sha256: '73e29a56974ae5a651133a04584c7483b21afe941e17dc91d79814a758b608d3',
    member: 'ffmpeg-n9.0.2-39-g7adfbe40dc-linux64-gpl-9.0/bin/ffmpeg',
  },
  // gyan.dev release essentials build 9.0.2 (static, GPLv3).
  'x86_64-pc-windows-msvc': {
    version: '9.0.2',
    url: 'https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip',
    sha256: '60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba',
    member: 'ffmpeg-9.0.2-essentials_build/bin/ffmpeg.exe',
  },
};

const BINARIES_DIR = join(import.meta.dirname, '..', 'src-tauri', 'binaries');

export function sidecarPath(triple: string, dir = BINARIES_DIR): string {
  return join(dir, `r2-ffmpeg-${triple}${triple.includes('windows') ? '.exe' : ''}`);
}

/** Target triples named by `--target X` / `--target=X`, plus the host. */
export function requestedTriples(argv: string[], host: string, envTriple?: string): string[] {
  const out = new Set<string>([envTriple || host]);
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--target' && argv[i + 1]) out.add(argv[++i]);
    else if (arg.startsWith('--target=')) out.add(arg.slice('--target='.length));
  }
  return [...out];
}

function hostTriple(): string {
  const info = execFileSync('rustc', ['-vV'], { encoding: 'utf8' });
  const host = /^host: (.+)$/m.exec(info)?.[1]?.trim();
  if (!host) throw new Error('could not read the host triple from `rustc -vV`');
  return host;
}

function sha256(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex');
}

function extract(archive: string, member: string, into: string): void {
  if (process.platform === 'win32') {
    // Windows' bsdtar unpacks zip; Git's GNU tar earlier on PATH would not.
    const tar = join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe');
    execFileSync(tar, ['-xf', archive, '-C', into, member], { stdio: 'inherit' });
  } else if (archive.endsWith('.zip')) {
    execFileSync('unzip', ['-o', '-q', archive, member, '-d', into], { stdio: 'inherit' });
  } else {
    execFileSync('tar', ['-xJf', archive, '-C', into, member], { stdio: 'inherit' });
  }
}

async function fetchSidecar(triple: string): Promise<void> {
  const build = FFMPEG_BUILDS[triple];
  if (!build) {
    throw new Error(
      `no pinned ffmpeg build for ${triple}; supported: ${Object.keys(FFMPEG_BUILDS).join(', ')}`
    );
  }
  const target = sidecarPath(triple);
  const stamp = `${target}.sha256`;
  if (
    existsSync(target) &&
    existsSync(stamp) &&
    readFileSync(stamp, 'utf8').trim() === build.sha256
  ) {
    console.log(`ffmpeg ${build.version} for ${triple} is up to date`);
    return;
  }

  console.log(`Downloading ffmpeg ${build.version} for ${triple}`);
  const response = await fetch(build.url);
  if (!response.ok) throw new Error(`${build.url}: HTTP ${response.status}`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  const actual = sha256(bytes);
  if (actual !== build.sha256) {
    throw new Error(`${build.url}: sha256 ${actual}, expected ${build.sha256}`);
  }

  // Unpack next to the target so the final rename never crosses volumes.
  mkdirSync(dirname(target), { recursive: true });
  const work = mkdtempSync(join(dirname(target), '.fetch-'));
  try {
    const archive = join(work, build.url.endsWith('.zip') ? 'ffmpeg.zip' : 'ffmpeg.tar.xz');
    writeFileSync(archive, bytes);
    extract(archive, build.member, work);
    renameSync(join(work, build.member), target);
    chmodSync(target, 0o755);
    writeFileSync(stamp, `${build.sha256}\n`);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
  console.log(`ffmpeg ${build.version} -> ${target}`);
}

if (import.meta.main) {
  const triples = requestedTriples(
    process.argv.slice(2),
    hostTriple(),
    process.env.TAURI_ENV_TARGET_TRIPLE
  );
  for (const triple of triples) {
    await fetchSidecar(triple);
  }
}
