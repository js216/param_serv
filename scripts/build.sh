#!/bin/bash
# Build param_serv and report warnings/errors
cd "$(dirname "$0")/.."
cargo build 2>&1
