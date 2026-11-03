# FFmpeg

R2 Client ships an unmodified static build of [FFmpeg](https://ffmpeg.org)
as the separate program `r2-ffmpeg` (`r2-ffmpeg.exe` on Windows). The app
runs it only to render video thumbnails; it is not linked into R2 Client.

FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project.
These builds are not made or endorsed by the FFmpeg project.

These builds are configured with `--enable-gpl --enable-version3` and are
licensed under the GNU General Public License, version 3 (see `GPL-3.0.txt`
next to this file). R2 Client itself remains under its own MIT license.

| Platform     | FFmpeg version        | Build                                                                                                     |
| ------------ | --------------------- | --------------------------------------------------------------------------------------------------------- |
| macOS arm64  | 9.0.2                 | Martin Riedl, https://ffmpeg.martin-riedl.de (`macos/arm64/1789931890_9.0.2`)                             |
| macOS x86_64 | 9.0.2                 | Martin Riedl, https://ffmpeg.martin-riedl.de (`macos/amd64/1789931006_9.0.2`)                             |
| Linux x86_64 | n9.0.2-39-g7adfbe40dc | BtbN/FFmpeg-Builds, https://github.com/BtbN/FFmpeg-Builds (release `autobuild-2026-10-31-12-59`)          |
| Windows x64  | 9.0.2                 | gyan.dev essentials, https://www.gyan.dev/ffmpeg/builds/ (https://github.com/GyanD/codexffmpeg tag 9.0.2) |

## Corresponding source

- FFmpeg 9.0.2: https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz and
  https://git.ffmpeg.org/ffmpeg.git (tag `n9.0.2`; the Linux build is the
  `release/9.0` branch at commit `7adfbe40dc`).
- The build scripts and the third-party libraries each build compiles in are
  published by its builder at the links above (Martin Riedl:
  https://git.martin-riedl.de/ffmpeg/build-script, BtbN:
  https://github.com/BtbN/FFmpeg-Builds, gyan.dev: the build page above lists
  every component and version).

If any of those sources becomes unavailable, open an issue at
https://github.com/dickwu/r2/issues to receive the complete corresponding
source for the FFmpeg binary in the release you got, for three years from
that release.

Pinned download URLs and sha256 checksums are in `scripts/fetch-ffmpeg.ts` in
the R2 Client repository.
