#!/usr/bin/env bash
# Build .deb and .rpm packages for the `swarm` binary.
#
#   scripts/package.sh                  # both formats, into ./dist
#   scripts/package.sh --deb            # only the .deb
#   scripts/package.sh --rpm -o /tmp/pkgs
#   VERSION=0.2.0 scripts/package.sh    # override the Cargo.toml version
#
# Everything happens inside Dockerfile.package, so the host needs neither a
# Rust toolchain nor dpkg-dev/rpmbuild. The repository is bind-mounted, so the
# release build is cached in target/package and the packages land in the
# output directory on the host.
#
# Environment:
#   OUT_DIR     output directory (default: dist)
#   VERSION     overrides the workspace version from Cargo.toml
#   RELEASE     RPM release number (default: 1)
#   MAINTAINER  "Name <email>" for the .deb (default: git config user, then a
#               placeholder)

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

IMAGE=swarmgit-package
OUT_DIR=${OUT_DIR:-dist}
RELEASE=${RELEASE:-1}
VERSION_OVERRIDE=${VERSION:-}
WANT_DEB=${WANT_DEB:-0}
WANT_RPM=${WANT_RPM:-0}
SELECTED=0

usage() {
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while (($#)); do
    case "$1" in
    --deb)
        WANT_DEB=1
        SELECTED=1
        ;;
    --rpm)
        WANT_RPM=1
        SELECTED=1
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

if ((!SELECTED && ${SWARM_PKG_INSIDE:-0} != 1)); then
    WANT_DEB=1
    WANT_RPM=1
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

if [[ -z ${MAINTAINER:-} ]]; then
    name=$(git config user.name 2>/dev/null || true)
    email=$(git config user.email 2>/dev/null || true)
    if [[ -n $name && -n $email ]]; then
        MAINTAINER="$name <$email>"
    else
        MAINTAINER="Swarmgit Maintainers <maintainers@swarmgit.invalid>"
    fi
fi

# ---------------------------------------------------------------------------
# Inside the packaging container.
# ---------------------------------------------------------------------------
if [[ ${SWARM_PKG_INSIDE:-} == 1 ]]; then
    export CARGO_TARGET_DIR=/workspace/target/package
    BIN=$CARGO_TARGET_DIR/release/swarm
    DEB_ARCH=$(dpkg --print-architecture)
    RPM_ARCH=$(rpm --eval '%{_arch}')
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT

    echo "== cargo build --release (version $VERSION)"
    cargo build --release --locked -p swarm-cli

    package_deb() {
        local stage=$WORK/deb/swarmgit
        mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/doc/swarmgit"
        install -m 0755 "$BIN" "$stage/usr/bin/swarm"
        ln -sfn swarm "$stage/usr/bin/git-remote-swarm"
        for doc in README.md CHANGELOG.md; do
            [[ -f $doc ]] && install -m 0644 "$doc" "$stage/usr/share/doc/swarmgit/$doc"
        done

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

        mkdir -p "$OUT_DIR"
        dpkg-deb --build --root-owner-group "$stage" \
            "$OUT_DIR/swarmgit_${VERSION}_${DEB_ARCH}.deb"
    }

    package_rpm() {
        local top=$WORK/rpmbuild src=$WORK/rpm-src spec=$WORK/rpmbuild/SPECS/swarmgit.spec
        mkdir -p "$top"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS} "$src"
        install -m 0755 "$BIN" "$src/swarm"
        [[ -f README.md ]] && install -m 0644 README.md "$src/README.md"

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
License:        MIT OR Apache-2.0
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
EOF
        [[ -f $src/README.md ]] && echo '%{_docdir}/swarmgit/README.md' >>"$spec"

        rpmbuild -bb --quiet --define "_topdir $top" "$spec"

        local built
        built=$(find "$top/RPMS" -type f -name '*.rpm' | head -n1)
        [[ -n $built ]] || {
            echo "rpmbuild produced no package" >&2
            exit 1
        }
        mkdir -p "$OUT_DIR"
        install -m 0644 "$built" \
            "$OUT_DIR/swarmgit-${RPM_VERSION}-${RELEASE}.${RPM_ARCH}.rpm"
    }

    if ((WANT_DEB)); then package_deb; fi
    if ((WANT_RPM)); then package_rpm; fi

    echo
    echo "packages in $OUT_DIR:"
    find "$OUT_DIR" -maxdepth 1 -type f \( -name '*.deb' -o -name '*.rpm' \) \
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
docker build -f Dockerfile.package -t "$IMAGE" \
    --build-arg USER_ID="$(id -u)" \
    --build-arg GROUP_ID="$(id -g)" \
    .

echo "== packaging swarmgit $VERSION"
exec docker run --rm -i \
    -e SWARM_PKG_INSIDE=1 \
    -e VERSION="$VERSION" \
    -e RELEASE="$RELEASE" \
    -e OUT_DIR="$CONTAINER_OUT" \
    -e WANT_DEB="$WANT_DEB" \
    -e WANT_RPM="$WANT_RPM" \
    -e MAINTAINER="$MAINTAINER" \
    -v "$PWD":/workspace \
    "${OUT_MOUNT[@]}" \
    -v swarmgit-cargo-registry:/usr/local/cargo/registry \
    -v swarmgit-cargo-git:/usr/local/cargo/git \
    "$IMAGE" scripts/package.sh
