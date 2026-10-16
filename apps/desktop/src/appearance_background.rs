//! The user's own background image: import, storage, and the layer that paints
//! it behind the workbench.
//!
//! The contract mirrors the sidebar's project icons, because it answers the same
//! questions. Only a content-addressed file name is persisted — never an
//! external path, never image bytes — and the image itself lives under the
//! runtime home's background directory. The name is the SHA-256 of the file, so
//! re-importing one image reuses one file, and replacing an image produces a new
//! name rather than invalidating a cached decode at the old one.
//!
//! A missing home, a malformed stored name, or a file that has been deleted is
//! not an error the reader has to see: resolution simply reports "no image" and
//! the workbench paints the theme's own page color again.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Styled as _,
    StyledImage as _, div, img, px,
};
use sha2::{Digest, Sha256};
use vibex_desktop_model::{
    BACKGROUND_IMAGE_EXTENSIONS, BackgroundImageFit, BackgroundImageUiState,
    background_image_file_name_is_valid,
};

use crate::{locale, theme};

/// Directory under the runtime home that holds imported background images.
pub(crate) const BACKGROUND_IMAGE_DIRECTORY: &str = "backgrounds";

/// The largest source file the importer accepts.
///
/// A wallpaper is a photograph or a piece of artwork, so the ceiling is generous
/// next to the project icons' 16 MiB — but it is still a ceiling, because the
/// file is read and decoded on the way in.
const BACKGROUND_IMAGE_MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

/// The largest source image the importer accepts, before any decode.
///
/// Bounded separately from the byte size: a small file can name a huge canvas,
/// and a canvas is what the renderer has to upload.
const BACKGROUND_IMAGE_MAX_SOURCE_PIXELS: u64 = 40_000_000;

/// The largest animation the importer accepts, as pixels across every frame.
///
/// Every frame of an animation is decoded and kept — the image element decodes
/// the whole file once and loops the frames it holds — so the bound that matters
/// is the total, not the canvas: at four bytes a pixel this is 192 MiB of
/// decoded frames, which covers a 720p loop of about two seconds and is what
/// keeps an animated wallpaper from allocating like a video player.
const BACKGROUND_IMAGE_MAX_ANIMATION_PIXELS: u64 = 48_000_000;

/// The preview size the appearance page paints a stored image at.
pub(crate) const BACKGROUND_IMAGE_THUMBNAIL_SIZE: f32 = 36.0;

/// Where the image stored as `file_name` lives.
pub(crate) fn path(home: &Path, file_name: &str) -> PathBuf {
    home.join(BACKGROUND_IMAGE_DIRECTORY).join(file_name)
}

/// Where the configured background image lives, if it is configured, correctly
/// named, and still on disk.
///
/// The stat is what makes a deleted file degrade to the theme's page color
/// instead of painting an empty surface. It runs when an appearance is applied
/// and once per frame while the workbench paints, which one `stat` covers
/// comfortably.
pub(crate) fn resolve(home: Option<&Path>, state: &BackgroundImageUiState) -> Option<PathBuf> {
    let file_name = state.file.as_deref()?;
    if !background_image_file_name_is_valid(file_name) {
        return None;
    }
    let path = path(home?, file_name);
    path.is_file().then_some(path)
}

/// Imports `source` into the managed background directory and returns the file
/// name it is stored under.
///
/// The stored file keeps its original bytes and format: a wallpaper is not
/// resized or re-encoded on the way in, so no generation loss and no large PNG
/// standing in for a JPEG. Equal content reuses one file, and the copy lands
/// through a temporary name so a failed import never leaves a half-written
/// image behind.
///
/// An animation — GIF or animated WebP — is stored the same way and loops as the
/// wallpaper, which is why its frames are counted here: the renderer decodes
/// every frame of the file and keeps them, so the budget has to be checked
/// before the file is accepted rather than when it is painted.
pub(crate) fn import(source: &Path, home: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(source)
        .map_err(|error| format!("could not read the selected file: {error}"))?;
    if !metadata.is_file() {
        return Err("the selected path is not a file".to_string());
    }
    if metadata.len() > BACKGROUND_IMAGE_MAX_SOURCE_BYTES {
        return Err("the selected image is larger than 32 MB".to_string());
    }
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| BACKGROUND_IMAGE_EXTENSIONS.contains(&extension.as_str()))
        .ok_or_else(|| "choose a PNG, JPEG, WebP, GIF, BMP, or TIFF image".to_string())?;
    let (width, height) = image::image_dimensions(source)
        .map_err(|error| format!("unsupported or invalid image: {error}"))?;
    if width == 0
        || height == 0
        || u64::from(width)
            .checked_mul(u64::from(height))
            .is_none_or(|pixels| pixels > BACKGROUND_IMAGE_MAX_SOURCE_PIXELS)
    {
        return Err("the selected image dimensions are too large".to_string());
    }
    if let Some(frames) = animation_frames(source, &extension) {
        let total = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(u64::from(frames)));
        if total.is_none_or(|pixels| pixels > BACKGROUND_IMAGE_MAX_ANIMATION_PIXELS) {
            return Err(
                "the animation is too large to loop as a wallpaper: shorten it, or scale it to \
                 about 720p"
                    .to_string(),
            );
        }
    }

    let digest = file_digest(source)?;
    let file_name = format!("{digest}.{extension}");
    let directory = home.join(BACKGROUND_IMAGE_DIRECTORY);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create background image storage: {error}"))?;
    let destination = directory.join(&file_name);
    if !destination.exists() {
        let temporary = directory.join(format!(".{digest}.tmp"));
        if let Err(error) = std::fs::copy(source, &temporary) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("could not store the background image: {error}"));
        }
        if let Err(error) = std::fs::rename(&temporary, &destination) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("could not install the background image: {error}"));
        }
    }
    Ok(file_name)
}

/// How many frames `source` holds, when it holds an animation at all.
///
/// GIF and WebP are the two formats the image element loops; every other
/// accepted format is a single still and reports `None`. The frames are decoded
/// to count them, on the import's own task rather than the UI thread, and each
/// one is dropped as it is counted.
fn animation_frames(source: &Path, extension: &str) -> Option<u32> {
    use image::AnimationDecoder as _;

    let reader = std::io::BufReader::new(std::fs::File::open(source).ok()?);
    let frames = match extension {
        "gif" => image::codecs::gif::GifDecoder::new(reader)
            .ok()?
            .into_frames()
            .count(),
        "webp" => {
            let decoder = image::codecs::webp::WebPDecoder::new(reader).ok()?;
            if !decoder.has_animation() {
                return None;
            }
            decoder.into_frames().count()
        }
        _ => return None,
    };
    u32::try_from(frames).ok()
}

/// The SHA-256 of a file's bytes, read in chunks so a large wallpaper never has
/// to be held in memory to be named.
fn file_digest(source: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(source)
        .map_err(|error| format!("could not read the selected file: {error}"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|error| format!("could not read the selected file: {error}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// How the stored image is fitted to the window.
pub(crate) fn object_fit(fit: BackgroundImageFit) -> ObjectFit {
    match fit {
        BackgroundImageFit::Cover => ObjectFit::Cover,
        BackgroundImageFit::Contain => ObjectFit::Contain,
        BackgroundImageFit::Fill => ObjectFit::Fill,
    }
}

/// The element id the wallpaper image paints under.
///
/// It is what an animated GIF or WebP needs to loop: the frame cursor lives in
/// an element's own state, and an element that declares no id has nowhere to
/// keep one — it would paint its first frame forever. The loop stops on its own
/// while the window is inactive or the reader asked for reduced motion. The
/// thumbnail in the settings page is a different element and takes no id, so a
/// preview stays a still.
pub(crate) const WALLPAPER_IMAGE_ELEMENT_ID: &str = "workbench-background-image";

/// The wallpaper image itself: the stored file, fitted to the space it is given.
fn wallpaper_image(state: &BackgroundImageUiState, path: &Path) -> gpui::Stateful<gpui::Img> {
    img(path.to_path_buf())
        .id(WALLPAPER_IMAGE_ELEMENT_ID)
        .absolute()
        .inset_0()
        .size_full()
        .object_fit(object_fit(state.fit))
}

/// The image and its readability veil, painted over the window's opaque plate.
///
/// This is the first child of the workbench, so it paints behind every surface.
/// The image covers the plate; the veil over it is what keeps the text on those
/// surfaces legible over any wallpaper. A `Contain` fit letterboxes, and the
/// letterbox shows the plate under the veil — which is the same page color the
/// veil is mixed from, so the bars read as part of the page rather than as a
/// frame around the image.
pub(crate) fn wallpaper_layer(
    state: &BackgroundImageUiState,
    path: &Path,
    is_dark: bool,
) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .child(wallpaper_image(state, path))
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(theme::wallpaper_veil(is_dark, state.dim_percent)),
        )
        .into_any_element()
}

/// A small preview of the stored image for the appearance page.
pub(crate) fn thumbnail(path: &Path) -> AnyElement {
    img(path.to_path_buf())
        .w(px(BACKGROUND_IMAGE_THUMBNAIL_SIZE))
        .h(px(BACKGROUND_IMAGE_THUMBNAIL_SIZE))
        .flex_none()
        .rounded(px(6.0))
        .overflow_hidden()
        .object_fit(ObjectFit::Cover)
        .into_any_element()
}

/// The labels the appearance page paints for the wallpaper controls.
///
/// They live here rather than at the call site because the settings search
/// finds a row by its own title: the page and the search index read the same
/// string, so a row can never be listed under a title it does not carry.
pub(crate) fn setting_title() -> &'static str {
    locale::text("Background image", "背景图片", "背景圖片")
}

pub(crate) fn setting_description() -> &'static str {
    locale::text(
        "Paint an image of your own behind the workbench. The theme's panels turn translucent so it shows through, and the veil below keeps the text on them readable. A GIF or animated WebP loops while the window is active, and holds still under reduced motion.",
        "在工作台后面铺一张自己的图片。主题面板会变为半透明以透出图片，下方暗化用于保证文字可读。GIF 或动态 WebP 会在窗口激活时循环播放，开启减少动效时保持静止。",
        "在工作台後面鋪一張自己的圖片。主題面板會變為半透明以透出圖片，下方暗化用於保證文字可讀。GIF 或動態 WebP 會在視窗啟用時循環播放，開啟減少動效時保持靜止。",
    )
}

/// The localized name of a fit, for the settings control.
pub(crate) fn fit_label(fit: BackgroundImageFit) -> &'static str {
    match fit {
        BackgroundImageFit::Cover => locale::text("Fill the window", "铺满窗口", "鋪滿視窗"),
        BackgroundImageFit::Contain => locale::text("Show it all", "完整显示", "完整顯示"),
        BackgroundImageFit::Fill => locale::text("Stretch to fit", "拉伸填充", "拉伸填充"),
    }
}

/// The row that offers the three fits, described as the set of choices rather
/// than as the one selected, because that is what the row is for.
pub(crate) fn fit_title() -> &'static str {
    locale::text("Fit", "填充方式", "填充方式")
}

pub(crate) fn fit_setting_description() -> &'static str {
    locale::text(
        "Fill the window and crop whatever overflows, show the whole image inside it, or stretch it to the window's shape.",
        "铺满窗口并裁掉超出部分、完整显示整张图片，或将图片拉伸到窗口比例。",
        "鋪滿視窗並裁掉超出部分、完整顯示整張圖片，或將圖片拉伸到視窗比例。",
    )
}

pub(crate) fn veil_title() -> &'static str {
    locale::text("Image veil", "图片暗化", "圖片暗化")
}

pub(crate) fn veil_description() -> &'static str {
    locale::text(
        "How much of the theme's page color is laid over the image. Raise it when text is hard to read on a bright picture.",
        "主题底色覆盖在图片上的比例。图片较亮、文字不易阅读时调高。",
        "主題底色覆蓋在圖片上的比例。圖片較亮、文字不易閱讀時調高。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny valid PNG, written through the `image` crate so the fixture is a
    /// real decodable image rather than a hand-copied byte blob.
    fn write_png(directory: &Path, name: &str, width: u32, height: u32) -> PathBuf {
        write_png_in_color(directory, name, width, height, [10, 20, 30, 255])
    }

    fn write_png_in_color(
        directory: &Path,
        name: &str,
        width: u32,
        height: u32,
        color: [u8; 4],
    ) -> PathBuf {
        let path = directory.join(name);
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        image
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        path
    }

    /// A real animated GIF: `frames` frames that each differ by one pixel, so a
    /// decoder cannot collapse them into a still.
    fn write_animated_gif(
        directory: &Path,
        name: &str,
        width: u32,
        height: u32,
        frames: u32,
    ) -> PathBuf {
        let path = directory.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = image::codecs::gif::GifEncoder::new(std::io::BufWriter::new(file));
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .unwrap();
        for index in 0..frames {
            let pixel = image::Rgba([index as u8, 20, 30, 255]);
            let frame = image::RgbaImage::from_pixel(width, height, pixel);
            encoder
                .encode_frame(image::Frame::from_parts(
                    frame,
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(40, 1),
                ))
                .unwrap();
        }
        drop(encoder);
        path
    }

    #[test]
    fn an_imported_image_is_stored_under_its_content_digest() {
        let home = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = write_png(source_dir.path(), "wallpaper.png", 64, 40);

        let file_name = import(&source, home.path()).unwrap();
        assert!(
            background_image_file_name_is_valid(&file_name),
            "{file_name} is not a content-addressed name"
        );
        let stored = path(home.path(), &file_name);
        assert!(stored.is_file());

        // Equal content reuses the file, and a replacement image at the same
        // source path gets its own name rather than invalidating the old one.
        assert_eq!(import(&source, home.path()).unwrap(), file_name);
        let replacement = write_png_in_color(
            source_dir.path(),
            "wallpaper-2.png",
            64,
            40,
            [200, 40, 10, 255],
        );
        std::fs::copy(&replacement, &source).unwrap();
        let next = import(&source, home.path()).unwrap();
        assert_ne!(next, file_name);
        assert!(path(home.path(), &next).is_file());
        assert!(
            stored.is_file(),
            "the replaced image stays until the user clears it"
        );
    }

    #[test]
    fn a_stored_image_must_be_resolvable_and_decodable_to_be_imported() {
        let home = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();

        let unsupported = source_dir.path().join("wallpaper.svg");
        std::fs::write(&unsupported, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
        let error = import(&unsupported, home.path()).unwrap_err();
        assert!(error.contains("PNG"), "unexpected rejection: {error}");

        let not_an_image = source_dir.path().join("wallpaper.png");
        std::fs::write(&not_an_image, b"not an image at all").unwrap();
        assert!(import(&not_an_image, home.path()).is_err());

        assert!(import(source_dir.path(), home.path()).is_err());

        let named_like_a_traversal = source_dir.path().join("wallpaper.jpg");
        std::fs::write(&named_like_a_traversal, b"still not an image").unwrap();
        assert!(import(&named_like_a_traversal, home.path()).is_err());
    }

    #[test]
    fn resolution_ignores_a_missing_home_name_or_file() {
        let home = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let file_name = import(
            &write_png(source_dir.path(), "wallpaper.png", 32, 32),
            home.path(),
        )
        .unwrap();
        let stored = BackgroundImageUiState {
            file: Some(file_name.clone()),
            ..BackgroundImageUiState::default()
        };

        assert_eq!(
            resolve(Some(home.path()), &stored),
            Some(path(home.path(), &file_name))
        );
        assert_eq!(resolve(None, &stored), None);
        assert_eq!(
            resolve(
                Some(home.path()),
                &BackgroundImageUiState {
                    file: Some("../../etc/passwd.png".to_string()),
                    ..BackgroundImageUiState::default()
                }
            ),
            None
        );
        std::fs::remove_file(path(home.path(), &file_name)).unwrap();
        assert_eq!(resolve(Some(home.path()), &stored), None);
    }

    #[test]
    fn every_fit_maps_to_the_matching_object_fit() {
        assert!(matches!(
            object_fit(BackgroundImageFit::Cover),
            ObjectFit::Cover
        ));
        assert!(matches!(
            object_fit(BackgroundImageFit::Contain),
            ObjectFit::Contain
        ));
        assert!(matches!(
            object_fit(BackgroundImageFit::Fill),
            ObjectFit::Fill
        ));
    }

    /// A GIF or animated WebP is a wallpaper that loops, so it is accepted —
    /// and its frames are counted, because the renderer keeps every one of them
    /// in memory.
    #[test]
    fn an_animated_image_is_accepted_and_its_frames_are_counted() {
        let home = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = write_animated_gif(source_dir.path(), "loop.gif", 64, 40, 6);

        assert_eq!(animation_frames(&source, "gif"), Some(6));
        let file_name = import(&source, home.path()).unwrap();
        assert!(background_image_file_name_is_valid(&file_name));
        assert!(file_name.ends_with(".gif"), "{file_name} lost its format");
        assert!(path(home.path(), &file_name).is_file());
    }

    /// Only a format that can animate reports frames; a still never pays for the
    /// budget, and an unreadable file is rejected later by the dimension check
    /// rather than by a false frame count.
    #[test]
    fn a_still_image_reports_no_frames_to_budget() {
        let source_dir = tempfile::tempdir().unwrap();
        let still = write_png(source_dir.path(), "still.png", 32, 32);
        assert_eq!(animation_frames(&still, "png"), None);
        assert_eq!(animation_frames(&still, "bmp"), None);

        let broken = source_dir.path().join("broken.gif");
        std::fs::write(&broken, b"not a gif").unwrap();
        assert_eq!(animation_frames(&broken, "gif"), None);
    }

    /// An animation is bounded by its whole decoded size, not by its canvas: a
    /// long or large loop is rejected before the file is copied, with a message
    /// that says what to do about it.
    #[test]
    fn an_animation_larger_than_the_budget_is_rejected() {
        let home = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();

        // 400x400 for 400 frames is 64 million pixels of frames, past the
        // 48-million budget, on a canvas that is far inside the still one.
        let heavy = write_animated_gif(source_dir.path(), "heavy.gif", 400, 400, 400);
        let error = import(&heavy, home.path()).unwrap_err();
        assert!(error.contains("720p"), "unexpected rejection: {error}");
        assert!(
            !home.path().join(BACKGROUND_IMAGE_DIRECTORY).exists(),
            "a rejected import stores nothing at all"
        );

        // The same canvas with a handful of frames is inside it.
        let light = write_animated_gif(source_dir.path(), "light.gif", 400, 400, 12);
        assert!(import(&light, home.path()).is_ok());
    }

    /// An animated wallpaper loops only if its image element declares an id: the
    /// frame cursor lives in that element's state, and gpui's animated path is
    /// gated on the element having one at all.
    ///
    /// The loop itself cannot be observed here — gpui advances frames only while
    /// the window reports itself active, and the test platform's windows report
    /// themselves inactive — so this asserts the property the framework asks
    /// for, through the same `Element::id`.
    #[test]
    fn the_wallpaper_image_declares_the_id_an_animation_loops_under() {
        let state = BackgroundImageUiState::default();
        let image = wallpaper_image(&state, Path::new("/nonexistent/wallpaper.gif"));

        // Qualified: `InteractiveElement::id` is the builder that *sets* one.
        match gpui::Element::id(&image) {
            Some(gpui::ElementId::Name(name)) => {
                assert_eq!(name.as_ref(), WALLPAPER_IMAGE_ELEMENT_ID)
            }
            other => panic!(
                "an animated wallpaper needs a named element id to keep its frame cursor, got \
                 {other:?}"
            ),
        }
    }
}
