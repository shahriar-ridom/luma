# Built in CI from the source tarball made by tools/make-source-tarball.sh.
# Crates are fetched from crates.io during %build, so this spec is for
# project-built packages, not for Fedora's own offline build system.
%global debug_package %{nil}

Name:           file-manager
Version:        0.1.0
Release:        1%{?dist}
Summary:        A fast, native GTK4/libadwaita file manager
License:        GPL-3.0-or-later
URL:            https://github.com/shahriar-ridom/luma.git
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  pkgconfig(gtk4) >= 4.12
BuildRequires:  pkgconfig(libadwaita-1) >= 1.5
BuildRequires:  pkgconfig(sqlite3)
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

Requires:       gtk4 >= 4.12
Requires:       libadwaita >= 1.5
Requires:       bsdtar
Requires:       shared-mime-info
Recommends:     git
Recommends:     gvfs
Recommends:     gvfs-fuse
Suggests:       rclone
Suggests:       fuse3
Suggests:       poppler-utils

%description
File Manager pairs a calm, Finder-like interface with the depth of a power-user
file manager: icon, list, column and gallery views, tabs and split view,
Quick Look, archives, search, tags, Git status, Google Drive and network
locations, undo, plugins and custom actions.

%prep
%autosetup

%build
export CARGO_HOME=%{_builddir}/cargo-home
cargo build --release --locked

%install
make install DESTDIR=%{buildroot} PREFIX=%{_prefix}

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/dev.filemanager.FileManager.desktop

%files
%{_bindir}/file-manager
%{_datadir}/applications/dev.filemanager.FileManager.desktop
%{_datadir}/metainfo/dev.filemanager.FileManager.metainfo.xml
%{_datadir}/icons/hicolor/scalable/*
%doc %{_docdir}/file-manager

%changelog
* Mon Nov 02 2026 File Manager contributors <noreply@example.com> - 0.1.0-1
- First release
