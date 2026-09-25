# Maintainer: liberuum
pkgname=omarchy-castd
pkgver=0.1.1
pkgrel=1
pkgdesc="UniCast backend: cast local media to DLNA, Google Cast and AirPlay receivers"
arch=('x86_64' 'aarch64')
url="https://github.com/liberuum/unicast"
license=('MIT')
depends=('ffmpeg')
makedepends=('cargo' 'cmake' 'clang')
optdepends=('ufw: per-receiver firewall rule for the media port'
            'polkit: authorizes that firewall rule')
source=("$pkgname-$pkgver.tar.gz::$url/archive/refs/tags/v$pkgver.tar.gz")
# Tracks the newest tag whose tarball checksum is known; after tagging a
# release, bump pkgver and run updpkgsums.
sha256sums=('30c3f47969760d0f807379198d056ba291986df6d58bbbe99002d190f154a1f9')

prepare() {
  cd "$srcdir/unicast-$pkgver"
  # Fetch the locked crates up front so build() works offline (clean chroot).
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$srcdir/unicast-$pkgver"
  export CARGO_TARGET_DIR=target
  cargo build --frozen --release
}

package() {
  cd "$srcdir/unicast-$pkgver"
  install -Dm755 target/release/omarchy-castd "$pkgdir/usr/bin/omarchy-castd"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
