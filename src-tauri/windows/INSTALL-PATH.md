# Windows install path (3Ready Prototyping Lab)

Publisher: 3Ready Prototyping Lab
Package id: com.threereadylab.<app>
installMode=perMachine installs to:
  C:\Program Files\<productName>\

Nested company folder (enforced by `installer-hooks.nsh`):
  C:\Program Files\3ReadyLab\<productName>\
The canonical Windows release is NSIS only. The hook migrates either historical
default (`Program Files\Basiliskos` or `%LOCALAPPDATA%\Basiliskos`) before the
new files and shortcuts are written, while preserving a genuinely custom path.

Start Menu: 3ReadyLab
