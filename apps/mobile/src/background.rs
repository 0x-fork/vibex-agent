//! The user's own background image on the phone.
//!
//! The desktop answers "what is behind the workbench" with a managed file under
//! the runtime home; the phone cannot reach that home, and its picker never
//! hands out a path at all. This is the mobile half of the same feature, built
//! on the same contract: only a content-addressed file name is persisted —
//! never an external path, never image bytes — and the bytes live in the app's
//! own data directory. The name is the SHA-256 of the file, so re-importing one
//! image reuses one file and replacing it produces a new name rather than
//! invalidating a cached decode.
//!
//! The picker delivers a `data:` URL rather than a path, which is the one form
//! the Android host can hand across the JNI boundary for an item the app was
//! granted read access to. Decoding is therefore part of the import rather than
//! the paint: a wallpaper is written once and painted from disk afterwards.
//!
//! A missing directory, a malformed stored name, or a file that has been
//! deleted is not an error the reader has to see: resolution simply reports "no
//! image" and the page paints the theme's own color again.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use gpui::{
    AnyElement, InteractiveElement as _, IntoElement as _, ObjectFit, ParentElement as _,
    Styled as _, StyledImage as _, div, img, px,
};
use sha2::{Digest, Sha256};
use vibex_desktop_model::{
    BackgroundImageFit, BackgroundImageUiState, background_image_file_name_is_valid,
};
use vibex_ui::WallpaperSurfaces;

/// Directory under the app's data directory that holds imported wallpapers.
pub(crate) const BACKGROUND_IMAGE_DIRECTORY: &str = "backgrounds";

/// The largest source file the importer accepts.
///
/// Generous next to the attachments' 8 MiB — a wallpaper is a photograph or a
/// piece of artwork, and it is stored on the phone rather than relayed — but it
/// is still a ceiling, because the file is decoded, held, hashed, and written.
const BACKGROUND_IMAGE_MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;

/// The largest source image the importer accepts, before any decode.
///
/// Bounded separately from the byte size: a small file can name a huge canvas,
/// and a canvas is what the renderer has to upload.
const BACKGROUND_IMAGE_MAX_SOURCE_PIXELS: u64 = 40_000_000;

/// The preview size the Appearance page paints a stored image at.
pub(crate) const BACKGROUND_IMAGE_THUMBNAIL_SIZE: f32 = 36.0;

/// Where the configured wallpaper lives, if it is configured, correctly named,
/// and still on disk.
///
/// The stat is what makes a deleted file degrade to the theme's color instead
/// of painting an empty surface. It runs when an appearance is applied and once
/// per frame while the page paints, which one `stat` covers comfortably.
pub(crate) fn resolve(data_dir: &Path, state: &BackgroundImageUiState) -> Option<PathBuf> {
    let file_name = state.file.as_deref()?;
    if !background_image_file_name_is_valid(file_name) {
        return None;
    }
    let path = data_dir.join(BACKGROUND_IMAGE_DIRECTORY).join(file_name);
    path.is_file().then_some(path)
}

/// The bytes behind a picked `data:` URL, and the extension they should be
/// stored under.
///
/// The MIME type in the header is what the host reported, but the bytes are
/// sniffed too: the store's name carries an extension and the renderer's decode
/// trusts nothing else, so a mislabelled pick has to be caught here rather than
/// written under a name that lies about its format.
fn decode_picked(data_url: &str) -> Result<(Vec<u8>, &'static str), String> {
    let trimmed = data_url.trim();
    let Some(rest) = trimmed.strip_prefix("data:") else {
        return Err("the picked image did not arrive as image data".to_string());
    };
    let Some((header, encoded)) = rest.split_once(',') else {
        return Err("the picked image did not arrive as image data".to_string());
    };
    if !header
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"))
    {
        return Err("the picked image was not base64-encoded".to_string());
    }
    // STANDARD base64 expands three bytes into four characters. Reject an
    // oversized payload before decoding so a malformed payload cannot cause an
    // unbounded temporary allocation.
    if encoded.len() > BACKGROUND_IMAGE_MAX_SOURCE_BYTES.saturating_mul(4) / 3 + 4 {
        return Err("the picked image is larger than 32 MB".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| "the picked image could not be decoded".to_string())?;
    if bytes.is_empty() {
        return Err("the picked image is empty".to_string());
    }
    if bytes.len() > BACKGROUND_IMAGE_MAX_SOURCE_BYTES {
        return Err("the picked image is larger than 32 MB".to_string());
    }
    let format = image::guess_format(&bytes)
        .map_err(|_| "the picked file is not a supported image".to_string())?;
    let extension = match format {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpeg",
        image::ImageFormat::WebP => "webp",
        image::ImageFormat::Gif => "gif",
        _ => return Err("choose a PNG, JPEG, WebP, or GIF image".to_string()),
    };
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|_| "the picked file is not a supported image".to_string())?;
    if width == 0
        || height == 0
        || u64::from(width)
            .checked_mul(u64::from(height))
            .is_none_or(|pixels| pixels > BACKGROUND_IMAGE_MAX_SOURCE_PIXELS)
    {
        return Err("the picked image dimensions are too large".to_string());
    }
    Ok((bytes, extension))
}

/// Imports one picked image into the managed wallpaper directory and returns
/// the file name it is stored under.
///
/// The stored file keeps its original bytes: a wallpaper is not resized or
/// re-encoded on the way in, so there is no generation loss and an animation
/// stays an animation. Equal content reuses one file, and the copy lands
/// through a temporary name so a failed import never leaves a half-written
/// image behind.
pub(crate) fn import_picked(data_url: &str, data_dir: &Path) -> Result<String, String> {
    let (bytes, extension) = decode_picked(data_url)?;
    let digest = {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    };
    let file_name = format!("{digest}.{extension}");
    if !background_image_file_name_is_valid(&file_name) {
        return Err("the picked image could not be named".to_string());
    }
    let directory = data_dir.join(BACKGROUND_IMAGE_DIRECTORY);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create wallpaper storage: {error}"))?;
    let destination = directory.join(&file_name);
    if destination.is_file() {
        return Ok(file_name);
    }
    let temporary = directory.join(format!(".{file_name}.tmp"));
    std::fs::write(&temporary, &bytes)
        .and_then(|()| std::fs::rename(&temporary, &destination))
        .map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            format!("could not store the wallpaper: {error}")
        })?;
    Ok(file_name)
}

/// How the stored image is fitted to the page.
pub(crate) fn object_fit(fit: BackgroundImageFit) -> ObjectFit {
    match fit {
        BackgroundImageFit::Cover => ObjectFit::Cover,
        BackgroundImageFit::Contain => ObjectFit::Contain,
        BackgroundImageFit::Fill => ObjectFit::Fill,
    }
}

/// The element id the wallpaper image paints under.
///
/// It is what an animated GIF needs to loop: the frame cursor lives in an
/// element's own state, and an element that declares no id has nowhere to keep
/// one — it would paint its first frame forever. The loop stops on its own
/// while the window is inactive or the reader asked for reduced motion. The
/// thumbnail in the settings page is a different element and takes no id, so a
/// preview stays a still.
pub(crate) const WALLPAPER_IMAGE_ELEMENT_ID: &str = "mobile-background-image";

/// The image and its readability veil, painted over the page's opaque plate.
///
/// This is the first child of the page, so it paints behind every surface. The
/// image covers the plate; the veil over it is what keeps the text on those
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
        .child(
            img(path.to_path_buf())
                .id(WALLPAPER_IMAGE_ELEMENT_ID)
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(object_fit(state.fit)),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(wallpaper_veil(is_dark, state.dim_percent)),
        )
        .into_any_element()
}

/// A small preview of the stored image for the Appearance page.
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

/// The veil painted over a stored image, mixed from the page color rather than
/// from black: in a light appearance the veil is the theme's own light page
/// color, so the ink that reads over it keeps the contrast the palette was
/// tuned for, and in a dark appearance it darkens the image the same way.
pub(crate) fn wallpaper_veil(is_dark: bool, dim_percent: u16) -> gpui::Hsla {
    let alpha = f32::from(dim_percent.min(100)) / 100.0;
    crate::theme::page_plate(is_dark).alpha(alpha)
}

/// The page color at full opacity, which the wallpaper is painted over.
///
/// While a wallpaper is behind them the theme's containers fade, so the page's
/// own fill turns translucent — and a `Contain` fit's letterbox would then show
/// the window's plate instead of the page. Mixing the veil from the authored
/// color is what keeps the two reading as one surface.
pub(crate) fn surface_alpha(role: &str, is_dark: bool) -> Option<f32> {
    let surfaces = WallpaperSurfaces::for_appearance(is_dark);
    let _ = surfaces;
    vibex_ui::wallpaper_surface_alpha(role, is_dark)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_desktop_model::BackgroundImageUiState;

    /// A one-pixel PNG, base64-encoded, which is the smallest real image the
    /// importer can be handed.
    const PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==";

    fn data_url(mime: &str, encoded: &str) -> String {
        format!("data:{mime};base64,{encoded}")
    }

    #[test]
    fn a_picked_image_is_stored_under_its_content_hash() {
        let temp = tempfile::tempdir().unwrap();
        let name = import_picked(&data_url("image/png", PIXEL_PNG), temp.path()).unwrap();
        assert!(background_image_file_name_is_valid(&name));
        assert!(name.ends_with(".png"));
        // The same bytes are the same name: a re-import reuses one file rather
        // than growing the directory by a copy.
        let again = import_picked(&data_url("image/png", PIXEL_PNG), temp.path()).unwrap();
        assert_eq!(again, name);
        let stored = temp.path().join(BACKGROUND_IMAGE_DIRECTORY).join(&name);
        assert!(stored.is_file());
        assert_eq!(resolve(temp.path(), &configured(&name)), Some(stored));
    }

    fn configured(file_name: &str) -> BackgroundImageUiState {
        BackgroundImageUiState {
            file: Some(file_name.to_string()),
            ..BackgroundImageUiState::default()
        }
    }

    #[test]
    fn a_pick_that_is_not_an_image_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let plain = base64::engine::general_purpose::STANDARD.encode(b"not an image");
        for url in [
            data_url("image/png", &plain),
            data_url("image/png", "!!!"),
            "https://example.com/a.png".to_string(),
            String::new(),
        ] {
            assert!(
                import_picked(&url, temp.path()).is_err(),
                "{url:?} must not be stored as a wallpaper"
            );
        }
        assert!(
            !temp.path().join(BACKGROUND_IMAGE_DIRECTORY).exists(),
            "a rejected pick leaves no storage behind"
        );
    }

    #[test]
    fn a_name_that_is_not_content_addressed_resolves_to_nothing() {
        let temp = tempfile::tempdir().unwrap();
        for file_name in [
            "../../../etc/passwd.png",
            "/data/user/0/ai.vibex/mobile/backgrounds/a.png",
            "a.png",
            &format!("{}.svg", "a".repeat(64)),
        ] {
            assert_eq!(resolve(temp.path(), &configured(file_name)), None);
        }
        // And a correctly named file that is not on disk is no image either,
        // which is what makes a deleted wallpaper fall back to the theme.
        let missing = format!("{}.png", "a".repeat(64));
        assert_eq!(resolve(temp.path(), &configured(&missing)), None);
    }

    #[test]
    fn an_unconfigured_appearance_resolves_to_no_image() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve(temp.path(), &BackgroundImageUiState::default()),
            None
        );
    }
}
