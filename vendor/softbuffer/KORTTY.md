Vendored crates.io softbuffer 0.4.8 from upstream commit
d871852faa1137d4615b99b9ad31c3ba80f345b5. The original license files are included.

The optional premultiplied-argb feature accepts the alpha byte of the packed
0xAARRGGBB pixels produced by iced_tiny_skia 0.14.1. Wayland submits them as
ARGB8888. AppKit uses straight-alpha Core Graphics images only for nonopaque
native windows and converts the disposable presentation buffer in place.
The feature leaves the default API and opaque AppKit and Win32 paths intact.

AppKit keeps one renderer buffer per surface so Iced can repaint damaged
regions. It advertises age one only after a successful presentation and
invalidates the retained frame after a size or scale change or an abandoned
write. For damage-aware presentation, AppKit divides the physical buffer into
128-pixel horizontal strips. It replaces the immutable Core Animation image
only for each strip that intersects a damage rectangle. Strip edges share
pixel boundaries at the view's backing scale. The straight-alpha conversion
runs on each changed strip image, leaving the renderer's premultiplied pixels
intact. Plain `present()` and UIKit use a single image.
