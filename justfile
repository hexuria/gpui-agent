# Publish gpui-agent to crates.io by tagging main. CI has the registry token.
#   just publish            bump patch (0.2.0 -> 0.2.1), push main, tag v<version>
#   just publish 0.2.0      publish that exact version
# Does not run cargo publish and does not read the registry token.

# Bump the patch, or publish an exact MAJOR.MINOR.PATCH, via the v* tag workflow.
publish version="":
    #!/usr/bin/env bash
    set -euo pipefail
    ver="{{version}}"
    if [ -n "${ver}" ] && ! [[ "${ver}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "version must be MAJOR.MINOR.PATCH, got ${ver}" >&2
        exit 1
    fi
    if [ -n "${ver}" ]; then
        python3 scripts/publish_crates.py "${ver}"
    else
        python3 scripts/publish_crates.py
    fi
