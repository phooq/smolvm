# Third-Party Licenses

smolvm bundles the following third-party libraries in the `lib/` directory.

## libkrun

- **Version:** 1.15.1
- **License:** Apache License 2.0
- **Source:** https://github.com/containers/libkrun
- **Copyright:** The libkrun Authors

Licensed under the Apache License, Version 2.0. You may obtain a copy of the License at:
http://www.apache.org/licenses/LICENSE-2.0

## libkrunfw

- **Version:** 4.x
- **License:** LGPL-2.1-only (library), GPL-2.0-only (bundled Linux kernel)
- **Source:** https://github.com/containers/libkrunfw
- **Copyright:** The libkrunfw Authors

libkrunfw is a library that bundles the Linux kernel for use with libkrun.

### Source Code Availability

In compliance with LGPL-2.1 and GPL-2.0, the complete source code for libkrunfw and the bundled Linux kernel is available at:

- **libkrunfw:** https://github.com/containers/libkrunfw
- **Linux kernel (with patches):** https://github.com/containers/libkrunfw/tree/main/patches

To obtain the exact source code corresponding to the bundled binary, check out the version tag matching the library version from the repository above.

### Your Rights Under LGPL-2.1

You have the right to:
- Use this library in your own projects
- Modify the library and distribute your modifications
- Reverse engineer the library for debugging purposes

If you distribute a modified version of libkrunfw, you must make your modifications available under the same license.
