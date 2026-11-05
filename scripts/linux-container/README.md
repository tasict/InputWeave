# Linux build and test containers

The Linux backends, the GTK app and the `.deb` packages are built and tested in two Ubuntu 24.04 containers, from the two stages of the `Dockerfile` here:

| Container | Image stage | Used for |
|---|---|---|
| `inputweave-linux` | `native` | amd64 builds and tests: GTK 4 / libadwaita development packages, Xvfb, xdotool, xclip, sway, wl-clipboard, dbus, qemu-user, musl-tools |
| `inputweave-linux-arm64` | `arm64` | arm64 cross builds: the `aarch64-linux-gnu` toolchain and the arm64 GTK 4 / libadwaita (Ubuntu 24.04 cannot install both architectures' development packages side by side) |

`scripts/linux-container.sh` drives them:

```sh
scripts/linux-container.sh create          # build both images and start both containers
scripts/linux-container.sh run 'CMD'       # run CMD in /src in the amd64 container
scripts/linux-container.sh run-arm64 'CMD' # the same in the arm64 container
scripts/linux-container.sh shell           # an interactive shell in the amd64 container
scripts/linux-container.sh deb             # build dist/inputweave_<version>_{amd64,arm64}.deb
scripts/linux-container.sh check-deb       # install the amd64 package in fresh Ubuntu 24.04 and Debian 13
```

Common commands:

```sh
scripts/linux-container.sh run 'cargo test --workspace'
scripts/linux-container.sh run sh scripts/linux-x11-tests.sh       # the X11 tests on Xvfb (ignored by default)
scripts/linux-container.sh run sh scripts/linux-wayland-tests.sh   # a fake portal, a libei server and headless sway
scripts/linux-container.sh run 'cd apps/linux && CARGO_TARGET_DIR=/target/app cargo test'
```

## Where docker runs

By default the script runs `docker` on this machine and mounts this checkout at `/src`. To use docker on another machine, for example one that shares the checkout over the network, set:

| Variable | Meaning |
|---|---|
| `INPUTWEAVE_DOCKER_SSH` | ssh destination of the machine that runs docker |
| `INPUTWEAVE_DOCKER` | the docker program on that machine (default `docker`) |
| `INPUTWEAVE_REPO_ON_HOST` | the path of this repository as that machine sees it (default: this checkout's own path) |

The script reads `scripts/linux-container.env` first when it exists (it is ignored by git), so these can be kept there, for instance:

```sh
: "${INPUTWEAVE_DOCKER_SSH:=builder@build-host}"
: "${INPUTWEAVE_REPO_ON_HOST:=/srv/src/InputWeave}"
```

## How the containers are set up

- The repository is mounted at `/src`. The cargo registry (`inputweave-cargo-registry`, shared by both containers) and each container's target directory (`inputweave-target`, `inputweave-target-arm64`, mounted at `/target`) are named volumes, so builds never touch the repository's own `target/`. Do not let both containers download crates for the first time at once: they share the registry and can overwrite each other's unpacked sources ("File exists").
- A second cargo workspace (such as `apps/linux`) uses a subfolder of `/target`, so it does not wait on the root workspace's lock.
- Commands run as root, since root may own the mounted files, but `setpriv` removes `CAP_DAC_OVERRIDE` and `CAP_DAC_READ_SEARCH`, so file permissions still apply: some tests make a file unreadable and expect it to stay so.
- Each container is limited to 8 GB of memory and `CARGO_BUILD_JOBS=2`; a first build of the whole workspace takes a while, so run long builds in the background.
- The containers run with `docker run --init`, so stopping them works and Xvfb and dbus-daemon processes that tests leave behind are reaped.
- Arm64 programs can be tried with `qemu-aarch64 -L /usr/aarch64-linux-gnu <program>`.
- On a Linux 5.10 host kernel, Ubuntu 24.04's `systemd-sysusers` fails (fchmod through `/proc` on an `O_PATH` descriptor), and with it every package script that creates a system user. The image therefore replaces it with `sysusers.sh`, which creates users and groups with shadow's tools, and makes systemd's own postinst a no-op. systemd itself never runs in the containers (`systemd-analyze` works). For the same reason `check-deb` installs `dbus-x11` first, so GTK's dconf does not pull in systemd, and `dpkg-deb -x` may report that it cannot set folder modes; the files are extracted anyway.
- To install a package into a running container, use `docker exec` directly: `run` drops the capabilities apt's `_apt` folder needs.
