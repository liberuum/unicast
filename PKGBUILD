# Maintainer: liberuum
pkgname=omarchy-castd
pkgver=0.1.1
pkgrel=1
pkgdesc="UniCast backend: cast local media to DLNA, Google Cast and AirPlay receivers"
arch=('x86_64' 'aarch64')
url="https://github.com/liberuum/unicast"
license=('MIT')
depends=('ffmpeg')
makedepends=('cargo' 'cmake' 'clang' 'git')
provides=('omarchy-castd')
source=("$pkgname-$pkgver.tar.gz::$url/archive/refs/tags/v$pkgver.tar.gz")
# Update on release: updpkgsums
sha256sums=('SKIP')

build() {
  cd "$srcdir/unicast-$pkgver"
  cargo build --release --locked
}

package() {
  cd "$srcdir/unicast-$pkgver"
  install -Dm755 target/release/omarchy-castd "$pkgdir/usr/bin/omarchy-castd"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
