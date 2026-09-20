//! libvlc's pictures: `libvlc_picture_*`.
//!
//! A picture is one image: what a thumbnail request produces ([`crate::vlc_media`]'s
//! `thumbnail_request_by_time`/`_by_pos`) and what a media's embedded cover art is reported
//! as. It is not a Godot type -- turning it into one is a separate step -- and it is
//! not a resource in the project sense: the wrapper here is a `RefCounted`, because a
//! decoded image is a transient thing a script is handed rather than something a scene
//! references.
//!
//! # What is in the buffer
//! `libvlc_picture_get_buffer` hands back **one** buffer, not one per plane, and its length
//! comes back with it. What those bytes are depends on the type, and the two families do
//! not resemble each other:
//!
//! - [constant TYPE_ARGB] and [constant TYPE_RGBA]: one packed frame of four bytes per
//!   pixel, laid out row by row. [method get_stride] is how wide a row is -- and it is the
//!   only one of the two raw types whose value libvlc computes rather than reads, see its
//!   own note.
//! - [constant TYPE_PNG], [constant TYPE_JPG], [constant TYPE_WEBP]: a **complete encoded
//!   image file** in the buffer. [method save] writes exactly these bytes to disk, so a
//!   picture of one of these types can be saved with any name at all; the bytes decide the
//!   format, not the extension.
//!
//! There is no per-row padding in the length either way: the buffer length is what the
//! encoder produced, which for the encoded types is the file size and for the raw ones is
//! assumed to be `stride * height`.
//!
//! # Ownership
//! libvlc counts references on a picture, and every picture this wrapper exists for is one
//! whose reference the wrapper owns: `Drop` gives it back. Pictures that arrive inside
//! libvlc's events are **borrowed** -- the sender releases them the moment the event has
//! been delivered -- so the code that receives them takes a reference first, and hands that
//! reference over.

use godot::prelude::*;

use crate::vlc::*;

/// One picture libvlc produced, holding its own reference to it.
///
/// # Where one comes from
/// - a thumbnail request that finished: the [signal VLCMedia.thumbnail_generated] signal
///   carries one, or none when libvlc could not produce a picture;
/// - a media's embedded cover art: [signal VLCMedia.attached_thumbnails_found] carries an
///   array of them.
///
/// There is no constructor: a script cannot make a picture, only receive one.
#[derive(GodotClass)]
#[class(base=RefCounted, rename=VLCPicture, no_init)]
pub struct VlcPicture {
    base: Base<RefCounted>,
    /// The picture, and the reference that keeps it alive.
    ptr: *mut libvlc_picture_t,
}

impl Drop for VlcPicture {
    fn drop(&mut self) {
        unsafe { libvlc_picture_release(self.ptr) };
    }
}

// The cast bridges two types rather than being redundant: a #[constant] has to be a
// Godot i32, while libvlc's enum takes whatever bindgen chose for it on the target
// being built -- the same cast, and the same allowance, as VlcMedia carries.
#[allow(clippy::unnecessary_cast)]
#[godot_api]
impl VlcPicture {
    /// Four bytes per pixel, in the order alpha, red, green, blue.
    #[constant]
    const TYPE_ARGB: i32 = libvlc_picture_type_t_libvlc_picture_Argb as i32;
    /// A complete PNG file in the buffer.
    #[constant]
    const TYPE_PNG: i32 = libvlc_picture_type_t_libvlc_picture_Png as i32;
    /// A complete JPEG file in the buffer.
    #[constant]
    const TYPE_JPG: i32 = libvlc_picture_type_t_libvlc_picture_Jpg as i32;
    /// A complete WebP file in the buffer.
    #[constant]
    const TYPE_WEBP: i32 = libvlc_picture_type_t_libvlc_picture_WebP as i32;
    /// Four bytes per pixel, in the order red, green, blue, alpha.
    #[constant]
    const TYPE_RGBA: i32 = libvlc_picture_type_t_libvlc_picture_Rgba as i32;

    /// The width of the image in pixels.
    ///
    /// # Returns
    /// the width libvlc reports, which is the *visible* width: a picture that was scaled or
    /// cropped to fit a requested size reports the size that came out, not the one asked for.
    #[func]
    fn get_width(&self) -> i32 {
        unsafe { libvlc_picture_get_width(self.ptr) as i32 }
    }

    /// The height of the image in pixels.
    #[func]
    fn get_height(&self) -> i32 {
        unsafe { libvlc_picture_get_height(self.ptr) as i32 }
    }

    /// The number of bytes in one row of pixels.
    ///
    /// # Returns
    /// the stride, or `-1` for a picture whose type is not [constant TYPE_ARGB] or
    /// [constant TYPE_RGBA].
    ///
    /// # Note
    /// **The `-1` is ours, and it is there because libvlc would abort instead.** Its
    /// implementation is `assert(type == Argb || type == Rgba); return width * 4;`, and the
    /// runtime this binding ships with is built with assertions on -- so asking a JPEG for
    /// its stride is not an error code, it is the end of the process. The header says the
    /// same restriction in prose ("This can only be called on images of type
    /// `libvlc_picture_Argb` or `libvlc_picture_Rgba`") without mentioning the assert.
    ///
    /// For the encoded types there is nothing to return anyway: their buffer is a whole
    /// image file, and rows are not a thing it has.
    ///
    /// Note also that the value is *computed* as `width * 4`, not read out of the picture,
    /// so it assumes the rows are tight against each other even though the header describes
    /// the buffer as including "potential padding". The acceptance test measures the
    /// assumption rather than repeating it: it checks `buffer size == stride * height`.
    #[func]
    fn get_stride(&self) -> i32 {
        let picture_type = self.get_type();
        if picture_type != Self::TYPE_ARGB && picture_type != Self::TYPE_RGBA {
            godot_error!(
                "godot-vlc: get_stride() was refused for a picture of type {}, because libvlc's own implementation asserts on anything but ARGB and RGBA -- an abort in this runtime, not an error code. The buffer of an encoded picture is a whole image file; use to_image() or save() for it.",
                picture_type
            );
            return -1;
        }
        unsafe { libvlc_picture_get_stride(self.ptr) as i32 }
    }

    /// The time at which this picture was generated, in milliseconds.
    ///
    /// # Returns
    /// the time, or `0` when the picture did not come from a time in the media -- an
    /// embedded cover has no position, and libvlc's own "invalid" timestamp is 0 at the
    /// revision this binding pins, so the two are the same number.
    #[func]
    fn get_time(&self) -> i64 {
        unsafe { libvlc_picture_get_time(self.ptr) as i64 }
    }

    /// What is in the buffer.
    ///
    /// # Returns
    /// one of the `TYPE_*` constants.
    #[func]
    fn get_type(&self) -> i32 {
        unsafe { libvlc_picture_type(self.ptr) as i32 }
    }

    /// The image bytes, as libvlc holds them.
    ///
    /// # Returns
    /// a copy of the whole buffer: **one** buffer, not one per plane. Its length is part of
    /// the answer and is not `stride * height` by definition -- for the encoded types it is
    /// a file size. The copy is deliberate: libvlc's buffer belongs to the picture and must
    /// not be written to, and a `PackedByteArray` that shared it would be a way to do
    /// exactly that.
    #[func]
    fn get_buffer(&self) -> PackedByteArray {
        let mut size: usize = 0;
        let buffer = unsafe { libvlc_picture_get_buffer(self.ptr, &mut size) };
        if buffer.is_null() || size == 0 {
            return PackedByteArray::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(buffer, size) };
        PackedByteArray::from(bytes)
    }

    /// The picture as a Godot [Image].
    ///
    /// # Returns
    /// an image, or null when this picture cannot be turned into one.
    ///
    /// # Note
    /// - **The encoded types are decoded by Godot**, not re-encoded here: a PNG, JPEG or WebP
    ///   buffer is a whole image file, so `Image`'s own loaders read it. Nothing is guessed
    ///   about the file.
    /// - **The raw types are copied row by row**, `[method get_stride]` bytes at a time, into
    ///   an `RGBA8` image. The buffer must hold at least `stride * height` bytes; if it holds
    ///   more, the extra is dropped rather than handed to `Image` -- a longer buffer would
    ///   mean rows that are not tight against each other, which is the one thing the stride
    ///   is supposed to describe.
    /// - **[constant TYPE_ARGB] is rotated, because the rotation was measured.** Its buffer
    ///   holds its four bytes per pixel in libvlc's own order -- alpha, red, green, blue --
    ///   while Godot wants red, green, blue, alpha. The acceptance compares the same frame
    ///   taken in both raw types and asserts that rotating ARGB left by one byte reproduces
    ///   RGBA exactly; it passes on this runtime and prints both buffers if a future one
    ///   writes a different order, so the conversion follows the bytes rather than the name.
    /// - **Nothing is shared with the picture**: the image owns a copy, so releasing the
    ///   picture, or letting go of this wrapper, does not disturb it.
    #[func]
    pub(crate) fn to_image(&self) -> Option<Gd<godot::classes::Image>> {
        use godot::classes::image::Format;

        let picture_type = self.get_type();
        let bytes = self.get_buffer();
        if bytes.is_empty() {
            godot_error!("godot-vlc: this picture has no bytes to turn into an image");
            return None;
        }

        match picture_type {
            Self::TYPE_PNG | Self::TYPE_JPG | Self::TYPE_WEBP => {
                // Godot's own readers, because an encoded buffer is a whole image file.
                let mut image = godot::classes::Image::new_gd();
                let error = match picture_type {
                    Self::TYPE_PNG => image.load_png_from_buffer(&bytes),
                    Self::TYPE_JPG => image.load_jpg_from_buffer(&bytes),
                    _ => image.load_webp_from_buffer(&bytes),
                };
                if error != godot::global::Error::OK {
                    godot_error!(
                        "godot-vlc: Godot could not read this encoded picture ({error:?}); a WebP needs a build of Godot with WebP support"
                    );
                    return None;
                }
                Some(image)
            }
            Self::TYPE_RGBA | Self::TYPE_ARGB => {
                let width = self.get_width();
                let height = self.get_height();
                let stride = self.get_stride();
                if width <= 0 || height <= 0 || stride <= 0 {
                    godot_error!(
                        "godot-vlc: a raw picture reported {width}x{height} with a stride of {stride}"
                    );
                    return None;
                }
                let needed = stride as usize * height as usize;
                if bytes.len() < needed {
                    godot_error!(
                        "godot-vlc: the picture holds {} bytes, fewer than the {needed} its stride and height describe",
                        bytes.len()
                    );
                    return None;
                }
                let data = if bytes.len() == needed {
                    bytes
                } else {
                    PackedByteArray::from(&bytes.as_slice()[..needed])
                };
                let data = if picture_type == Self::TYPE_ARGB {
                    rotated_left_by_one(&data)
                } else {
                    data
                };
                let mut image =
                    godot::classes::Image::create_empty(width, height, false, Format::RGBA8)?;
                image.set_data(width, height, false, Format::RGBA8, &data);
                Some(image)
            }

            other => {
                godot_error!("godot-vlc: to_image() does not know picture type {other}");
                None
            }
        }
    }

    /// Writes the picture to a file.
    ///
    /// # Parameters
    /// - [param path] where to write it.
    ///
    /// # Returns
    /// `0` on success, `-1` on failure -- libvlc's own answer, unchanged.
    ///
    /// # Note
    /// **The extension decides nothing.** libvlc opens the path and writes the buffer
    /// verbatim, so what lands on disk is whatever [method get_type] says: a PNG picture
    /// saved as `cover.jpg` is a PNG file with a misleading name. libvlc also writes in one
    /// call, so a partial write is reported as `-1` rather than retried.
    #[func]
    fn save(&self, path: GString) -> i32 {
        let path = match std::ffi::CString::new(path.to_string()) {
            Ok(path) => path,
            Err(_) => {
                godot_error!("godot-vlc: the save path contains a NUL byte");
                return -1;
            }
        };
        unsafe { libvlc_picture_save(self.ptr, path.as_ptr()) }
    }
}

impl VlcPicture {
    /// Wraps a picture libvlc handed over, **taking the reference that came with it**.
    ///
    /// The event payloads are borrowed -- the sender releases them as soon as the event has
    /// been delivered -- so the caller of this takes a reference first
    /// (`libvlc_picture_retain`) and this wrapper owns it from there.
    pub(crate) fn from_retained(ptr: *mut libvlc_picture_t) -> Option<Gd<Self>> {
        if ptr.is_null() {
            return None;
        }
        Some(Gd::from_init_fn(|base| Self { base, ptr }))
    }
}

/// The same bytes with each pixel's four bytes rotated left by one.
///
/// libvlc names the ARGB order alpha, red, green, blue and writes it that way -- measured by
/// [`the_argb_buffer_is_the_rgba_buffer_rotated`] in the acceptance, which compares the same
/// frame taken in both raw types -- while Godot wants red, green, blue, alpha. One rotation of
/// each group of four is the whole conversion.
fn rotated_left_by_one(bytes: &PackedByteArray) -> PackedByteArray {
    let source = bytes.as_slice();
    let mut rotated: Vec<u8> = Vec::with_capacity(source.len());
    for index in (0..source.len()).step_by(4) {
        let pixel = &source[index..index + 4];
        rotated.extend_from_slice(&[pixel[1], pixel[2], pixel[3], pixel[0]]);
    }
    PackedByteArray::from(rotated.as_slice())
}
