# Builds and installs Luma. Packagers: set DESTDIR and PREFIX.
PREFIX  ?= /usr/local
DESTDIR ?=
BINARY  ?= target/release/luma
APPID   := dev.ridom.Luma

all: build

build:
	cargo build --release --locked

install:
	install -Dm755 $(BINARY) $(DESTDIR)$(PREFIX)/bin/luma
	install -Dm644 data/$(APPID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APPID).desktop
	install -Dm644 data/$(APPID).metainfo.xml $(DESTDIR)$(PREFIX)/share/metainfo/$(APPID).metainfo.xml
	cd data/icons && find hicolor -type f \( -name '*.svg' -o -name '*.png' \) -exec install -Dm644 {} $(DESTDIR)$(PREFIX)/share/icons/{} \;
	install -Dm644 README.md $(DESTDIR)$(PREFIX)/share/doc/luma/README.md
	install -Dm644 CHANGELOG.md $(DESTDIR)$(PREFIX)/share/doc/luma/CHANGELOG.md
	install -Dm644 LICENSE $(DESTDIR)$(PREFIX)/share/licenses/luma/LICENSE

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/luma
	rm -f $(DESTDIR)$(PREFIX)/share/applications/$(APPID).desktop
	rm -f $(DESTDIR)$(PREFIX)/share/metainfo/$(APPID).metainfo.xml
	cd data/icons && find hicolor -type f \( -name '*.svg' -o -name '*.png' \) -exec rm -f $(DESTDIR)$(PREFIX)/share/icons/{} \;
	rm -rf $(DESTDIR)$(PREFIX)/share/doc/luma $(DESTDIR)$(PREFIX)/share/licenses/luma

.PHONY: all build install uninstall
