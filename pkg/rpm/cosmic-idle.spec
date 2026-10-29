Name:           cosmic-idle
Epoch:          1
Version: 1.0.0
Release:        1%{?dist}
Summary:        COSMIC Idle - Idle daemon for the COSMIC desktop (Playtron fork)

License:        GPL-3.0-only
URL:            https://github.com/playtron-os/cosmic-idle
Source0:        %{name}-%{_arch}.tar.gz

# Override the upstream cosmic-idle
Provides:       cosmic-idle = %{epoch}:%{version}-%{release}
Obsoletes:      cosmic-idle < %{epoch}:%{version}

%description
Idle daemon for the COSMIC desktop environment.
Turns the screens off, locks and suspends after idle time, and serves
org.freedesktop.ScreenSaver, including the screen lock state.

%prep
%autosetup -n %{name} -p1

%build

%install
install -Dm0755 "usr/bin/cosmic-idle" "%{buildroot}%{_bindir}/cosmic-idle"
install -Dm0644 "usr/share/licenses/cosmic-idle/LICENSE" "%{buildroot}%{_datadir}/licenses/cosmic-idle/LICENSE"

%files
%license %{_datadir}/licenses/cosmic-idle/LICENSE
%{_bindir}/cosmic-idle

%changelog
* Wed Oct 28 2026 Playtron <dev@playtron.one> - 1.11.0-1
- Initial RPM package for Playtron fork
