Vendored crates.io iced_tiny_skia 0.14.1 from upstream commit
0ecf60664df7b8ac7d7aef5f7279d5323027f693. The upstream MIT license is included.

The renderer builds a clip mask only when a primitive crosses its clip. It
reuses identical clips and clears only the previous rectangle between updates.
The fast path uses Tiny Skia 0.11's non-antialiased fixed-point edge rounding
for rectangles inside an untiled surface. Fractional edges outside the surface,
thin rectangles and tiled surfaces keep Tiny Skia's original scanner.

The renderer trims solid rectangular fills to the clip's surrounding pixel
boundaries before scanning them. The clip mask and fractional edges are
preserved. On macOS the compositor forwards the grouped damage in physical
pixels to softbuffer.

Text damage also compares editor buffer identity. Upstream editor equality
compares font and dimensions, so two live snippet tabs with different text can
otherwise reuse the preceding tab's pixels. Unchanged buffers retain the
existing damage fast path.

The file editor uses a scoped caret painter with iced_widget 0.14.2.
That TextEditor sends its value color to both the text renderer and the
blinking caret quad. The scope replaces the adapter's marker in those two
draw operations and leaves the widget's layout, selection, events and IME
handling in iced. The block caret repaints the same shaped glyph in a
clipped layer with contrasting ink.
