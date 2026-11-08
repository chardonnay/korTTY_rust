Vendored crates.io smithay-clipboard 0.7.3 from upstream commit
26c2f53f15f6bdc4f41a442d0ae2c2d63bbc617c. The original MIT license is included.

The drag module lets the app request an in-process move on the already-owned
Wayland data device. Some compositors choose a single data device resource per
client for drag events, so reusing the clipboard worker avoids a competing
device. The app supplies a random per-gesture MIME identifier and never sends
or reads a drag payload.

The same worker supports publishing `image/png` from a focused kortty surface
using its observed keyboard or pointer serial. Each regular clipboard source
owns its MIME payload. The primary selection remains text. Image requests and
nonblocking paste transfers have bounded concurrency and size.
