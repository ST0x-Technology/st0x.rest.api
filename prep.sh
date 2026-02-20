#!/bin/bash
set -euxo pipefail

echo "Running orderbook prep-base..."
(cd lib/rain.orderbook && ./prep-base.sh)

echo "Setup complete!"
