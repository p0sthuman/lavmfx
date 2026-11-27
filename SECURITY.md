# Security

## Reporting a vulnerability

Please report security problems **privately**: use
[GitHub's private vulnerability reporting](https://github.com/p0sthuman/lavmfx/security/advisories/new)
(the repository's **Security** tab › **Report a vulnerability**). Don't open a
public issue.

Include what's affected, how to reproduce it, and what an attacker could do
with it. This is a volunteer project, so there's no fixed response time, but
reports get looked at before regular issues. Once a fix is out, you'll be
credited in the advisory if you want.

## Supported versions

Fixes go into the `main` branch and the next release. Only the latest
release (currently a 0.1.x pre-release) gets security fixes; please update
before reporting.

## What to keep in mind when you run LAVMFX

LAVMFX is made for stage networks and trusted machines. Some features
deliberately accept input from the network or run third-party code:

- **REST server and OSC input** (Preferences › Webserver / OSC) are **off by
  default**. When you turn them on, they have **no authentication**: anyone who
  can reach the port can control the app: trigger clips and columns, change
  any parameter of the composition, and read its state. The REST server listens on all
  network interfaces by default (`0.0.0.0`). On a shared network, set its
  address to `127.0.0.1` or a trusted interface, or put a firewall in front.
- **Art-Net / sACN** input and output are plain UDP with no authentication, as
  the protocols define.
- **FFGL plugins** are native code and run with your user's rights. Only load
  plugins you trust.
- **ISF shaders** run on the GPU. A shader can't access your files, but a broken
  or hostile one can stall the GPU or crash the driver.
- **Compositions and media files** are parsed by LAVMFX and FFmpeg. Crashes or
  hangs caused by a crafted file count as security bugs: please report them.

Reports in scope: anything that lets a file, a network message or a plugin do
more than the feature is meant to allow (code execution, reading or writing
files outside what the user chose, crashes from a crafted file or packet).
