#!/bin/bash
set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Get current version from package.json
CURRENT_VERSION=$(grep '"version"' package.json | head -1 | sed 's/.*: "\(.*\)".*/\1/')

usage() {
    echo "Usage: $0 <version>"
    echo ""
    echo "Examples:"
    echo "  $0 0.2.0      # Set specific version"
    echo "  $0 patch      # Bump patch: $CURRENT_VERSION -> $(echo $CURRENT_VERSION | awk -F. '{print $1"."$2"."$3+1}')"
    echo "  $0 minor      # Bump minor: $CURRENT_VERSION -> $(echo $CURRENT_VERSION | awk -F. '{print $1"."$2+1".0"}')"
    echo "  $0 major      # Bump major: $CURRENT_VERSION -> $(echo $CURRENT_VERSION | awk -F. '{print $1+1".0.0"}')"
    echo ""
    echo "Current version: $CURRENT_VERSION"
    exit 1
}

if [ -z "$1" ]; then
    usage
fi

# Calculate new version
case "$1" in
    patch)
        NEW_VERSION=$(echo $CURRENT_VERSION | awk -F. '{print $1"."$2"."$3+1}')
        ;;
    minor)
        NEW_VERSION=$(echo $CURRENT_VERSION | awk -F. '{print $1"."$2+1".0"}')
        ;;
    major)
        NEW_VERSION=$(echo $CURRENT_VERSION | awk -F. '{print $1+1".0.0"}')
        ;;
    *)
        # Validate version format (x.y.z)
        if [[ ! "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            echo -e "${RED}Error: Invalid version format. Use x.y.z (e.g., 1.2.3)${NC}"
            exit 1
        fi
        NEW_VERSION="$1"
        ;;
esac

echo -e "${YELLOW}Updating version: ${CURRENT_VERSION} -> ${NEW_VERSION}${NC}"

# Check for uncommitted changes
if [ -n "$(git status --porcelain)" ]; then
    echo -e "${RED}Error: Working directory has uncommitted changes. Commit or stash them first.${NC}"
    exit 1
fi

# Update package.json
sed -i '' 's/"version": "[0-9]*\.[0-9]*\.[0-9]*"/"version": "'"$NEW_VERSION"'"/' package.json
echo -e "${GREEN}✓ Updated package.json${NC}"

# Update src-tauri/tauri.conf.json (use regex pattern to match any version)
sed -i '' 's/"version": "[0-9]*\.[0-9]*\.[0-9]*"/"version": "'"$NEW_VERSION"'"/' src-tauri/tauri.conf.json
echo -e "${GREEN}✓ Updated src-tauri/tauri.conf.json${NC}"

# Update src-tauri/Cargo.toml (use regex pattern to match any version on version line)
sed -i '' 's/^version = "[0-9]*\.[0-9]*\.[0-9]*"/version = "'"$NEW_VERSION"'"/' src-tauri/Cargo.toml
echo -e "${GREEN}✓ Updated src-tauri/Cargo.toml${NC}"

# Update src-tauri/Cargo.lock (version is on line after 'name = "r2"')
sed -i '' '/^name = "r2"$/{n;s/^version = ".*"/version = "'"$NEW_VERSION"'"/;}' src-tauri/Cargo.lock
echo -e "${GREEN}✓ Updated src-tauri/Cargo.lock${NC}"

# Git commit
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "chore: bump version to $NEW_VERSION"
echo -e "${GREEN}✓ Committed changes${NC}"

# Create and push tag
git tag "v$NEW_VERSION"
echo -e "${GREEN}✓ Created tag v$NEW_VERSION${NC}"

git push
git push origin "v$NEW_VERSION"
echo -e "${GREEN}✓ Pushed to remote${NC}"

echo ""
echo -e "${GREEN}Successfully published v$NEW_VERSION${NC}"
echo -e "GitHub Actions will now build and create the release."

