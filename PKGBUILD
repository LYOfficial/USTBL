# Maintainer: USTBL <ustb@ustb.world>
# Maintainer: xpe-online <xpecnh2n@gmail.com>
# Maintainer: raindropqwq <raindropqwq@outlook.com>

pkgname=ustbl-bin
pkgdesc='🌟 A Minecraft launcher for USTB Servers'
pkgver=0.2.0
_github_pkgver=0.2.0
pkgrel=1
arch=('x86_64' 'aarch64')
license=(GPL-3.0,custom:LICENSE.EXTRA)
url='https://github.com/USTB-SkyCode/USTBL'
_baseurl="${url}/releases/download/v${_github_pkgver}"
_source="USTBL_${_github_pkgver}_linux_${CARCH}.deb"

sha512sums=('SKIP')
sha512sums_x86_64=('SKIP')
sha512sums_aarch64=('SKIP')

source=('LICENSE.EXTRA')
source_x86_64=("${_baseurl}/${_source}")
source_aarch64=("${_baseurl}/${_source}")
depends=('cairo' 'desktop-file-utils' 'gdk-pixbuf2' 'glib2' 'gtk3' 'hicolor-icon-theme' 'libsoup' 'pango' 'webkit2gtk-4.1')
options=('!strip' '!emptydirs')
provides=('ustbl')
conflicts=('ustbl')

package() {
  bsdtar -xf data.tar.gz -C "${pkgdir}"
  chmod +x ${pkgdir}/usr/bin/USTBL
  install -Dm 644 "${srcdir}/LICENSE.EXTRA" "${pkgdir}/usr/share/licenses/${pkgname}/LICENSE.EXTRA"
}
