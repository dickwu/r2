#!/bin/bash

# Generate all Tauri app icons from logo.png
# Requires: ImageMagick (magick command) and iconutil (macOS)

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

if ! command -v magick &> /dev/null; then
    echo "Error: ImageMagick not found. Install with: brew install imagemagick"
    exit 1
fi

echo "Generating icons from logo.png..."

# Standard PNG sizes
magick -background none logo.png -resize 32x32 32x32.png
magick -background none logo.png -resize 128x128 128x128.png
magick -background none logo.png -resize 256x256 128x128@2x.png
magick -background none logo.png -resize 512x512 icon.png

# Windows Store logos
magick -background none logo.png -resize 30x30 Square30x30Logo.png
magick -background none logo.png -resize 44x44 Square44x44Logo.png
magick -background none logo.png -resize 71x71 Square71x71Logo.png
magick -background none logo.png -resize 89x89 Square89x89Logo.png
magick -background none logo.png -resize 107x107 Square107x107Logo.png
magick -background none logo.png -resize 142x142 Square142x142Logo.png
magick -background none logo.png -resize 150x150 Square150x150Logo.png
magick -background none logo.png -resize 284x284 Square284x284Logo.png
magick -background none logo.png -resize 310x310 Square310x310Logo.png
magick -background none logo.png -resize 50x50 StoreLogo.png

echo "✓ PNG icons generated"

# Windows ICO (multi-resolution)
magick -background none logo.png -resize 16x16 icon_16.png
magick -background none logo.png -resize 24x24 icon_24.png
magick -background none logo.png -resize 32x32 icon_32.png
magick -background none logo.png -resize 48x48 icon_48.png
magick -background none logo.png -resize 64x64 icon_64.png
magick -background none logo.png -resize 128x128 icon_128.png
magick -background none logo.png -resize 256x256 icon_256.png
magick icon_16.png icon_24.png icon_32.png icon_48.png icon_64.png icon_128.png icon_256.png icon.ico
rm icon_16.png icon_24.png icon_32.png icon_48.png icon_64.png icon_128.png icon_256.png

echo "✓ Windows icon.ico generated"

# macOS ICNS
if command -v iconutil &> /dev/null; then
    mkdir -p icon.iconset
    magick -background none logo.png -resize 16x16 icon.iconset/icon_16x16.png
    magick -background none logo.png -resize 32x32 icon.iconset/icon_16x16@2x.png
    magick -background none logo.png -resize 32x32 icon.iconset/icon_32x32.png
    magick -background none logo.png -resize 64x64 icon.iconset/icon_32x32@2x.png
    magick -background none logo.png -resize 128x128 icon.iconset/icon_128x128.png
    magick -background none logo.png -resize 256x256 icon.iconset/icon_128x128@2x.png
    magick -background none logo.png -resize 256x256 icon.iconset/icon_256x256.png
    magick -background none logo.png -resize 512x512 icon.iconset/icon_256x256@2x.png
    magick -background none logo.png -resize 512x512 icon.iconset/icon_512x512.png
    magick -background none logo.png -resize 1024x1024 icon.iconset/icon_512x512@2x.png
    iconutil -c icns icon.iconset -o icon.icns
    rm -rf icon.iconset
    echo "✓ macOS icon.icns generated"
else
    echo "⚠ iconutil not found (macOS only), skipping icon.icns"
fi

echo ""
echo "All icons generated successfully!"

