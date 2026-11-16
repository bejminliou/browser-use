#!/usr/bin/env bash
set -euo pipefail

echo "==> Running tests and verifying build..."
just test

echo "==> Building release..."
just build

echo "==> Packaging release..."
# Remove old zip if it exists
rm -f BrowserUse.zip
zip -r BrowserUse.zip build/Browser\ Use.app

echo "==> Release artifact created: BrowserUse.zip"
