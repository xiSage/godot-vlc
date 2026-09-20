//! The editor's inspector, for a `VLCMedia` in a scene or a file system dock.
//!
//! Two classes make this work, and one line in [crate::vlc_editor_plugin::VlcEditorPlugin] joins
//! them to the editor plugin that already exists:
//!
//! - [`VlcMediaInspectorPlugin`] decides *which* objects get the extra control and adds it;
//! - [`VlcMediaInspector`] is the control: a picture, and what libvlc knows about the media.
//!
//! # Why the control is a container
//! `add_custom_control` takes any `Control`, and where it lands depends on what it is handed: an
//! `EditorProperty` is the widget a property's **value** is drawn in, so giving it one puts the
//! picture in the value column, aligned with the input boxes and with half the inspector left
//! empty. This class is a `VBoxContainer` -- so the row *is* the control, and its three children
//! are simply its children, laid out by it.
//!
//! # How the picture is sized
//! It has no width of its own: `IGNORE_SIZE` keeps the image's dimensions out of the layout, and
//! the picture expands both ways, so it fills whatever width the dock gives it while the stretch
//! mode keeps its proportions. It has one minimum height, because a row whose height could only
//! come from a picture that imposes nothing would have no height at all.
//!
//! # Why it is event-driven rather than blocking
//! A thumbnail has no synchronous API -- a request is answered by an event, on a thread of
//! libvlc's -- and libvlc cannot be asked whether a media *has* embedded cover art without parsing
//! it. Both facts are measured rather than assumed: the acceptance shows a cover event arriving
//! once per parse, never arriving at all for a media without a cover, and a second `parse_request`
//! being refused.
//!
//! So the control does what a script would: it asks for a thumbnail, connects to the two signals,
//! and draws whatever arrives. The generated thumbnail is shown first and an embedded cover
//! replaces it if one turns up, which is the order those measurements force -- waiting for the
//! cover to find out whether there is one would mean waiting forever for most media.
//!
//! # What it displays
//! The picture, where it came from, and the metadata libvlc reports: every name
//! [method VLCMedia.get_meta_extra_names] lists, not a chosen few, because an inspector has room
//! for what a script might not want to ask for.
//!
//! # Why the description is written twice
//! Because the control is filled *before* it is handed to the inspector: the plugin builds it,
//! calls [method VlcMediaInspector.show_media], and only then hands it to `add_custom_control`.
//! The labels are made in `enter_tree`, so the description `show_media` writes has nowhere to
//! land and is dropped. The one that lands is written when the control enters the tree, which is
//! also the first moment the panel has anything to write into.
//!
//! That would be no more than untidy if every panel was driven by events -- but a panel is not
//! always driven by events, because of what "opening a media" means to the editor.
//!
//! # A media that was read before the panel existed
//! Opening the same file twice is not two parses: the loader caches a `VLCMedia` by path, so the
//! second inspector is handed the media the first one parsed. libvlc will not announce that media
//! again -- its cover event is once per parse, and a second `parse_request` for a media it has
//! already read is refused, both measured and both written down on
//! [method VLCMedia.parse_request] -- so a panel that only redraws on a parse shows nothing at
//! all, which is what a second open looked like.
//!
//! The panel therefore reads what libvlc already knows when it enters the tree, whether or not a
//! parse is coming, and a cover that libvlc will not report again is found on the media itself:
//! [member VLCMedia.editor_cover] is where a panel leaves the cover it drew, and the next panel
//! built for that media finds it there. The lifetime is the whole reason it works -- while the
//! media object lives the cover is with it, and when it is freed the editor loads the media again,
//! unparsed, so libvlc reports the cover again.
//!
//! # Why the cover is not read out of libvlc's art cache
//! It looks like the obvious source -- a parsed media's `ArtworkURL` names a file libvlc wrote --
//! and it is wrong for exactly the media this panel is usually showing. That cache is keyed on a
//! media's URL, and every media this extension loads through callbacks answers the constant
//! `imem://` (see [method VLCMedia.get_mrl]), so a media with a cover and a media without one
//! share one cache slot. Measured on two `res://` media: a 13 KB FLAC with an embedded 16x16 cover
//! and the demo's `test.mp4`, which has none. After the FLAC was read, the MP4's own `ArtworkURL`
//! named the FLAC's artwork file and the panel drew the FLAC's cover on the MP4 -- which is the
//! defect that replaced the cache route. The art cache is still worth knowing about for other
//! reasons; [method VLCMedia.get_meta] says what it is and is not.
//!
//! # Why the cover is not in the media's metadata either
//! `set_meta` looks like the same idea with less machinery, and it is not: Godot writes a
//! resource's metadata into any `.tres` or `.tscn` that carries that resource. Measured, packing
//! the demo scene around a media whose metadata held a 640x640 cover produced a 6.6 MB scene file
//! with the cover in it as pixel data. A plain `#[var]` property is not written -- it carries no
//! `STORAGE` usage flag -- which is what [member VLCMedia.editor_cover] is.
//!
//! A cover outranks a generated frame whichever way round the two arrive, on the first open and on
//! every later one, so every panel for a media ends up showing the same picture.

use godot::classes::{
    EditorInspectorPlugin, IEditorInspectorPlugin, IVBoxContainer, ImageTexture, Label, Object,
    Texture2D, TextureRect, VBoxContainer, control as control_classes,
    texture_rect as texture_rect_classes,
};
use godot::prelude::*;

use crate::vlc::*;
use crate::vlc_media::VlcMedia;
use crate::vlc_picture::VlcPicture;
use crate::vlc_thumbnail::VlcThumbnailRequest;

/// The size asked libvlc for, in pixels. Only the request: the preview takes whatever width the
/// inspector gives it, at the picture's proportions.
const THUMBNAIL_SIZE: i32 = 256;

/// Adds the media control to the inspector of every `VLCMedia`.
#[derive(GodotClass)]
#[class(tool, init, base=EditorInspectorPlugin)]
pub struct VlcMediaInspectorPlugin {
    base: Base<EditorInspectorPlugin>,
}

#[godot_api]
impl IEditorInspectorPlugin for VlcMediaInspectorPlugin {
    /// Whether this plugin handles an object: a `VLCMedia`, and everything that extends it.
    fn can_handle(&self, object: Option<Gd<Object>>) -> bool {
        object.is_some_and(|object| object.is_class("VLCMedia"))
    }

    /// Adds the control at the top of the inspector, ahead of the properties.
    fn parse_begin(&mut self, object: Option<Gd<Object>>) {
        let Some(object) = object else {
            return;
        };
        let Ok(media) = object.try_cast::<VlcMedia>() else {
            return;
        };
        let mut control = VlcMediaInspector::new_alloc();
        control.bind_mut().show_media(media);
        self.base_mut().add_custom_control(&control);
    }
}

/// One media's picture and metadata, as a row in the inspector.
#[derive(GodotClass)]
#[class(tool, init, base=VBoxContainer)]
pub struct VlcMediaInspector {
    base: Base<VBoxContainer>,
    /// The picture.
    picture: Option<Gd<TextureRect>>,
    /// Where the picture came from, which is worth saying: a generated thumbnail and an embedded
    /// cover are different things.
    origin: Option<Gd<Label>>,
    /// The metadata, as one label: a row has no room for a tree.
    metadata: Option<Gd<Label>>,
    /// The request, held so that it is not destroyed while libvlc is still working on it.
    request: Option<Gd<VlcThumbnailRequest>>,
    /// Why there will be no metadata, when that is already known: an unparsed media would otherwise
    /// look like one that is still being read.
    unparsed: Option<String>,
    /// Whether the picture that is up is a cover rather than a generated frame.
    ///
    /// A cover outranks a frame: that is the order a first open arrives in (the module notes say
    /// why), and the two are independent events, so the precedence is kept rather than assumed.
    picture_is_cover: bool,
    /// The media this control is showing, so that its signals can be let go of.
    media: Option<Gd<VlcMedia>>,
}

#[godot_api]
impl IVBoxContainer for VlcMediaInspector {
    fn enter_tree(&mut self) {
        let mut picture = TextureRect::new_alloc();
        // No width of its own, and a minimum height: `IGNORE_SIZE` keeps the image's dimensions out
        // of the layout, the two expand flags let the container give it the whole row, and the
        // minimum is what stops that row from having no height before a picture arrives.
        picture.set_expand_mode(texture_rect_classes::ExpandMode::IGNORE_SIZE);
        picture.set_stretch_mode(texture_rect_classes::StretchMode::KEEP_ASPECT_CENTERED);
        picture.set_h_size_flags(control_classes::SizeFlags::EXPAND_FILL);
        picture.set_v_size_flags(control_classes::SizeFlags::EXPAND_FILL);
        picture.set_custom_minimum_size(Vector2 { x: 0.0, y: 256.0 });
        // Both labels wrap: a path or an MRL is one long word, and a label that will not break one
        // is a label that widens the whole inspector.
        let mut origin = Label::new_alloc();
        origin.set_autowrap_mode(godot::classes::text_server::AutowrapMode::WORD_SMART);
        let mut metadata = Label::new_alloc();
        metadata.set_autowrap_mode(godot::classes::text_server::AutowrapMode::WORD_SMART);

        self.base_mut().add_child(&picture);
        self.base_mut().add_child(&origin);
        self.base_mut().add_child(&metadata);
        self.base_mut()
            .set_h_size_flags(control_classes::SizeFlags::EXPAND_FILL);

        self.picture = Some(picture);
        self.origin = Some(origin);
        self.metadata = Some(metadata);

        // Now that the labels exist, the description is written again -- and this is the one that
        // lands, because the plugin filled this control before the inspector had it. Nothing is
        // waiting for an event to make that necessary: a media libvlc has already read never
        // announces itself again, and this is the only description such a panel will ever get.
        // (`show_media` still describes, for a caller that fills a control already in the tree.)
        if let Some(media) = self.media.clone() {
            self.describe(&media);
            // The same is true of the picture, and the cover for it is on the media itself: the
            // panel that first saw one left it in [member VLCMedia.editor_cover], which lives
            // exactly as long as the media does. This is where there is a picture to put it in.
            if let Ok(cover) = media.get("editor_cover").try_to::<Gd<Texture2D>>() {
                self.show_cover_texture(&cover, "remembered from when this media was last shown");
            }
        }
    }
}

#[godot_api]
impl VlcMediaInspector {
    /// Shows a media: its metadata now, and its picture when one arrives.
    ///
    /// Called by the plugin as the control is built, once per inspector rebuild, which is why there
    /// is no "the selection changed" path here: a rebuild makes a new control.
    #[func]
    fn show_media(&mut self, media: Gd<VlcMedia>) {
        self.describe(&media);
        self.watch(&media);
        self.media = Some(media);
    }

    /// The parse this control asked for has moved on, so the metadata may be there now.
    ///
    /// libvlc reports its progress in steps and this runs for each of them: the names appear when
    /// they appear. It is that parse which also finds embedded cover art, and that arrives on its
    /// own signal.
    #[func]
    fn on_parsed_changed(&mut self, _status: i32) {
        let Some(media) = self.media.clone() else {
            return;
        };
        self.describe(&media);
    }

    /// A thumbnail request was answered -- with a picture, or with nothing at all.
    #[func]
    fn on_thumbnail_generated(&mut self, picture: Option<Gd<VlcPicture>>) {
        match picture {
            // A cover that is already up is the better picture, and this is the one event that
            // could replace it: the two arrive independently, and a remembered cover is up before
            // the request it was made alongside has been answered.
            Some(picture) if !self.picture_is_cover => self.show(&picture, "generated thumbnail"),
            Some(_) => {}
            // libvlc's whole vocabulary of failure: no picture, and a line in its log.
            None => self.say_while_no_picture("generated thumbnail: libvlc produced none"),
        }
    }

    /// Embedded cover art turned up, which is a better picture than a generated frame.
    #[func]
    fn on_attached_thumbnails_found(&mut self, pictures: Array<Gd<VlcPicture>>) {
        match pictures.get(0) {
            Some(picture) => self.show_cover(&picture, "embedded cover"),
            None => self.say_while_no_picture("embedded cover: the event carried none"),
        }
    }
}

// The casts below bridge two types rather than being redundant: libvlc's enums are typed by
// bindgen per target, while this binding's own #[func]s take i32. The same allowance, for the
// same reason, as VlcMedia and VlcPicture carry.
#[allow(clippy::unnecessary_cast)]
impl VlcMediaInspector {
    /// Fills in what libvlc can already answer: the MRL, and whatever metadata there is.
    ///
    /// Metadata is empty until a media has been parsed, so the control asks for a parse rather than
    /// showing a media with nothing under it. It is the same parse that finds embedded cover art,
    /// and the one [signal VLCMedia.parsed_changed] announces, which is when this runs again.
    ///
    /// `get_mrl` and the metadata methods are reached through Godot rather than directly, because
    /// they are `#[func]`s: the binding exposes them to scripts, not to its own Rust.
    fn describe(&mut self, media: &Gd<VlcMedia>) {
        // `Object::call` takes the object mutably, and this only has it by reference: a clone of the
        // handle is the same object.
        let mut media = media.clone();
        let mrl = media.call("get_mrl", &[]).to::<GString>();
        let mut text = format!("MRL: {mrl}");
        let mut shown = 0;

        // The standard kinds first, because they are the ones a demuxer actually fills: a tagged
        // file reports its Title and Artist here and reports **no** extra names at all, which is why
        // the row looked empty while the parse was working. The extra names below are for the
        // arbitrary ones a file happens to carry, and they come second because they are the rarer
        // case.
        for (label, kind) in [
            ("Title", libvlc_meta_t_libvlc_meta_Title),
            ("Artist", libvlc_meta_t_libvlc_meta_Artist),
            ("Album artist", libvlc_meta_t_libvlc_meta_AlbumArtist),
            ("Album", libvlc_meta_t_libvlc_meta_Album),
            ("Date", libvlc_meta_t_libvlc_meta_Date),
            ("Genre", libvlc_meta_t_libvlc_meta_Genre),
            ("Track", libvlc_meta_t_libvlc_meta_TrackNumber),
            ("Tracks", libvlc_meta_t_libvlc_meta_TrackTotal),
            ("Disc", libvlc_meta_t_libvlc_meta_DiscNumber),
            ("Discs", libvlc_meta_t_libvlc_meta_DiscTotal),
            ("Copyright", libvlc_meta_t_libvlc_meta_Copyright),
            ("Publisher", libvlc_meta_t_libvlc_meta_Publisher),
            ("Encoded by", libvlc_meta_t_libvlc_meta_EncodedBy),
            ("Description", libvlc_meta_t_libvlc_meta_Description),
            ("Language", libvlc_meta_t_libvlc_meta_Language),
            ("Rating", libvlc_meta_t_libvlc_meta_Rating),
            ("Setting", libvlc_meta_t_libvlc_meta_Setting),
            ("URL", libvlc_meta_t_libvlc_meta_URL),
            ("Artwork", libvlc_meta_t_libvlc_meta_ArtworkURL),
            ("Director", libvlc_meta_t_libvlc_meta_Director),
            ("Actors", libvlc_meta_t_libvlc_meta_Actors),
            ("Show", libvlc_meta_t_libvlc_meta_ShowName),
            ("Season", libvlc_meta_t_libvlc_meta_Season),
            ("Episode", libvlc_meta_t_libvlc_meta_Episode),
        ] {
            let value = media
                .call("get_meta", &[(kind as u32).to_variant()])
                .to::<GString>()
                .to_string();
            // libvlc falls back to the input item's name for Title, which for an in-memory media is
            // "imem://" -- measured -- and that is noise, not metadata.
            if value.is_empty() || value == mrl.to_string() {
                continue;
            }
            text.push('\n');
            text.push_str(label);
            text.push_str(": ");
            text.push_str(&value);
            shown += 1;
        }

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
                shown += 1;
            }
        }
        if shown == 0 {
            match self.unparsed.clone() {
                Some(reason) => {
                    text.push('\n');
                    text.push_str(&reason);
                }
                None => text.push_str("\n(no metadata yet: libvlc fills it in as it parses)"),
            }
        }
        if let Some(metadata) = self.metadata.as_mut() {
            metadata.set_text(&text);
        }
    }

    /// Connects to what the media reports, and asks for a thumbnail at its middle.
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
        media.connect("parsed_changed", &self.base().callable("on_parsed_changed"));
        // The parse is asked for, not assumed: metadata is empty until one has run, and the same
        // parse is what reports embedded cover art. `parse_request` is a `#[func]`, so it is reached
        // through Godot like the metadata readers are.
        //
        // Measured, and it is what an earlier version of this got wrong: libvlc does **not** refuse
        // a media it cannot type. It skips the type test when PARSE_FORCED is passed, and an
        // in-memory media -- what `load_from_file` makes, and what an inspector usually has --
        // parses, fills its metadata and reaches `done` with that flag.
        //
        // What libvlc does refuse is a media that is already being parsed, or has been parsed
        // (`lib/media.c:740-744`). That returns -1 with `libvlc_errmsg()` left NULL, which is the
        // "refused to parse" line that appeared once per inspector rebuild. So the status is read
        // first, and only a media with nothing to lose is asked again.
        let status = media.call("get_parsed_status", &[]).to::<i32>();
        let worth_asking = status
            == libvlc_media_parsed_status_t_libvlc_media_parsed_status_none as i32
            || status == libvlc_media_parsed_status_t_libvlc_media_parsed_status_failed as i32
            || status == libvlc_media_parsed_status_t_libvlc_media_parsed_status_timeout as i32
            || status == libvlc_media_parsed_status_t_libvlc_media_parsed_status_cancelled as i32;
        if worth_asking {
            let flags = libvlc_media_parse_flag_t_libvlc_media_parse_local as i32
                | libvlc_media_parse_flag_t_libvlc_media_parse_forced as i32;
            let _ = media.call("parse_request", &[flags.to_variant(), 0i32.to_variant()]);
        } else if status == libvlc_media_parsed_status_t_libvlc_media_parsed_status_pending as i32 {
            self.unparsed = Some("libvlc is reading this media: its metadata follows.".to_string());
        } else {
            // Read, or declined to read: either way libvlc has nothing more to say about this
            // media, so nothing will arrive that could redraw the metadata.
            self.unparsed = Some(
                if status == libvlc_media_parsed_status_t_libvlc_media_parsed_status_done as i32 {
                    "libvlc has read this media; it reports no metadata for it."
                } else {
                    "libvlc did not parse this media, so it has no metadata to report."
                }
                .to_string(),
            );
        }
        let request = media.bind().thumbnail_request_by_pos(
            0.5,
            VlcThumbnailRequest::SEEK_PRECISE as i32,
            THUMBNAIL_SIZE,
            THUMBNAIL_SIZE,
            false,
            libvlc_picture_type_t_libvlc_picture_Png as i32,
            5000,
        );
        if request.is_none() {
            self.say("no thumbnail: libvlc requested none");
        }
        self.request = request;
    }

    /// Puts a picture in the control, and says where it came from.
    ///
    /// Nothing about the size is decided here: the container lays the row out, the picture expands
    /// into it and the stretch mode keeps the picture's proportions. See the module notes.
    fn show(&mut self, picture: &Gd<VlcPicture>, from: &str) {
        let Some(image) = picture.bind().to_image() else {
            self.say(&format!(
                "{from}: the picture could not be turned into an image"
            ));
            return;
        };
        let Some(texture) = ImageTexture::create_from_image(&image) else {
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

    /// Puts up a cover that libvlc is not going to report, with the line saying where it came from.
    ///
    /// The panel cannot ask libvlc for it again: the cover event fires once per parse, and a second
    /// `parse_request` for a media libvlc has already read is refused (both measured; see
    /// [method VLCMedia.parse_request]). The media's `ArtworkURL` is not an answer either, and it is
    /// worth saying why, because it looks like one: that cache is keyed on the media's URL, and
    /// every media this extension loads through callbacks answers the constant `imem://`, so a
    /// media with a cover and a media without one share one slot -- measured on the engine test's
    /// FLAC fixture and the demo's `test.mp4`, where the MP4 was handed the FLAC's artwork file.
    fn show_cover_texture(&mut self, cover: &Gd<Texture2D>, from: &str) {
        if let Some(picture) = self.picture.as_mut() {
            picture.set_texture(cover);
        }
        // It is a cover, so the generated thumbnail that is still on its way must not replace it.
        self.picture_is_cover = true;
        self.say(&format!(
            "embedded cover: {}x{} ({from})",
            cover.get_width(),
            cover.get_height()
        ));
    }

    /// Shows a cover from libvlc's event, and leaves it on the media for the panels built after
    /// this one.
    fn show_cover(&mut self, picture: &Gd<VlcPicture>, from: &str) {
        self.picture_is_cover = true;
        self.show(picture, from);
        // The picture that is up is the one to leave behind: `show` fails quietly when a picture
        // cannot be turned into a texture, and then there is nothing worth keeping.
        let shown = self
            .picture
            .as_ref()
            .and_then(|picture| picture.get_texture());
        if let (Some(mut media), Some(shown)) = (self.media.clone(), shown) {
            media.set("editor_cover", &shown.to_variant());
        }
    }

    fn say(&mut self, text: &str) {
        if let Some(origin) = self.origin.as_mut() {
            origin.set_text(text);
        }
    }

    /// Says why there is no picture, but only while there is none.
    ///
    /// The line describes the picture that is up, so a picture that came from somewhere else must
    /// not be talked over by the news that a generated thumbnail produced nothing. Measured: a
    /// media with a cover arrived at `picture=1x1` while its row read `generated thumbnail: libvlc
    /// produced none`, because the cover event and the thumbnail's own answer are two independent
    /// events and either can be last.
    fn say_while_no_picture(&mut self, text: &str) {
        let has_picture = self
            .picture
            .as_ref()
            .is_some_and(|picture| picture.get_texture().is_some());
        if !has_picture {
            self.say(text);
        }
    }
}
