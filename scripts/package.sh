#!/usr/bin/env bash
# Build release packages for `swarm` and the `swarm-relay` server: .deb, .rpm and generic .tar.gz
# for Linux (x86_64, aarch64), .tar.gz for macOS (x86_64, aarch64), plus a
# SHA256SUMS file and an optional detached GPG signature over it.
#
#   scripts/package.sh                  # everything, into ./dist
#   scripts/package.sh --deb            # only the .deb files
#   scripts/package.sh --rpm -o /tmp/pkgs
#   scripts/package.sh --tar --macos    # only the macOS archives
#   scripts/package.sh --arch aarch64   # only ARM64 (repeatable)
#   scripts/package.sh --sign           # also sign SHA256SUMS with gpg
#   VERSION=0.2.0 scripts/package.sh    # override the Cargo.toml version
#
# Two packages come out of each .deb/.rpm build: `swarmgit` (the swarm CLI and
# git remote helper) and `swarm-relay` (the relay server, with a systemd unit
# and /etc/default/swarm-relay). The .tar.gz holds both binaries.
#
# Format flags (--deb --rpm --tar) and platform flags (--linux --macos) narrow
# the build; a group with no flag means "all of it". .deb and .rpm exist for
# Linux only; macOS gets a .tar.gz.
#
# Everything is built inside Dockerfile.package, so the host needs neither a
# Rust toolchain nor dpkg-dev/rpmbuild/zig. Targets are cross-compiled with
# cargo-zigbuild; Linux binaries link against glibc 2.17 so they run on
# anything from RHEL 7 / Debian 9 onwards. The repository is bind-mounted, so
# builds are cached in target/package and packages land in the output
# directory on the host.
#
# macOS archives are NOT signed or notarized (that needs an Apple Developer ID
# and macOS tooling). Binaries carry the linker's ad-hoc signature, so they run
# on Apple Silicon; a browser-downloaded archive may need
# `xattr -d com.apple.quarantine swarm`.
#
# Environment:
#   OUT_DIR     output directory (default: dist)
#   VERSION     overrides the workspace version from Cargo.toml
#   RELEASE     RPM release number (default: 1)
#   MAINTAINER  "Name <email>" for the .deb (default: Swarmgit, with a
#               placeholder address)
#   SIGN_KEY    gpg key id/fingerprint used to sign SHA256SUMS (implies --sign)
#   GLIBC       glibc floor for Linux builds (default: 2.17)

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

IMAGE=swarmgit-package
OUT_DIR=${OUT_DIR:-dist}
RELEASE=${RELEASE:-1}
GLIBC=${GLIBC:-2.17}
VERSION_OVERRIDE=${VERSION:-}
WANT_DEB=${WANT_DEB:-0}
WANT_RPM=${WANT_RPM:-0}
WANT_TAR=${WANT_TAR:-0}
WANT_SIGN=${WANT_SIGN:-0}
[[ -n ${SIGN_KEY:-} ]] && WANT_SIGN=1
# Space-separated "os:arch" pairs; filled in below on the host and handed to the
# container through the environment.
TARGETS=${TARGETS:-}
SEL_FMT=0 SEL_OS=0
OSES=() ARCHES=()

usage() {
    sed -n '2,41p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

add_arch() {
    case "$1" in
    x86_64 | amd64) ARCHES+=(x86_64) ;;
    aarch64 | arm64) ARCHES+=(aarch64) ;;
    *)
        echo "unknown architecture: $1 (use x86_64 or aarch64)" >&2
        exit 2
        ;;
    esac
}

while (($#)); do
    case "$1" in
    --deb) WANT_DEB=1 SEL_FMT=1 ;;
    --rpm) WANT_RPM=1 SEL_FMT=1 ;;
    --tar) WANT_TAR=1 SEL_FMT=1 ;;
    --linux) OSES+=(linux) SEL_OS=1 ;;
    --macos) OSES+=(macos) SEL_OS=1 ;;
    --sign) WANT_SIGN=1 ;;
    --arch)
        [[ $# -ge 2 ]] || {
            echo "$1 needs an architecture" >&2
            exit 2
        }
        add_arch "$2"
        shift
        ;;
    -o | --output)
        [[ $# -ge 2 ]] || {
            echo "$1 needs a directory" >&2
            exit 2
        }
        OUT_DIR=$2
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        echo "unknown argument: $1" >&2
        usage >&2
        exit 2
        ;;
    esac
    shift
done

# Host side only: turn the flags into a target list. Inside the container the
# list arrives through $TARGETS and the WANT_* variables are already final.
if [[ ${SWARM_PKG_INSIDE:-} != 1 ]]; then
    if ((!SEL_FMT)); then
        WANT_DEB=1 WANT_RPM=1 WANT_TAR=1
    fi
    ((${#OSES[@]})) || OSES=(linux macos)
    ((${#ARCHES[@]})) || ARCHES=(x86_64 aarch64)
    for os in $(printf '%s\n' "${OSES[@]}" | sort -u); do
        # .deb/.rpm only make sense on Linux; macOS only gets the archive.
        if [[ $os == macos ]] && ((!WANT_TAR)); then continue; fi
        for arch in $(printf '%s\n' "${ARCHES[@]}" | sort -u); do
            TARGETS+="${TARGETS:+ }$os:$arch"
        done
    done
    [[ -n $TARGETS ]] || {
        echo "nothing to build: --macos only produces .tar.gz (add --tar)" >&2
        exit 2
    }
fi

detect_version() {
    if [[ -n $VERSION_OVERRIDE ]]; then
        printf '%s' "$VERSION_OVERRIDE"
        return
    fi
    awk '
        /^\[workspace\.package\]/ { section = 1; next }
        /^\[/ { section = 0 }
        section && /^version[[:space:]]*=/ {
            sub(/.*=[[:space:]]*"/, "")
            sub(/".*/, "")
            print
            exit
        }
    ' Cargo.toml
}

VERSION=$(detect_version)
[[ -n $VERSION ]] || {
    echo "cannot read the version from Cargo.toml" >&2
    exit 1
}

# Debian and RPM have different version grammars; RPM forbids '-'. A Debian
# revision suffix (0.1.0-1) becomes 0.1.0 for RPM, which is why RELEASE exists.
RPM_VERSION=${VERSION//-/.}

# Published with every package, so it is never taken from the builder's git
# settings.
MAINTAINER=${MAINTAINER:-Swarmgit <maintainers@swarmgit.invalid>}

# ---------------------------------------------------------------------------
# Inside the packaging container.
# ---------------------------------------------------------------------------
if [[ ${SWARM_PKG_INSIDE:-} == 1 ]]; then
    export CARGO_TARGET_DIR=$PWD/target/package
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    # Pin archive timestamps to the commit so identical sources give identical
    # tarballs.
    EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}
    mkdir -p "$OUT_DIR"

    # Per-target context, set by build_target.
    OS= ARCH= BIN= RELAY_BIN= DEB_ARCH= RPM_ARCH=

    build_target() {
        OS=$1 ARCH=$2
        local triple
        case $OS in
        linux)
            triple=$ARCH-unknown-linux-gnu
            # The ".2.17" suffix makes zig link against that glibc, not the
            # container's (2.36), so the binary runs on old distributions.
            echo "== cargo zigbuild --release ($triple, glibc $GLIBC, version $VERSION)"
            cargo zigbuild --release --locked -p swarm-cli -p swarm-relay --target "$triple.$GLIBC"
            ;;
        macos)
            triple=$ARCH-apple-darwin
            echo "== cargo zigbuild --release ($triple, version $VERSION)"
            cargo zigbuild --release --locked -p swarm-cli -p swarm-relay --target "$triple"
            ;;
        esac
        BIN=$CARGO_TARGET_DIR/$triple/release/swarm
        RELAY_BIN=$CARGO_TARGET_DIR/$triple/release/swarm-relay
        for bin in "$BIN" "$RELAY_BIN"; do
            [[ -x $bin ]] || {
                echo "build produced no binary at $bin" >&2
                exit 1
            }
        done
        case $ARCH in
        x86_64) DEB_ARCH=amd64 RPM_ARCH=x86_64 ;;
        aarch64) DEB_ARCH=arm64 RPM_ARCH=aarch64 ;;
        esac
    }

    # Files shared by the relay's .deb and .rpm.
    relay_unit() {
        cat <<'EOF'
[Unit]
Description=Swarmgit relay
After=network-online.target
Wants=network-online.target

[Service]
User=swarm-relay
Group=swarm-relay
EnvironmentFile=-/etc/default/swarm-relay
ExecStart=/usr/bin/swarm-relay $SWARM_RELAY_ARGS
StateDirectory=swarm-relay
StateDirectoryMode=0700
Restart=always
RestartSec=2
LimitNOFILE=65536
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6
CapabilityBoundingSet=
AmbientCapabilities=

[Install]
WantedBy=multi-user.target
EOF
    }

    relay_defaults() {
        cat <<'EOF'
# Arguments for swarm-relay (see `swarm-relay --help`).
#
# --name is the exact host:port that clients put in SWARM_RELAYS. Set it to
# this machine's public address or DNS name, then run:
#   systemctl restart swarm-relay
#
# --no-network keeps this relay private: only people you give its address to
# use it. Remove it to join the public relay network, where any swarm client
# may find and use this machine.
SWARM_RELAY_ARGS="--listen 0.0.0.0:42425 --no-network --key-file /var/lib/swarm-relay/relay.key"
#SWARM_RELAY_ARGS="--listen 0.0.0.0:42425 --name relay.example.org:42425 --key-file /var/lib/swarm-relay/relay.key"
EOF
    }

    package_relay_deb() {
        local stage=$WORK/relay-deb-$ARCH/swarm-relay
        mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/lib/systemd/system" \
            "$stage/etc/default" "$stage/usr/share/doc/swarm-relay"
        install -m 0755 "$RELAY_BIN" "$stage/usr/bin/swarm-relay"
        relay_unit >"$stage/lib/systemd/system/swarm-relay.service"
        relay_defaults >"$stage/etc/default/swarm-relay"
        chmod 0644 "$stage/lib/systemd/system/swarm-relay.service" "$stage/etc/default/swarm-relay"
        [[ -f README.md ]] && install -m 0644 README.md "$stage/usr/share/doc/swarm-relay/README.md"
        echo /etc/default/swarm-relay >"$stage/DEBIAN/conffiles"

        local size
        size=$(find "$stage" -type f -printf '%s\n' |
            awk '{ total += $1 } END { print int((total + 1023) / 1024) }')
        cat >"$stage/DEBIAN/control" <<EOF
Package: swarm-relay
Version: $VERSION
Architecture: $DEB_ARCH
Maintainer: $MAINTAINER
Installed-Size: $size
Section: net
Priority: optional
Recommends: systemd
Description: relay server for swarmgit peers behind NAT
 A swarm-relay joins two outbound connections so peers that cannot accept
 inbound connections (NAT, mobile data) can still reach each other. The
 swarm handshake runs end to end through it, so the relay sees only
 ciphertext. Listens on TCP 42425 by default.
EOF
        cat >"$stage/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    if ! getent passwd swarm-relay >/dev/null; then
        adduser --system --group --no-create-home --home /nonexistent \
            --shell /usr/sbin/nologin swarm-relay >/dev/null
    fi
    if [ -d /run/systemd/system ]; then
        systemctl daemon-reload || true
        systemctl enable swarm-relay.service >/dev/null 2>&1 || true
        systemctl restart swarm-relay.service || true
    fi
fi
exit 0
EOF
        cat >"$stage/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = remove ] || [ "$1" = deconfigure ]; then
    if [ -d /run/systemd/system ]; then
        systemctl stop swarm-relay.service || true
        systemctl disable swarm-relay.service >/dev/null 2>&1 || true
    fi
fi
exit 0
EOF
        cat >"$stage/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
if [ "$1" = purge ] && getent passwd swarm-relay >/dev/null; then
    deluser --system swarm-relay >/dev/null 2>&1 || true
fi
exit 0
EOF
        chmod 0755 "$stage/DEBIAN/postinst" "$stage/DEBIAN/prerm" "$stage/DEBIAN/postrm"

        dpkg-deb --build --root-owner-group "$stage" \
            "$OUT_DIR/swarm-relay_${VERSION}_${DEB_ARCH}.deb"
    }

    package_relay_rpm() {
        local top=$WORK/relay-rpmbuild-$ARCH src=$WORK/relay-rpm-src-$ARCH spec=$WORK/relay-rpmbuild-$ARCH/SPECS/swarm-relay.spec
        mkdir -p "$top"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS} "$src"
        install -m 0755 "$RELAY_BIN" "$src/swarm-relay"
        relay_unit >"$src/swarm-relay.service"
        relay_defaults >"$src/swarm-relay.default"

        cat >"$spec" <<EOF
%global debug_package %{nil}
%global _build_id_links none
%global __os_install_post %{nil}
AutoReqProv: no

Name:           swarm-relay
Version:        $RPM_VERSION
Release:        $RELEASE
Summary:        Relay server for swarmgit peers behind NAT
License:        GPL-3.0-or-later
Requires(pre):  shadow-utils

%description
A swarm-relay joins two outbound connections so peers that cannot accept
inbound connections (NAT, mobile data) can still reach each other. The swarm
handshake runs end to end through it. Listens on TCP 42425 by default.

%install
install -Dpm 0755 $src/swarm-relay %{buildroot}%{_bindir}/swarm-relay
install -Dpm 0644 $src/swarm-relay.service %{buildroot}/usr/lib/systemd/system/swarm-relay.service
install -Dpm 0644 $src/swarm-relay.default %{buildroot}/etc/default/swarm-relay

%pre
getent passwd swarm-relay >/dev/null || useradd -r -M -d /nonexistent -s /sbin/nologin swarm-relay

%post
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || :
    systemctl enable swarm-relay.service >/dev/null 2>&1 || :
    systemctl restart swarm-relay.service || :
fi

%preun
if [ \$1 -eq 0 ] && [ -d /run/systemd/system ]; then
    systemctl stop swarm-relay.service || :
    systemctl disable swarm-relay.service >/dev/null 2>&1 || :
fi

%files
%{_bindir}/swarm-relay
/usr/lib/systemd/system/swarm-relay.service
%config(noreplace) /etc/default/swarm-relay
EOF
        rpmbuild -bb --quiet --target "$RPM_ARCH-linux" --define "_topdir $top" "$spec"
        local built
        built=$(find "$top/RPMS" -type f -name '*.rpm' | head -n1)
        [[ -n $built ]] || {
            echo "rpmbuild produced no relay package" >&2
            exit 1
        }
        install -m 0644 "$built" \
            "$OUT_DIR/swarm-relay-${RPM_VERSION}-${RELEASE}.${RPM_ARCH}.rpm"
    }

    package_deb() {
        local stage=$WORK/deb-$ARCH/swarmgit
        mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/doc/swarmgit"
        install -m 0755 "$BIN" "$stage/usr/bin/swarm"
        ln -sfn swarm "$stage/usr/bin/git-remote-swarm"
        for doc in README.md CHANGELOG.md; do
            [[ -f $doc ]] && install -m 0644 "$doc" "$stage/usr/share/doc/swarmgit/$doc"
        done
        install -Dm 0644 completions/swarm.bash "$stage/usr/share/bash-completion/completions/swarm"
        install -Dm 0644 completions/_swarm "$stage/usr/share/zsh/vendor-completions/_swarm"
        install -Dm 0644 completions/swarm.fish "$stage/usr/share/fish/vendor_completions.d/swarm.fish"
        mkdir -p "$stage/usr/share/man/man1"
        for page in man/*.1; do
            gzip -9nc "$page" >"$stage/usr/share/man/man1/${page##*/}.gz"
        done
        mkdir -p "$stage/etc/swarmgit"
        install -m 0644 docs/config.example.yaml "$stage/etc/swarmgit/config.yaml"
        echo /etc/swarmgit/config.yaml >"$stage/DEBIAN/conffiles"

        local size
        # Sum apparent sizes instead of `du`: on overlayfs/ZFS a copied file can
        # share extents and report a few KiB, which would understate the size.
        size=$(find "$stage" -type f -printf '%s\n' |
            awk '{ total += $1 } END { print int((total + 1023) / 1024) }')

        cat >"$stage/DEBIAN/control" <<EOF
Package: swarmgit
Version: $VERSION
Architecture: $DEB_ARCH
Maintainer: $MAINTAINER
Installed-Size: $size
Section: vcs
Priority: optional
Depends: git, ca-certificates
Recommends: sqlite3
Description: peer-to-peer git over an encrypted, post-quantum transport
 Swarmgit identifies a repository by a swarm:// link; any peer that can
 reach a holder of that link can clone it, with no server registration
 and no accounts. This package ships the swarm CLI and the
 git-remote-swarm git remote helper.
EOF

        dpkg-deb --build --root-owner-group "$stage" \
            "$OUT_DIR/swarmgit_${VERSION}_${DEB_ARCH}.deb"
    }

    package_rpm() {
        local top=$WORK/rpmbuild-$ARCH src=$WORK/rpm-src-$ARCH spec=$WORK/rpmbuild-$ARCH/SPECS/swarmgit.spec
        mkdir -p "$top"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS} "$src"
        install -m 0755 "$BIN" "$src/swarm"
        [[ -f README.md ]] && install -m 0644 README.md "$src/README.md"
        install -m 0644 docs/config.example.yaml "$src/config.yaml"
        cp completions/swarm.bash completions/_swarm completions/swarm.fish "$src/"
        mkdir -p "$src/man"
        for page in man/*.1; do
            gzip -9nc "$page" >"$src/man/${page##*/}.gz"
        done

        cat >"$spec" <<EOF
# Strip/BRP hooks and automatic dependency generation assume a full Fedora
# rpm setup; on Debian they are the parts most likely to fail, so disable them.
%global debug_package %{nil}
%global _build_id_links none
%global __os_install_post %{nil}
AutoReqProv: no

Name:           swarmgit
Version:        $RPM_VERSION
Release:        $RELEASE
Summary:        Peer-to-peer git over an encrypted, post-quantum transport
License:        GPL-3.0-or-later
Requires:       git
Requires:       ca-certificates

%description
Swarmgit identifies a repository by a swarm:// link; any peer that can reach
a holder of that link can clone it, with no server registration and no
accounts. This package ships the swarm CLI and the git-remote-swarm git remote
helper.

%install
install -Dpm 0755 $src/swarm %{buildroot}%{_bindir}/swarm
ln -sfn swarm %{buildroot}%{_bindir}/git-remote-swarm
install -Dpm 0644 $src/config.yaml %{buildroot}/etc/swarmgit/config.yaml
install -Dpm 0644 $src/swarm.bash %{buildroot}/usr/share/bash-completion/completions/swarm
install -Dpm 0644 $src/_swarm %{buildroot}/usr/share/zsh/site-functions/_swarm
install -Dpm 0644 $src/swarm.fish %{buildroot}/usr/share/fish/vendor_completions.d/swarm.fish
mkdir -p %{buildroot}/usr/share/man/man1
install -pm 0644 $src/man/*.1.gz %{buildroot}/usr/share/man/man1/
EOF
        if [[ -f $src/README.md ]]; then
            cat >>"$spec" <<EOF
install -Dpm 0644 $src/README.md %{buildroot}%{_docdir}/swarmgit/README.md
EOF
        fi
        cat >>"$spec" <<EOF

%files
%{_bindir}/swarm
%{_bindir}/git-remote-swarm
%config(noreplace) /etc/swarmgit/config.yaml
/usr/share/bash-completion/completions/swarm
/usr/share/zsh/site-functions/_swarm
/usr/share/fish/vendor_completions.d/swarm.fish
/usr/share/man/man1/swarm*.1.gz
EOF
        [[ -f $src/README.md ]] && echo '%{_docdir}/swarmgit/README.md' >>"$spec"

        # --target selects the package architecture even though the binary was
        # cross-compiled; rpmbuild never executes it.
        rpmbuild -bb --quiet --target "$RPM_ARCH-linux" --define "_topdir $top" "$spec"

        local built
        built=$(find "$top/RPMS" -type f -name '*.rpm' | head -n1)
        [[ -n $built ]] || {
            echo "rpmbuild produced no package" >&2
            exit 1
        }
        install -m 0644 "$built" \
            "$OUT_DIR/swarmgit-${RPM_VERSION}-${RELEASE}.${RPM_ARCH}.rpm"
    }

    package_tar() {
        local base=swarmgit-$VERSION-$OS-$ARCH
        local stage=$WORK/tar-$OS-$ARCH/$base
        mkdir -p "$stage/bin"
        install -m 0755 "$BIN" "$stage/bin/swarm"
        ln -sfn swarm "$stage/bin/git-remote-swarm"
        install -m 0755 "$RELAY_BIN" "$stage/bin/swarm-relay"
        mkdir -p "$stage/share/man/man1"
        install -m 0644 man/*.1 "$stage/share/man/man1/"
        for doc in README.md CHANGELOG.md; do
            [[ -f $doc ]] && install -m 0644 "$doc" "$stage/$doc"
        done
        # Fixed order, owner and mtime; gzip -n drops the timestamp and name.
        tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$EPOCH" \
            -C "$(dirname "$stage")" -cf - "$base" |
            gzip -9n >"$OUT_DIR/$base.tar.gz"
    }

    for target in $TARGETS; do
        build_target "${target%%:*}" "${target##*:}"
        if [[ $OS == linux ]]; then
            if ((WANT_DEB)); then package_deb && package_relay_deb; fi
            if ((WANT_RPM)); then package_rpm && package_relay_rpm; fi
        fi
        if ((WANT_TAR)); then package_tar; fi
    done

    echo
    echo "packages in $OUT_DIR:"
    find "$OUT_DIR" -maxdepth 1 -type f \( -name '*.deb' -o -name '*.rpm' -o -name '*.tar.gz' \) \
        -printf '  %f (%s bytes)\n' | sort
    exit 0
fi

# ---------------------------------------------------------------------------
# Host side: build the image and hand off to the container.
# ---------------------------------------------------------------------------
command -v docker >/dev/null || {
    echo "docker is required to build packages" >&2
    exit 1
}
if ((WANT_SIGN)); then
    command -v gpg >/dev/null || {
        echo "gpg is required to sign (--sign / SIGN_KEY)" >&2
        exit 1
    }
fi

mkdir -p "$OUT_DIR"
# The container only sees the repository. Translate an output directory inside
# the repo to /workspace, and bind-mount one that lives anywhere else.
OUT_ABS=$(realpath -m "$OUT_DIR")
OUT_MOUNT=()
CONTAINER_OUT=$OUT_ABS
if [[ $OUT_ABS == "$PWD"/* ]]; then
    CONTAINER_OUT="/workspace/${OUT_ABS#"$PWD"/}"
else
    OUT_MOUNT=(-v "$OUT_ABS:$OUT_ABS")
fi

echo "== building $IMAGE"
docker build -f "$(realpath Dockerfile.package)" -t "$IMAGE" \
    --build-arg USER_ID="$(id -u)" \
    --build-arg GROUP_ID="$(id -g)" \
    .

echo "== packaging swarmgit $VERSION ($TARGETS)"
docker run --rm -i \
    -e SWARM_PKG_INSIDE=1 \
    -e VERSION="$VERSION" \
    -e RELEASE="$RELEASE" \
    -e GLIBC="$GLIBC" \
    -e OUT_DIR="$CONTAINER_OUT" \
    -e WANT_DEB="$WANT_DEB" \
    -e WANT_RPM="$WANT_RPM" \
    -e WANT_TAR="$WANT_TAR" \
    -e TARGETS="$TARGETS" \
    -e MAINTAINER="$MAINTAINER" \
    ${SOURCE_DATE_EPOCH:+-e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH"} \
    -v "$PWD":/workspace \
    "${OUT_MOUNT[@]}" \
    -v swarmgit-cargo-registry:/usr/local/cargo/registry \
    -v swarmgit-cargo-git:/usr/local/cargo/git \
    "$IMAGE" scripts/package.sh

# ---------------------------------------------------------------------------
# Checksums and signature, on the host so the signing key never enters the
# container. SHA256SUMS covers every package of this version in OUT_DIR, not
# only the ones built in this run, so it stays complete across partial builds.
# ---------------------------------------------------------------------------
(
    cd "$OUT_DIR"
    shopt -s nullglob
    files=(
        "swarmgit_${VERSION}_"*.deb
        "swarm-relay_${VERSION}_"*.deb
        "swarmgit-${RPM_VERSION}-${RELEASE}."*.rpm
        "swarm-relay-${RPM_VERSION}-${RELEASE}."*.rpm
        "swarmgit-${VERSION}-"{linux,macos}-*.tar.gz
    )
    ((${#files[@]})) || exit 0
    sha256sum -- "${files[@]}" >SHA256SUMS
    echo
    echo "== SHA256SUMS"
    cat SHA256SUMS

    rm -f SHA256SUMS.asc
    if ((WANT_SIGN)); then
        key=(${SIGN_KEY:+--local-user "$SIGN_KEY"})
        gpg --batch --yes "${key[@]}" --armor --detach-sign \
            --output SHA256SUMS.asc SHA256SUMS
        gpg --batch --verify SHA256SUMS.asc SHA256SUMS
        gpg --batch --yes "${key[@]}" --armor --export \
            --output swarmgit-release-key.asc
        echo "signed: $OUT_DIR/SHA256SUMS.asc (public key: swarmgit-release-key.asc)"
        echo "verify: gpg --verify SHA256SUMS.asc SHA256SUMS && sha256sum -c SHA256SUMS"
    else
        echo "not signed (use --sign or SIGN_KEY=<gpg key id> to sign SHA256SUMS)"
    fi
)
