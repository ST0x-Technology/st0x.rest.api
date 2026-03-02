#!/bin/bash
set -euxo pipefail

export COMMIT_SHA=$(git rev-parse HEAD)

# Initialize and update the ethgild submodule
echo "Initializing ethgild submodule..."
git submodule update --init --recursive

echo "Running orderbook prep-base..."
(cd lib/rain.orderbook && ./prep-base.sh)

echo "Building docs..."
nix build .#st0x-docs
rm -rf docs/book
cp -r result docs/book

echo "Setup complete!"
