#!/bin/bash
set -euxo pipefail

export COMMIT_SHA=$(git rev-parse HEAD)

echo "Running orderbook prep-base..."
(cd lib/rain.orderbook && ./prep-base.sh)

echo "Setup complete!"
