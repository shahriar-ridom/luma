# Built in CI from the source tarball made by tools/make-source-tarball.sh.
# Crates are fetched from crates.io during %build, so this spec is for
# project-built packages, not for Fedora's own offline build system.
%global debug_package %{nil}

Name:           luma
Version:        0.3.0
Release:        1%{?dist}
Summary:        A fast, native GTK4/libadwaita file manager
License:        GPL-3.0-only
URL:            https://github.com/shahriar-ridom/luma
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
Luma pairs a calm, Finder-like interface with the depth of a power-user
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
desktop-file-validate %{buildroot}%{_datadir}/applications/dev.ridom.Luma.desktop

%files
%{_bindir}/luma
%{_datadir}/applications/dev.ridom.Luma.desktop
%{_datadir}/metainfo/dev.ridom.Luma.metainfo.xml
%{_datadir}/icons/hicolor/scalable/*
%{_datadir}/icons/hicolor/256x256/*
%doc %{_docdir}/luma
%license %{_datadir}/licenses/luma/LICENSE

%changelog
* Mon Nov 02 2026 Luma contributors <info@ridom.dev> - 0.1.0-1
- First release
