//! The editor's inspector, for a `VLCMedia` in a scene or a file system dock.
//!
//! Two classes make this work, and one line in [crate::vlc_editor_plugin::VlcEditorPlugin] joins
//! them to the editor that already exists:
//!
//! - [`VlcMediaInspectorPlugin`] decides *which* objects get the extra control and adds it;
//! - [`VlcMediaInspector`] is the control: a picture and what libvlc knows about the media.
//!
//! # Why it is event-driven rather than blocking
//! A thumbnail has no synchronous API -- a request is answered by an event, on a thread of
//! libvlc's -- and libvlc cannot even be asked whether a media *has* embedded cover art without
//! parsing it. Both facts are measured rather than assumed: the acceptance shows a cover event
//! arriving once per parse, never arriving at all for a media without a cover, and a second
//! `parse_request` being refused.
//!
//! So the control does what a script would: it asks for a thumbnail, it connects to the two
//! signals, and it draws what arrives. The generated thumbnail is shown first and an embedded
//! cover replaces it if one turns up, which is the order the measurements force -- waiting for
//! the cover to decide whether there is one would mean waiting forever for most media.
//!
//! # What it displays
//! The picture, where it came from, and the metadata libvlc reports -- every name
//! [method VLCMedia.get_meta_names] lists, not a chosen few, because an inspector has room for
//! what a script might not want to ask for.

use godot::classes::{
    Control, EditorInspectorPlugin, EditorProperty, IEditorInspectorPlugin, IEditorProperty, Label,
    Object, TextureRect, VBoxContainer,
};
use godot::prelude::*;

use crate::vlc::*;
use crate::vlc_media::VlcMedia;
use crate::vlc_picture::VlcPicture;

/// The size asked for, in pixels. An inspector is a column, not a preview window.
const THUMBNAIL_SIZE: i32 = 192;

/// Adds the media control to the inspector of every `VLCMedia`.
#[derive(GodotClass)]
#[class(tool, init, base=EditorInspectorPlugin)]
pub struct VlcMediaInspectorPlugin {
    base: Base<EditorInspectorPlugin>,
}

#[godot_api]
impl IEditorInspectorPlugin for VlcMediaInspectorPlugin {
    /// Whether this plugin handles an object: only a `VLCMedia`, and everything that extends it.
    fn can_handle(&self, object: Option<Gd<Object>>) -> bool {
        object.is_some_and(|object| object.is_class("VLCMedia"))
    }

    /// Adds the control at the top of the inspector, before the properties.
    fn parse_begin(&mut self, _object: Option<Gd<Object>>) {
        let control = VlcMediaInspector::new_alloc();
        self.base_mut()
            .add_custom_control(&control.upcast::<Control>());
    }
}

/// The control itself: one media's picture and metadata.
#[derive(GodotClass)]
#[class(tool, init, base=EditorProperty)]
pub struct VlcMediaInspector {
    base: Base<EditorProperty>,
    /// The picture, or the placeholder that stands in for it.
    picture: Option<Gd<TextureRect>>,
    /// Where the picture came from, which is worth saying because a generated thumbnail and an
    /// embedded cover are different things.
    origin: Option<Gd<Label>>,
    /// The metadata, as one label: an inspector column has no room for a tree.
    metadata: Option<Gd<Label>>,
    /// The request, held so that it is not destroyed while libvlc is still working on it.
    request: Option<Gd<crate::vlc_thumbnail::VlcThumbnailRequest>>,
    /// The media this control was last built for, so that signals are connected once.
    media: Option<Gd<VlcMedia>>,
}

#[godot_api]
impl IEditorProperty for VlcMediaInspector {
    fn enter_tree(&mut self) {
        let mut column = VBoxContainer::new_alloc();
        let mut picture = TextureRect::new_alloc();
        picture.set_custom_minimum_size(Vector2::new(THUMBNAIL_SIZE as f32, 0.0));
        picture.set_expand_mode(godot::classes::texture_rect::ExpandMode::IGNORE_SIZE);
        picture.set_stretch_mode(godot::classes::texture_rect::StretchMode::KEEP_ASPECT_CENTERED);
        let origin = Label::new_alloc();
        let mut metadata = Label::new_alloc();
        metadata.set_autowrap_mode(godot::classes::text_server::AutowrapMode::WORD_SMART);

        column.add_child(&picture);
        column.add_child(&origin);
        column.add_child(&metadata);
        self.base_mut().add_child(&column);

        self.picture = Some(picture);
        self.origin = Some(origin);
        self.metadata = Some(metadata);
    }

    /// Called when the inspector wants this control to show its object.
    fn update_property(&mut self) {
        let Some(object) = self.base().get_edited_object() else {
            return;
        };
        let Ok(media) = object.try_cast::<VlcMedia>() else {
            return;
        };
        self.describe(&media);
        if self.media.as_ref() != Some(&media) {
            self.unwatch();
            self.watch(&media);
            self.media = Some(media);
        }
    }
}

#[allow(clippy::unnecessary_cast)]
impl VlcMediaInspector {
    /// Lets go of the media this control was showing, so that it stops reporting into a control
    /// that has moved on to another one.
    ///
    /// The inspector reuses a single control for whatever is selected, and a media outlives the
    /// selection: without this, every media ever inspected would keep a connection to this
    /// control, and the picture of one would arrive to overwrite the picture of another. The
    /// request goes with it, which is also what destroys libvlc's request.
    fn unwatch(&mut self) {
        let Some(previous) = self.media.take() else {
            return;
        };
        let mut previous = previous.clone();
        previous.disconnect(
            "thumbnail_generated",
            &self.base().callable("on_thumbnail_generated"),
        );
        previous.disconnect(
            "attached_thumbnails_found",
            &self.base().callable("on_attached_thumbnails_found"),
        );
        self.request = None;
        self.say("no thumbnail yet");
    }

    /// Fills in what libvlc can already answer: the MRL and whatever metadata there is.
    ///
    /// Metadata is empty until a media has been parsed, and an inspector must not parse
    /// anything by itself -- the parse is what tells the cover event where to come from, and
    /// this control asks for a thumbnail rather than a parse. So an unparsed media says so.
    fn describe(&mut self, media: &Gd<VlcMedia>) {
        // `Object::call` takes the object mutably, and this only has it by reference: a clone of
        // the handle is the same object.
        let mut media = media.clone();
        // `get_mrl` is reached through Godot rather than directly: it is a `#[func]`, so the
        // binding exposes it to scripts and not to its own Rust.
        let mrl = media.call("get_mrl", &[]).to::<GString>();
        let mut text = format!("MRL: {mrl}");
        // `get_meta_extra_names` and `get_meta_extra` are the pair for this, and they are
        // reached through Godot for the same reason `get_mrl` is: they are `#[func]`s, so the
        // binding exposes them to scripts rather than to its own Rust. They are also the right
        // pair: `get_meta` takes libvlc's metadata *kind*, while the extra names are the
        // arbitrary ones a file happens to carry.
        let names = media
            .call("get_meta_extra_names", &[])
            .to::<PackedStringArray>();
        for name in names.as_slice() {
            let name = name.to_string();
            let value = media
                .call(
                    "get_meta_extra",
                    &[GString::from(name.as_str()).to_variant()],
                )
                .to::<GString>();
            if !value.is_empty() {
                text.push('\n');
                text.push_str(&name);
                text.push_str(": ");
                text.push_str(&value.to_string());
            }
        }
        if let Some(metadata) = self.metadata.as_mut() {
            metadata.set_text(&text);
        }
    }

    /// Connects to what the media reports and asks for a thumbnail at its middle.
    ///
    /// By position, not by time: `0.5` is the middle of any media and needs no known duration,
    /// which is the one thing an inspector cannot count on before a parse has run.
    fn watch(&mut self, media: &Gd<VlcMedia>) {
        // Connecting takes the object mutably, and this only has it by reference: a clone of the
        // handle is the same object.
        let mut media = media.clone();
        media.connect(
            "thumbnail_generated",
            &self.base().callable("on_thumbnail_generated"),
        );
        media.connect(
            "attached_thumbnails_found",
            &self.base().callable("on_attached_thumbnails_found"),
        );
        self.request = media.bind().thumbnail_request_by_pos(
            0.5,
            crate::vlc_thumbnail::VlcThumbnailRequest::SEEK_PRECISE,
            THUMBNAIL_SIZE,
            THUMBNAIL_SIZE,
            false,
            libvlc_picture_type_t_libvlc_picture_Png as i32,
            5000,
        );
        if self.request.is_none() {
            self.say("no thumbnail: libvlc refused the request");
        }
    }

    /// Puts a picture in the control, saying where it came from.
    fn show(&mut self, picture: &Gd<VlcPicture>, from: &str) {
        let Some(image) = picture.bind().to_image() else {
            self.say(&format!(
                "{from}: the picture could not be turned into an image"
            ));
            return;
        };
        let Some(texture) = godot::classes::ImageTexture::create_from_image(&image) else {
            self.say(&format!("{from}: the image could not become a texture"));
            return;
        };
        if let Some(picture_rect) = self.picture.as_mut() {
            picture_rect.set_texture(&texture);
        }
        self.say(&format!(
            "{from}: {}x{}",
            image.get_width(),
            image.get_height()
        ));
    }

    fn say(&mut self, text: &str) {
        if let Some(origin) = self.origin.as_mut() {
            origin.set_text(text);
        }
    }
}

#[godot_api]
impl VlcMediaInspector {
    /// A thumbnail request was answered -- with a picture, or with nothing at all.
    #[func]
    fn on_thumbnail_generated(&mut self, picture: Option<Gd<VlcPicture>>) {
        match picture {
            Some(picture) => self.show(&picture, "generated thumbnail"),
            // Both halves of libvlc's vocabulary of failure: no picture, and libvlc's own log.
            None => self.say("generated thumbnail: libvlc produced none"),
        }
    }

    /// Embedded cover art turned up, which is a better picture than a generated frame.
    #[func]
    fn on_attached_thumbnails_found(&mut self, pictures: Array<Gd<VlcPicture>>) {
        match pictures.get(0) {
            Some(picture) => self.show(&picture, "embedded cover"),
            None => self.say("embedded cover: the event carried none"),
        }
    }
}
