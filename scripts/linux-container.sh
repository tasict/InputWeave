#!/bin/sh
# The Linux build and test containers (scripts/linux-container/README.md):
# inputweave-linux builds and tests for amd64, inputweave-linux-arm64 cross
# builds for arm64 (see the Dockerfile). The repo is mounted at /src in
# both; the cargo registry and each container's target directory are named
# volumes, so they stay out of the repo's own target/.
#
#   scripts/linux-container.sh create         build the images and start both containers
#   scripts/linux-container.sh run CMD…       run a shell command in /src (amd64)
#   scripts/linux-container.sh run-arm64 CMD… the same in the arm64 cross container
#   scripts/linux-container.sh shell          an interactive shell (amd64)
#   scripts/linux-container.sh deb            build the amd64 and arm64 .deb into dist/
#   scripts/linux-container.sh check-deb      install the amd64 .deb in fresh Ubuntu 24.04 and Debian 13,
#                                             and read its firewalld service
#
# Docker runs on this machine unless these say otherwise (they may also be
# set in scripts/linux-container.env, which is not part of the repository):
#   INPUTWEAVE_DOCKER_SSH    ssh destination of a machine to run docker on
#   INPUTWEAVE_DOCKER        the docker program there (default: docker)
#   INPUTWEAVE_REPO_ON_HOST  this repository's path as that machine sees it
#                            (default: this checkout's own path)
set -eu

top=$(cd "$(dirname "$0")/.." && pwd -P)
env_file=${INPUTWEAVE_CONTAINER_ENV:-$top/scripts/linux-container.env}
if [ -f "$env_file" ]; then
    # shellcheck disable=SC1090
    . "$env_file"
fi
ssh_to=${INPUTWEAVE_DOCKER_SSH:-}
docker=${INPUTWEAVE_DOCKER:-docker}
repo=${INPUTWEAVE_REPO_ON_HOST:-$top}

# Runs a shell command where docker is: over ssh, or here.
on_host() {
    if [ -n "$ssh_to" ]; then
        ssh "$ssh_to" "$1"
    else
        sh -c "$1"
    fi
}

# Commands run as root, which may own the mounted files, but without the
# capabilities that let root ignore file permissions: tests that make a
# file unreadable expect it to stay so.
exec_in() {
    name=$1
    shift
    # Quoted for the remote shell as one argument to bash -lc.
    cmd=$(printf '%s' "$*" | sed "s/'/'\\\\''/g")
    on_host "$docker exec $name setpriv --bounding-set=-dac_override,-dac_read_search bash -lc '$cmd'"
}

case "${1:-}" in
create)
    on_host "set -e
        for stage in native arm64; do
            $docker build --target \$stage -t inputweave-linux-\$stage:1 $repo/scripts/linux-container
        done
        for c in inputweave-linux:native:inputweave-target inputweave-linux-arm64:arm64:inputweave-target-arm64; do
            name=\${c%%:*}; rest=\${c#*:}; stage=\${rest%%:*}; target=\${rest#*:}
            $docker rm -f \$name >/dev/null 2>&1 || true
            $docker run -d --init --name \$name --restart unless-stopped \
                --memory 8g --memory-swap 10g \
                -v $repo:/src \
                -v inputweave-cargo-registry:/usr/local/cargo/registry \
                -v \$target:/target \
                inputweave-linux-\$stage:1 sleep infinity
        done"
    ;;
run)
    shift
    exec_in inputweave-linux "$@"
    ;;
run-arm64)
    shift
    exec_in inputweave-linux-arm64 "$@"
    ;;
shell)
    if [ -n "$ssh_to" ]; then
        ssh -t "$ssh_to" "$docker exec -it inputweave-linux bash"
    else
        $docker exec -it inputweave-linux bash
    fi
    ;;
deb)
    exec_in inputweave-linux sh packaging/linux/build-deb.sh amd64
    exec_in inputweave-linux-arm64 sh packaging/linux/build-deb.sh arm64
    ;;
check-deb)
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$top/Cargo.toml" | head -1)
    for image in ubuntu:24.04 debian:trixie; do
        echo "######## $image"
        on_host "$docker run --rm -v $repo/dist:/deb:ro -v $repo/packaging/linux:/pkg:ro $image \
            sh /pkg/check-install.sh inputweave_${version}_amd64.deb"
    done
    echo "######## firewalld"
    on_host "$docker run --rm -v $repo/packaging/linux:/pkg:ro inputweave-linux-native:1 \
        sh /pkg/check-firewalld.sh"
    ;;
*)
    sed -n '2,21p' "$0" >&2
    exit 2
    ;;
esac
