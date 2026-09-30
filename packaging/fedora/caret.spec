%define _debugsource_template %{nil}
%define debug_package %{nil}

%global app_id io.github.sachesi.caret

Name:           caret
# Copr's script for the package sets Version to the tag it builds.
Version:        0.1.0
Release:        1%{?dist}
Summary:        GPU-rendered terminal for GTK 4 and libadwaita

License:        GPL-3.0-or-later
URL:            https://github.com/sachesi/caret
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz
# The crates the build needs, from the release, so that it runs without a network.
Source1:        %{url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  rust >= 1.92
BuildRequires:  gcc
BuildRequires:  blueprint-compiler
BuildRequires:  desktop-file-utils
BuildRequires:  gettext
BuildRequires:  appstream
BuildRequires:  pkgconfig(gtk4) >= 4.22
BuildRequires:  pkgconfig(libadwaita-1) >= 1.9
BuildRequires:  pkgconfig(glib-2.0) >= 2.80

Requires:       gtk4%{?_isa} >= 4.22
Requires:       libadwaita%{?_isa} >= 1.9
Requires:       hicolor-icon-theme
# The renderer opens libEGL.so.1 itself, which the automatic dependencies do not see.
Requires:       libglvnd-egl%{?_isa}
Suggests:       xdg-terminal-exec
# The name before 0.2.0.
Obsoletes:      tangent < 0.2.0
Provides:       tangent = %{version}-%{release}

%description
Caret is a terminal built with GTK 4 and libadwaita. Programs' output is
parsed by alacritty_terminal, and Caret draws the text itself with OpenGL,
one texel to each pixel of the screen, so it stays sharp at fractional scales.
It has tabs, search through the history, links opened with Ctrl and a click,
and follows the desktop's font and style.

%prep
%autosetup -n %{name}-%{version} -b 1

%build
export CARGO_HOME="$PWD/.cargo-home"
export RUSTFLAGS="%{?build_rustflags}"
export CARET_LOCALEDIR="%{_datadir}/locale"
%if 0%{?_cargo_target_dir:1}
export CARGO_TARGET_DIR="%{_cargo_target_dir}"
%endif
cargo build --release --offline --locked

%install
%if 0%{?_cargo_target_dir:1}
target="%{_cargo_target_dir}/release"
%else
target="target/release"
%endif
install -Dpm 0755 "$target/caret" %{buildroot}%{_bindir}/caret

install -d %{buildroot}%{_datadir}/applications %{buildroot}%{_metainfodir}
msgfmt --desktop --template=data/%{app_id}.desktop -d po \
  -o %{buildroot}%{_datadir}/applications/%{app_id}.desktop
msgfmt --xml --template=data/%{app_id}.metainfo.xml -d po \
  -o %{buildroot}%{_metainfodir}/%{app_id}.metainfo.xml
install -Dpm 0644 data/%{app_id}.gschema.xml %{buildroot}%{_datadir}/glib-2.0/schemas/%{app_id}.gschema.xml
install -Dpm 0644 data/icons/hicolor/scalable/apps/%{app_id}.svg \
  %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
install -Dpm 0644 data/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg \
  %{buildroot}%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg

for lang in $(cat po/LINGUAS); do
  install -d %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES
  msgfmt -o %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES/%{name}.mo po/$lang.po
done
%find_lang %{name}

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
appstreamcli validate --no-net %{buildroot}%{_metainfodir}/%{app_id}.metainfo.xml
test -x %{buildroot}%{_bindir}/caret

%files -f %{name}.lang
%license LICENSE
%doc README.md docs
%{_bindir}/caret
%{_datadir}/applications/%{app_id}.desktop
%{_metainfodir}/%{app_id}.metainfo.xml
%{_datadir}/glib-2.0/schemas/%{app_id}.gschema.xml
%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg

%changelog
