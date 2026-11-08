//! Softbuffer implementation using CoreGraphics.
use crate::backend_interface::*;
use crate::error::InitError;
use crate::{util, Rect, SoftBufferError};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, MainThreadMarker, Message};
use objc2_core_foundation::{CFRetained, CGPoint};
#[cfg(target_os = "macos")]
use objc2_core_foundation::{CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGImageComponentInfo, CGImagePixelFormatInfo,
};
use objc2_foundation::{
    ns_string, NSDictionary, NSKeyValueChangeKey, NSKeyValueChangeNewKey,
    NSKeyValueObservingOptions, NSNumber, NSObject, NSObjectNSKeyValueObserverRegistration,
    NSString, NSValue,
};
use objc2_quartz_core::{kCAGravityTopLeft, CALayer, CATransaction};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::size_of;
use std::num::NonZeroU32;
use std::ops::Deref;
use std::ptr::{self, slice_from_raw_parts_mut, NonNull};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "macos")]
const STRIP_HEIGHT: usize = 128;

#[derive(Debug)]
struct ObserverState {
    layer: SendCALayer,
    #[cfg(target_os = "macos")]
    scale_changed: AtomicBool,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SoftbufferObserver"]
    #[ivars = ObserverState]
    #[derive(Debug)]
    struct Observer;

    /// NSKeyValueObserving
    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut c_void,
        ) {
            self.update(key_path, change);
        }
    }
);

impl Observer {
    fn new(layer: &CALayer) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ObserverState {
            layer: SendCALayer(layer.retain()),
            #[cfg(target_os = "macos")]
            scale_changed: AtomicBool::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn update(
        &self,
        key_path: Option<&NSString>,
        change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
    ) {
        let layer = &self.ivars().layer;

        let change =
            change.expect("requested a change dictionary in `addObserver`, but none was provided");
        let new = change
            .objectForKey(unsafe { NSKeyValueChangeNewKey })
            .expect("requested change dictionary did not contain `NSKeyValueChangeNewKey`");

        // NOTE: Setting these values usually causes a quarter second animation to occur, which is
        // undesirable.
        //
        // However, since we're setting them inside an observer, there already is a transaction
        // ongoing, and as such we don't need to wrap this in a `CATransaction` ourselves.

        if key_path == Some(ns_string!("contentsScale")) {
            let new = new.downcast::<NSNumber>().unwrap();
            let scale_factor = new.as_cgfloat();

            // Set the scale factor of the layer to match the root layer when it changes (e.g. if
            // moved to a different monitor, or monitor settings changed).
            #[cfg(target_os = "macos")]
            if layer.contentsScale() != scale_factor {
                self.ivars().scale_changed.store(true, Ordering::Relaxed);
            }
            layer.setContentsScale(scale_factor);
        } else if key_path == Some(ns_string!("bounds")) {
            let new = new.downcast::<NSValue>().unwrap();
            let bounds = new.get_rect().expect("new bounds value was not CGRect");

            // Set `bounds` and `position` so that the new layer is inside the superlayer.
            //
            // This differs from just setting the `bounds`, as it also takes into account any
            // translation that the superlayer may have that we'd want to preserve.
            layer.setFrame(bounds);
        } else {
            panic!("unknown observed keypath {key_path:?}");
        }
    }
}

#[derive(Debug)]
pub struct CGImpl<D, W> {
    /// Our layer.
    layer: SendCALayer,
    /// The layer that our layer was created from.
    ///
    /// Can also be retrieved from `layer.superlayer()`.
    root_layer: SendCALayer,
    observer: Retained<Observer>,
    color_space: CFRetained<CGColorSpace>,
    alpha: bool,
    /// The width of the underlying buffer.
    width: usize,
    /// The height of the underlying buffer.
    height: usize,
    // Keep the renderer's pixels separate from the immutable CGImage retained by CALayer.
    pixels: util::PixelBuffer,
    presented: bool,
    #[cfg(target_os = "macos")]
    strips: Option<Strips>,
    window_handle: W,
    _display: PhantomData<D>,
}

impl<D, W> Drop for CGImpl<D, W> {
    fn drop(&mut self) {
        // SAFETY: Registered in `new`, must be removed before the observer is deallocated.
        unsafe {
            self.root_layer
                .removeObserver_forKeyPath(&self.observer, ns_string!("contentsScale"));
            self.root_layer
                .removeObserver_forKeyPath(&self.observer, ns_string!("bounds"));
        }
        #[cfg(target_os = "macos")]
        self.layer.removeFromSuperlayer();
    }
}

impl<D: HasDisplayHandle, W: HasWindowHandle> SurfaceInterface<D, W> for CGImpl<D, W> {
    type Context = D;
    type Buffer<'a>
        = BufferImpl<'a, D, W>
    where
        Self: 'a;

    fn new(window_src: W, _display: &D) -> Result<Self, InitError<W>> {
        // `NSView`/`UIView` can only be accessed from the main thread.
        let _mtm = MainThreadMarker::new().ok_or(SoftBufferError::PlatformError(
            Some("can only access Core Graphics handles from the main thread".to_string()),
            None,
        ))?;

        let (root_layer, alpha) = match window_src.window_handle()?.as_raw() {
            RawWindowHandle::AppKit(handle) => {
                // SAFETY: The pointer came from `WindowHandle`, which ensures that the
                // `AppKitWindowHandle` contains a valid pointer to an `NSView`.
                //
                // We use `NSObject` here to avoid importing `objc2-app-kit`.
                let view: &NSObject = unsafe { handle.ns_view.cast().as_ref() };

                let alpha = cfg!(all(
                    feature = "premultiplied-argb",
                    target_endian = "little"
                )) && {
                    // SAFETY: This is an NSView on the main thread. Its optional NSWindow
                    // stays retained while reading its opacity flag.
                    let window: Option<Retained<NSObject>> = unsafe { msg_send![view, window] };
                    window.is_some_and(|window| {
                        let opaque: Bool = unsafe { msg_send![&*window, isOpaque] };
                        !opaque.as_bool()
                    })
                };

                // Force the view to become layer backed
                let _: () = unsafe { msg_send![view, setWantsLayer: Bool::YES] };

                // SAFETY: `-[NSView layer]` returns an optional `CALayer`
                let layer: Option<Retained<CALayer>> = unsafe { msg_send![view, layer] };
                (layer.expect("failed making the view layer-backed"), alpha)
            }
            RawWindowHandle::UiKit(handle) => {
                // SAFETY: The pointer came from `WindowHandle`, which ensures that the
                // `UiKitWindowHandle` contains a valid pointer to an `UIView`.
                //
                // We use `NSObject` here to avoid importing `objc2-ui-kit`.
                let view: &NSObject = unsafe { handle.ui_view.cast().as_ref() };

                // SAFETY: `-[UIView layer]` returns `CALayer`
                let layer: Retained<CALayer> = unsafe { msg_send![view, layer] };
                (layer, false)
            }
            _ => return Err(InitError::Unsupported(window_src)),
        };

        // Add a sublayer, to avoid interfering with the root layer, since setting the contents of
        // e.g. a view-controlled layer is brittle.
        let layer = CALayer::new();
        if alpha {
            root_layer.setOpaque(false);
            layer.setOpaque(false);
        }
        root_layer.addSublayer(&layer);

        // Set the anchor point and geometry. Softbuffer's uses a coordinate system with the origin
        // in the top-left corner.
        //
        // NOTE: This doesn't really matter unless we start modifying the `position` of our layer
        // ourselves, but it's nice to have in place.
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        layer.setGeometryFlipped(true);

        // Do not use auto-resizing mask.
        //
        // This is done to work around a bug in macOS 14 and above, where views using auto layout
        // may end up setting fractional values as the bounds, and that in turn doesn't propagate
        // properly through the auto-resizing mask and with contents gravity.
        //
        // Instead, we keep the bounds of the layer in sync with the root layer using an observer,
        // see below.
        //
        // layer.setAutoresizingMask(kCALayerHeightSizable | kCALayerWidthSizable);

        let observer = Observer::new(&layer);
        // Observe changes to the root layer's bounds and scale factor, and apply them to our layer.
        //
        // The previous implementation updated the scale factor inside `resize`, but this works
        // poorly with transactions, and is generally inefficient. Instead, we update the scale
        // factor only when needed because the super layer's scale factor changed.
        //
        // Note that inherent in this is an explicit design decision: We control the `bounds` and
        // `contentsScale` of the layer directly, and instead let the `resize` call that the user
        // controls only be the size of the underlying buffer.
        //
        // SAFETY: Observer deregistered in `Drop` before the observer object is deallocated.
        unsafe {
            root_layer.addObserver_forKeyPath_options_context(
                &observer,
                ns_string!("contentsScale"),
                NSKeyValueObservingOptions::New | NSKeyValueObservingOptions::Initial,
                ptr::null_mut(),
            );
            root_layer.addObserver_forKeyPath_options_context(
                &observer,
                ns_string!("bounds"),
                NSKeyValueObservingOptions::New | NSKeyValueObservingOptions::Initial,
                ptr::null_mut(),
            );
        }

        // Set the content so that it is placed in the top-left corner if it does not have the same
        // size as the surface itself.
        //
        // TODO(madsmtm): Consider changing this to `kCAGravityResize` to stretch the content if
        // resized to something that doesn't fit, see #177.
        layer.setContentsGravity(unsafe { kCAGravityTopLeft });

        // Initialize color space here, to reduce work later on.
        let color_space = CGColorSpace::new_device_rgb().unwrap();

        // Grab initial width and height from the layer (whose properties have just been initialized
        // by the observer using `NSKeyValueObservingOptionInitial`).
        let size = layer.bounds().size;
        let scale_factor = layer.contentsScale();
        let width = (size.width * scale_factor) as usize;
        let height = (size.height * scale_factor) as usize;

        Ok(Self {
            layer: SendCALayer(layer),
            root_layer: SendCALayer(root_layer),
            observer,
            color_space,
            alpha,
            width,
            height,
            pixels: util::PixelBuffer(Vec::new()),
            presented: false,
            #[cfg(target_os = "macos")]
            strips: None,
            _display: PhantomData,
            window_handle: window_src,
        })
    }

    #[inline]
    fn window(&self) -> &W {
        &self.window_handle
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), SoftBufferError> {
        let size = (width.get() as usize, height.get() as usize);
        if (self.width, self.height) != size {
            (self.width, self.height) = size;
            self.pixels.0 = Vec::new();
            self.presented = false;
        }
        Ok(())
    }

    fn buffer_mut(&mut self) -> Result<BufferImpl<'_, D, W>, SoftBufferError> {
        #[cfg(target_os = "macos")]
        if self
            .observer
            .ivars()
            .scale_changed
            .swap(false, Ordering::Relaxed)
        {
            self.presented = false;
        }
        self.pixels.0.resize(self.width * self.height, 0);
        Ok(BufferImpl {
            age: u8::from(self.presented),
            imp: self,
        })
    }
}

#[derive(Debug)]
pub struct BufferImpl<'a, D, W> {
    imp: &'a mut CGImpl<D, W>,
    age: u8,
}

impl<D: HasDisplayHandle, W: HasWindowHandle> BufferInterface for BufferImpl<'_, D, W> {
    fn width(&self) -> NonZeroU32 {
        NonZeroU32::new(self.imp.width as u32).unwrap()
    }

    fn height(&self) -> NonZeroU32 {
        NonZeroU32::new(self.imp.height as u32).unwrap()
    }

    #[inline]
    fn pixels(&self) -> &[u32] {
        &self.imp.pixels
    }

    #[inline]
    fn pixels_mut(&mut self) -> &mut [u32] {
        // Dropping a writable lease without presenting leaves unspecified pixels.
        self.imp.presented = false;
        &mut self.imp.pixels
    }

    fn age(&self) -> u8 {
        self.age
    }

    fn present(self) -> Result<(), SoftBufferError> {
        let surface = self.imp;
        if surface.presented {
            // Iced also presents frames whose damage list is empty.
            return Ok(());
        }
        let pixels = if cfg!(target_os = "macos") {
            // CGImage can outlive this frame. In transparent windows layer_image
            // converts its copy to straight alpha, leaving the render buffer intact.
            surface.pixels.0.clone()
        } else {
            std::mem::take(&mut surface.pixels.0)
        };
        let image = layer_image(
            pixels.into_boxed_slice(),
            surface.width,
            surface.height,
            &surface.color_space,
            surface.alpha,
        );

        // The CALayer has a default action associated with a change in the layer contents, causing
        // a quarter second fade transition to happen every time a new buffer is applied. This can
        // be avoided by wrapping the operation in a transaction and disabling all actions.
        CATransaction::begin();
        CATransaction::setDisableActions(true);

        #[cfg(target_os = "macos")]
        if let Some(strips) = surface.strips.take() {
            strips.detach();
        }
        // SAFETY: The contents is `CGImage`, which is a valid class for `contents`.
        unsafe { surface.layer.setContents(Some(image.as_ref())) };

        CATransaction::commit();
        surface.presented = cfg!(target_os = "macos");
        Ok(())
    }

    fn present_with_damage(self, damage: &[Rect]) -> Result<(), SoftBufferError> {
        #[cfg(target_os = "macos")]
        if self.imp.height > STRIP_HEIGHT {
            present_strips(self.imp, self.age, damage);
            return Ok(());
        }
        let _ = damage;
        self.present()
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct Strips {
    width: usize,
    height: usize,
    scale: f64,
    layers: Vec<SendCALayer>,
}

#[cfg(target_os = "macos")]
impl Strips {
    fn attach(parent: &CALayer, width: usize, height: usize, scale: f64) -> Self {
        // SAFETY: A null contents value clears the previous full-image presentation.
        unsafe { parent.setContents(None) };
        let layers = (0..strip_count(height))
            .map(|index| {
                let layer = CALayer::new();
                layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
                layer.setGeometryFlipped(true);
                layer.setContentsScale(scale);
                layer.setContentsGravity(unsafe { kCAGravityTopLeft });
                layer.setAllowsEdgeAntialiasing(false);
                layer.setFrame(strip_frame(index, width, height, scale));
                parent.addSublayer(&layer);
                SendCALayer(layer)
            })
            .collect();
        Self {
            width,
            height,
            scale,
            layers,
        }
    }

    fn detach(self) {
        for layer in self.layers {
            layer.removeFromSuperlayer();
        }
    }

    fn update(&self, pixels: &[u32], dirty: &[bool], color_space: &CGColorSpace, alpha: bool) {
        let (width, height) = (self.width, self.height);
        for (index, layer) in self.layers.iter().enumerate().filter(|(i, _)| dirty[*i]) {
            let top = index * STRIP_HEIGHT;
            let bottom = (top + STRIP_HEIGHT).min(height);
            let pixels = pixels[top * width..bottom * width]
                .to_vec()
                .into_boxed_slice();
            let image = layer_image(pixels, width, bottom - top, color_space, alpha);
            // SAFETY: `CGImage` is valid CALayer contents and owns this immutable strip copy.
            unsafe { layer.setContents(Some(image.as_ref())) };
        }
    }
}

#[cfg(target_os = "macos")]
fn strip_count(height: usize) -> usize {
    height.saturating_sub(1) / STRIP_HEIGHT + usize::from(height != 0)
}

#[cfg(target_os = "macos")]
fn strip_frame(index: usize, width: usize, height: usize, scale: f64) -> CGRect {
    let top = index * STRIP_HEIGHT;
    let bottom = (top + STRIP_HEIGHT).min(height);
    // The child layers are positioned in the parent's bottom-up coordinates;
    // each CGImage itself still starts at the top. Use a shared pixel boundary
    // for adjacent layers, including the shorter final strip.
    CGRect::new(
        CGPoint::new(0.0, (height - bottom) as f64 / scale),
        CGSize::new(
            width as f64 / scale,
            bottom as f64 / scale - top as f64 / scale,
        ),
    )
}

#[cfg(target_os = "macos")]
fn damaged_strips(width: usize, height: usize, damage: &[Rect]) -> Vec<bool> {
    let mut dirty = vec![false; strip_count(height)];
    for rect in damage {
        let top = rect.y as usize;
        if rect.x as usize >= width || top >= height {
            continue;
        }
        let bottom = top.saturating_add(rect.height.get() as usize).min(height);
        dirty[top / STRIP_HEIGHT..=(bottom - 1) / STRIP_HEIGHT].fill(true);
    }
    dirty
}

#[cfg(target_os = "macos")]
fn present_strips<D, W>(surface: &mut CGImpl<D, W>, age: u8, damage: &[Rect]) {
    if surface.presented {
        return;
    }
    let (width, height) = (surface.width, surface.height);
    let scale = surface.layer.contentsScale();
    let rebuild = !surface.strips.as_ref().is_some_and(|strips| {
        (strips.width, strips.height, strips.scale) == (width, height, scale)
    });
    let dirty = if rebuild || age == 0 {
        vec![true; strip_count(height)]
    } else {
        damaged_strips(width, height, damage)
    };
    if dirty.iter().any(|&changed| changed) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if rebuild {
            if let Some(strips) = surface.strips.take() {
                strips.detach();
            }
        }
        let strips = surface
            .strips
            .get_or_insert_with(|| Strips::attach(&surface.layer, width, height, scale));
        strips.update(&surface.pixels, &dirty, &surface.color_space, surface.alpha);
        CATransaction::commit();
    }
    surface.presented = true;
}

fn layer_image(
    mut pixels: Box<[u32]>,
    width: usize,
    height: usize,
    color_space: &CGColorSpace,
    alpha: bool,
) -> CFRetained<CGImage> {
    unsafe extern "C-unwind" fn release(_info: *mut c_void, data: NonNull<c_void>, size: usize) {
        let data = data.cast::<u32>();
        let slice = slice_from_raw_parts_mut(data.as_ptr(), size / size_of::<u32>());
        // SAFETY: This is the same slice that we passed to Box::into_raw below.
        drop(unsafe { Box::from_raw(slice) })
    }

    let alpha_info = if alpha {
        // AppKit transparent windows use straight alpha for this CGImage path.
        unpremultiply_argb(&mut pixels);
        CGImageAlphaInfo::First
    } else {
        CGImageAlphaInfo::NoneSkipFirst
    };
    let len = pixels.len() * size_of::<u32>();
    let raw = Box::into_raw(pixels).cast::<c_void>();
    // SAFETY: Core Graphics owns the slice until its release callback. The callback ignores info.
    let provider = unsafe { CGDataProvider::with_data(ptr::null_mut(), raw, len, Some(release)) }
        .expect("create image data provider");
    let bitmap_info = CGBitmapInfo(
        alpha_info.0
            | CGImageComponentInfo::Integer.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImagePixelFormatInfo::Packed.0,
    );
    // SAFETY: The provider holds the width-by-height 32-bit buffer for the image's lifetime.
    unsafe {
        CGImage::new(
            width,
            height,
            8,
            32,
            width * 4,
            Some(color_space),
            bitmap_info,
            Some(&provider),
            ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .expect("create presentation image")
}

fn unpremultiply_argb(pixels: &mut [u32]) {
    let mut last = (0, 0);
    for pixel in pixels {
        if *pixel != last.0 {
            let alpha = *pixel >> 24;
            let straight = match alpha {
                0 => 0,
                255 => *pixel,
                _ => {
                    let channel = |shift: u32| {
                        ((((*pixel >> shift) & 0xff) * 255 + alpha / 2) / alpha).min(255)
                    };
                    alpha << 24 | channel(16) << 16 | channel(8) << 8 | channel(0)
                }
            };
            last = (*pixel, straight);
        }
        *pixel = last.1;
    }
}

#[cfg(all(test, target_os = "macos"))]
mod alpha_tests {
    use super::*;

    fn damage(x: u32, y: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width: NonZeroU32::MIN,
            height: NonZeroU32::new(height).unwrap(),
        }
    }

    #[test]
    fn appkit_damage_selects_only_intersecting_strips() {
        assert_eq!(
            damaged_strips(5, 259, &[damage(4, 127, 1)]),
            [true, false, false]
        );
        assert_eq!(
            damaged_strips(5, 259, &[damage(4, 127, 2), damage(4, 128, 1)]),
            [true, true, false]
        );
        assert_eq!(
            damaged_strips(5, 259, &[damage(4, 256, u32::MAX), damage(5, 0, 129)]),
            [false, false, true]
        );
        assert_eq!(
            damaged_strips(5, 259, &[damage(u32::MAX, 0, 1), damage(4, u32::MAX, 1)]),
            [false, false, false]
        );
    }

    #[test]
    fn appkit_updates_strip_images_without_replacing_neighbors() {
        const WIDTH: usize = 7;
        const HEIGHT: usize = 259;
        let parent = CALayer::new();
        parent.setGeometryFlipped(true);
        parent.setContentsScale(2.0);
        parent.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(3.5, 129.5)));
        let color_space = CGColorSpace::new_device_rgb().unwrap();
        let mut pixels = vec![0x8040_2010; WIDTH * HEIGHT];
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let strips = Strips::attach(&parent, WIDTH, HEIGHT, 2.0);
        strips.update(&pixels, &[true, true, true], &color_space, true);
        let contents = || {
            strips
                .layers
                .iter()
                .map(|layer| {
                    // SAFETY: All contents were installed by Strips::update and retained here.
                    unsafe { layer.contents() }.unwrap()
                })
                .collect::<Vec<_>>()
        };
        let before = contents();
        pixels[WIDTH * STRIP_HEIGHT] = 0x8060_4000;
        strips.update(
            &pixels,
            &damaged_strips(WIDTH, HEIGHT, &[damage(0, 128, 1)]),
            &color_space,
            true,
        );
        let after = contents();
        let frames = strips
            .layers
            .iter()
            .map(|layer| layer.frame())
            .collect::<Vec<_>>();
        strips.detach();
        CATransaction::commit();

        let unchanged = |index: usize| ptr::eq(&*before[index], &*after[index]);
        assert!(unchanged(0));
        assert!(!unchanged(1));
        assert!(unchanged(2));
        assert_eq!(frames[0].origin.y, 65.5);
        assert_eq!(frames[1].origin.y, 1.5);
        assert_eq!(frames[2].origin.y, 0.0);
        assert_eq!(frames[2].size, CGSize::new(3.5, 1.5));
        assert_eq!(pixels[0], 0x8040_2010);
        assert_eq!(pixels[WIDTH * STRIP_HEIGHT], 0x8060_4000);
    }

    #[test]
    fn core_graphics_accepts_straight_argb_and_opaque_images() {
        let mut pixels = [0, 0x8040_2010, 0x8040_2010, 0xff80_4020];
        unpremultiply_argb(&mut pixels);
        assert_eq!(pixels, [0, 0x8080_4020, 0x8080_4020, 0xff80_4020]);
        let color_space = CGColorSpace::new_device_rgb().unwrap();
        let image = layer_image(Box::new([0x8040_2010]), 1, 1, &color_space, true);
        assert_eq!(CGImage::alpha_info(Some(&image)), CGImageAlphaInfo::First);
        let image = layer_image(Box::new([0xff80_4020]), 1, 1, &color_space, false);
        assert_eq!(
            CGImage::alpha_info(Some(&image)),
            CGImageAlphaInfo::NoneSkipFirst
        );
    }
}

#[derive(Debug)]
struct SendCALayer(Retained<CALayer>);

// SAFETY: CALayer is dubiously thread safe, like most things in Core Animation.
// But since we make sure to do our changes within a CATransaction, it is
// _probably_ fine for us to use CALayer from different threads.
//
// See also:
// https://developer.apple.com/documentation/quartzcore/catransaction/1448267-lock?language=objc
// https://stackoverflow.com/questions/76250226/how-to-render-content-of-calayer-on-a-background-thread
unsafe impl Send for SendCALayer {}
// SAFETY: Same as above.
unsafe impl Sync for SendCALayer {}

impl Deref for SendCALayer {
    type Target = CALayer;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
