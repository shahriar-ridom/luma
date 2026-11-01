# Builds and installs File Manager. Packagers: set DESTDIR and PREFIX.
PREFIX  ?= /usr/local
DESTDIR ?=
BINARY  ?= target/release/file-manager
APPID   := dev.filemanager.FileManager

all: build

build:
	cargo build --release --locked

install:
	install -Dm755 $(BINARY) $(DESTDIR)$(PREFIX)/bin/file-manager
	install -Dm644 data/$(APPID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APPID).desktop
	install -Dm644 data/$(APPID).metainfo.xml $(DESTDIR)$(PREFIX)/share/metainfo/$(APPID).metainfo.xml
	cd data/icons && find hicolor -type f -name '*.svg' -exec install -Dm644 {} $(DESTDIR)$(PREFIX)/share/icons/{} \;
	install -Dm644 README.md $(DESTDIR)$(PREFIX)/share/doc/file-manager/README.md
	install -Dm644 CHANGELOG.md $(DESTDIR)$(PREFIX)/share/doc/file-manager/CHANGELOG.md

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/file-manager
	rm -f $(DESTDIR)$(PREFIX)/share/applications/$(APPID).desktop
	rm -f $(DESTDIR)$(PREFIX)/share/metainfo/$(APPID).metainfo.xml
	cd data/icons && find hicolor -type f -name '*.svg' -exec rm -f $(DESTDIR)$(PREFIX)/share/icons/{} \;
	rm -rf $(DESTDIR)$(PREFIX)/share/doc/file-manager

.PHONY: all build install uninstall
