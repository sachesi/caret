%define _debugsource_template %{nil}
%define debug_package %{nil}

%global app_id io.github.sachesi.caret

Name:           caret
# The OBS workflow sets Version to the tag it builds; OBS counts the Release.
Version:        0.3.3
Release:        0
Summary:        GPU-rendered terminal for Wayland

License:        GPL-3.0-or-later
URL:            https://github.com/sachesi/caret
# Named as the Debian source package names them, which OBS builds from the same files.
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}_%{version}.orig.tar.gz
# The crates the build needs, from the release, so that it runs without a network.
Source1:        %{url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.xz#/%{name}_%{version}.orig-vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  rust >= 1.92
BuildRequires:  gcc
BuildRequires:  systemd-rpm-macros
BuildRequires:  shaderc
BuildRequires:  desktop-file-utils
BuildRequires:  gettext-tools
BuildRequires:  AppStream
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  pkgconfig(xkbcommon)
# Owns the icon directories, which OBS checks the package's files against.
BuildRequires:  hicolor-icon-theme

Requires:       hicolor-icon-theme
# The renderer opens libvulkan.so.1 itself, which the automatic dependencies do not see.
Requires:       libvulkan1
Suggests:       xdg-terminal-exec
# The name before 0.2.0.
Obsoletes:      tangent < 0.2.0
Provides:       tangent = %{version}-%{release}

%description
Caret is a terminal for Wayland. Programs' output is parsed by a terminal
emulator of Caret's own, and Caret draws its window and text with Vulkan, one
texel to each pixel of the screen, so it stays sharp at fractional scales.
It has tabs, split panes, sessions whose programs go on running when their
window is detached, search through the history, links opened with Ctrl and a
click, and follows the desktop's font and style.

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
install -Dpm 0755 "$target/caret-server" %{buildroot}%{_bindir}/caret-server
install -Dpm 0644 data/caret-server.socket %{buildroot}%{_userunitdir}/caret-server.socket
sed 's|@bindir@|%{_bindir}|' data/caret-server.service.in > caret-server.service
install -Dpm 0644 caret-server.service %{buildroot}%{_userunitdir}/caret-server.service

install -d %{buildroot}%{_datadir}/applications %{buildroot}%{_datadir}/metainfo
msgfmt --desktop --template=data/%{app_id}.desktop -d po \
  -o %{buildroot}%{_datadir}/applications/%{app_id}.desktop
msgfmt --xml --template=data/%{app_id}.metainfo.xml -d po \
  -o %{buildroot}%{_datadir}/metainfo/%{app_id}.metainfo.xml
install -Dpm 0644 data/icons/hicolor/scalable/apps/%{app_id}.svg \
  %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
install -Dpm 0644 data/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg \
  %{buildroot}%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg

"$target/caret" completions bash | install -Dm 0644 /dev/stdin %{buildroot}%{_datadir}/bash-completion/completions/caret
"$target/caret" completions zsh | install -Dm 0644 /dev/stdin %{buildroot}%{_datadir}/zsh/site-functions/_caret
"$target/caret" completions fish | install -Dm 0644 /dev/stdin %{buildroot}%{_datadir}/fish/vendor_completions.d/caret.fish

for lang in $(cat po/LINGUAS); do
  install -d %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES
  msgfmt -o %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES/%{name}.mo po/$lang.po
done
%find_lang %{name}

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
appstreamcli validate --no-net %{buildroot}%{_datadir}/metainfo/%{app_id}.metainfo.xml
test -x %{buildroot}%{_bindir}/caret
test -x %{buildroot}%{_bindir}/caret-server

%files -f %{name}.lang
%license LICENSE
%doc README.md docs
%{_bindir}/caret
%{_bindir}/caret-server
%{_userunitdir}/caret-server.socket
%{_userunitdir}/caret-server.service
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/metainfo/%{app_id}.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg
%dir %{_datadir}/bash-completion
%dir %{_datadir}/bash-completion/completions
%dir %{_datadir}/zsh
%dir %{_datadir}/zsh/site-functions
%dir %{_datadir}/fish
%dir %{_datadir}/fish/vendor_completions.d
%{_datadir}/bash-completion/completions/caret
%{_datadir}/zsh/site-functions/_caret
%{_datadir}/fish/vendor_completions.d/caret.fish

%changelog
