//! Workspace images embedded in session Markdown.
//!
//! The text view only decodes `data:` URLs by itself; every other destination
//! is handed to GPUI's asset loader as a URI, which fetches over HTTP. A
//! transcript that points at a file in the session workspace therefore needs a
//! resolver: this plugin claims those image nodes, renders them from the
//! canonical workspace path, and turns a click into the full-window image
//! preview the attachment surfaces already use.

use std::{
    hash::{Hash as _, Hasher as _},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use gpui::{
    App, ElementId, ImageSource, InteractiveElement as _, ObjectFit, Resource, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, img, relative,
};
use gpui_base::text::{
    InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
    markdown_ast::Node as MarkdownAst,
};
use percent_encoding::percent_decode_str;

/// Opens a workspace image in the full-window preview.
pub(super) type SessionImageOpen =
    Arc<dyn Fn(PathBuf, String, &mut Window, &mut App) + Send + Sync>;

const NAME: &str = "vibex-session-image";
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// A local image claimed from a Markdown `Image` node.
struct SessionImage {
    path: PathBuf,
    label: String,
}

/// Claims workspace image nodes in a session's Markdown surface.
pub(super) struct SessionImagePlugin {
    workspace_root: PathBuf,
    /// Canonicalized on the first image the parser resolves, not on every
    /// frame: the plugin is rebuilt for every Markdown view render.
    canonical_root: OnceLock<Option<PathBuf>>,
    open: SessionImageOpen,
    parser_revision: u64,
}

impl SessionImagePlugin {
    pub(super) fn new(workspace_root: PathBuf, open: SessionImageOpen) -> Self {
        let parser_revision = {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            workspace_root.hash(&mut hasher);
            hasher.finish()
        };
        Self {
            workspace_root,
            canonical_root: OnceLock::new(),
            open,
            parser_revision,
        }
    }

    /// A stable parser identity for this workspace. Rebuilding the plugin each
    /// frame keeps the parsed tree; changing the workspace reparses it.
    pub(super) fn parser_revision(&self) -> u64 {
        self.parser_revision
    }

    /// Resolve a Markdown image destination to a file inside the workspace.
    ///
    /// `data:` and HTTP(S) destinations are left to the built-in renderer.
    /// Everything else is treated as a workspace path: relative to the
    /// workspace root, root-relative when it starts with `/`, or absolute when
    /// it already lives under the root. `file://` is accepted too. The result
    /// must canonicalize back inside the workspace, name a regular file with an
    /// image extension, and stay under the same byte budget the Markdown
    /// preview uses.
    fn resolve(&self, url: &str) -> Option<PathBuf> {
        // A destination may carry a query or fragment the file never has; the
        // Markdown link resolver drops them too. Percent-encoded `?`/`#` stay
        // part of the name and are decoded below.
        let url = url
            .trim()
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .trim();
        if url.is_empty() {
            return None;
        }
        let lowercase = url.to_ascii_lowercase();
        if lowercase.starts_with("data:")
            || lowercase.starts_with("http://")
            || lowercase.starts_with("https://")
        {
            return None;
        }

        // Compare canonical paths on both sides: the workspace may sit behind a
        // symlink (`/var` on macOS), and a lexical prefix would then reject
        // every real file under it.
        let root = self
            .canonical_root
            .get_or_init(|| self.workspace_root.canonicalize().ok());
        let root = root.as_deref()?;

        let path = if lowercase.starts_with("file:") {
            // The URL parser decodes percent escapes once; decoding first
            // would turn `%2520` into a space instead of a literal `%20`.
            let path = url::Url::parse(url).ok()?.to_file_path().ok()?;
            path.is_absolute().then_some(path)?
        } else {
            let decoded = percent_decode_str(url).decode_utf8().ok()?;
            let path = Path::new(decoded.as_ref());
            if !path.is_absolute() && url::Url::parse(decoded.as_ref()).is_ok() {
                // A non-file scheme (`asset:`, `vibex:`, ...) is not a local
                // path. Absolute Windows paths are checked above because
                // `Url::parse` reads their drive letter as a scheme.
                return None;
            }
            let relative = path.strip_prefix("/").unwrap_or(path);
            if path.is_absolute() && path.starts_with(root) {
                path.to_path_buf()
            } else {
                // A Markdown path that starts with `/` is workspace-relative,
                // matching how links are resolved elsewhere. Joining an
                // absolute path outside the root replaces the root on Windows
                // and the containment check below then rejects it.
                self.workspace_root.join(relative)
            }
        };

        let canonical = path.canonicalize().ok()?;
        if !canonical.starts_with(root) || !canonical.is_file() {
            return None;
        }
        if canonical.metadata().ok()?.len() > MAX_IMAGE_BYTES {
            return None;
        }
        // Extension (or MIME, when one is known) decides whether the preview
        // can decode it; a text file named `notes.png` is still rejected later
        // by the decoder, but this keeps the renderer from trying.
        super::attachment_preview_image_format(&canonical, None)?;
        Some(canonical)
    }
}

impl MarkdownPlugin for SessionImagePlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn parse(&self, node: &MarkdownAst, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let MarkdownAst::Image(image) = node else {
            return None;
        };
        let path = self.resolve(&image.url)?;
        let label = if image.alt.trim().is_empty() {
            path.file_name()?.to_string_lossy().into_owned()
        } else {
            image.alt.clone()
        };
        // Keep the original syntax, including a reference definition or title,
        // so selecting and copying the image round-trips as the author wrote it.
        let markdown = cx
            .node_source(node)
            .map(str::to_string)
            .unwrap_or_else(|| format!("![{}]({})", image.alt, image.url));
        let data = SessionImage {
            path,
            label: label.clone(),
        };
        Some(
            MarkdownNode::new(NAME, data)
                .text(label.clone())
                .markdown(markdown)
                .accessibility_label(label),
        )
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        _context: &InlineRenderContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<InlineElement> {
        let image = node.data::<SessionImage>()?;
        let source_path: Arc<Path> = Arc::from(image.path.clone().into_boxed_path());
        let source = ImageSource::Resource(Resource::Path(source_path));
        let open = self.open.clone();
        let path = image.path.clone();
        let label = image.label.clone();
        let start = node.source_range().map(|range| range.start).unwrap_or(0);
        let id = SharedString::from(format!("session-image:{start}:{}", image.path.display()));
        let element = img(source)
            .id(ElementId::Name(id))
            .debug_selector(|| "vibex-session-image".to_string())
            .object_fit(ObjectFit::Contain)
            .max_w(relative(1.0))
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                open(path.clone(), label.clone(), window, cx);
            });
        Some(InlineElement::new(element))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_base::text::MarkdownExtensions;
    use std::fs;

    fn plugin(root: &Path) -> SessionImagePlugin {
        SessionImagePlugin::new(root.to_path_buf(), Arc::new(|_, _, _, _| {}))
    }

    fn write_image(root: &Path, relative: &str) -> PathBuf {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        path
    }

    fn canonical(path: &Path) -> PathBuf {
        path.canonicalize().unwrap()
    }

    #[test]
    fn resolves_workspace_relative_and_root_relative_paths() {
        let root = tempfile::tempdir().unwrap();
        let image = write_image(root.path(), "images/demo.png");
        let image = canonical(&image);
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("images/demo.png"), Some(image.clone()));
        assert_eq!(plugin.resolve("images/demo.png?raw=1"), Some(image.clone()));
        assert_eq!(
            plugin.resolve("images/demo.png#preview"),
            Some(image.clone())
        );
        assert_eq!(plugin.resolve("/images/demo.png"), Some(image.clone()));
        assert_eq!(plugin.resolve("./images/../images/demo.png"), Some(image));
    }

    #[test]
    fn resolves_absolute_and_file_urls_inside_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let image = write_image(root.path(), "shot.png");
        let image = canonical(&image);
        let plugin = plugin(root.path());

        assert_eq!(
            plugin.resolve(image.to_string_lossy().as_ref()),
            Some(image.clone())
        );
        let url = url::Url::from_file_path(&image).unwrap();
        assert_eq!(plugin.resolve(url.as_str()), Some(image));
    }

    #[test]
    fn resolves_percent_encoded_names() {
        let root = tempfile::tempdir().unwrap();
        let image = write_image(root.path(), "images/two words.png");
        let image = canonical(&image);
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("images/two%20words.png"), Some(image));
    }

    #[test]
    fn decodes_percent_escapes_once() {
        let root = tempfile::tempdir().unwrap();
        let image = write_image(root.path(), "images/100%20.png");
        let image = canonical(&image);
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("images/100%2520.png"), Some(image.clone()));
        let url = url::Url::from_file_path(&image).unwrap();
        assert_eq!(plugin.resolve(url.as_str()), Some(image));
    }

    #[test]
    fn leaves_embedded_and_remote_images_to_the_built_in_renderer() {
        let root = tempfile::tempdir().unwrap();
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("data:image/png;base64,iVBORw0KGgo="), None);
        assert_eq!(plugin.resolve("https://example.com/a.png"), None);
        assert_eq!(plugin.resolve("http://example.com/a.png"), None);
    }

    #[test]
    fn rejects_paths_that_escape_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir_in(root.path().parent().unwrap()).unwrap();
        let outside_image = write_image(outside.path(), "secret.png");
        let plugin = plugin(root.path());

        let relative = format!(
            "../{}/secret.png",
            outside.path().file_name().unwrap().to_string_lossy()
        );
        assert_eq!(plugin.resolve(&relative), None);
        assert_eq!(
            plugin.resolve(outside_image.to_string_lossy().as_ref()),
            None
        );
        let url = url::Url::from_file_path(&outside_image).unwrap();
        assert_eq!(plugin.resolve(url.as_str()), None);
    }

    #[test]
    fn rejects_non_images_directories_and_oversized_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("notes.txt"), b"not an image").unwrap();
        fs::create_dir(root.path().join("picture.png")).unwrap();
        let oversized = root.path().join("huge.png");
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&oversized)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES + 1)
            .unwrap();
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("notes.txt"), None);
        assert_eq!(plugin.resolve("picture.png"), None);
        assert_eq!(plugin.resolve("huge.png"), None);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_that_escapes_the_workspace() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_image = write_image(outside.path(), "secret.png");
        symlink(&outside_image, root.path().join("link.png")).unwrap();
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("link.png"), None);
    }

    const ONE_PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

    #[gpui::test]
    fn a_local_image_parses_and_draws_in_a_text_view(cx: &mut gpui::TestAppContext) {
        use std::sync::atomic::{AtomicBool, Ordering};

        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use gpui::{AppContext as _, Context, Entity, IntoElement, Modifiers, Render, Window};

        use crate::markdown::{TextView, TextViewState};

        struct Host {
            state: Entity<TextViewState>,
            extensions: MarkdownExtensions,
        }

        impl Render for Host {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                TextView::new(&self.state)
                    .markdown_extensions(self.extensions.clone())
                    .text_sm()
            }
        }

        cx.update(gpui_component::init);
        let root = tempfile::tempdir().unwrap();
        let bytes = STANDARD.decode(ONE_PIXEL_PNG).unwrap();
        fs::write(root.path().join("shot.png"), bytes).unwrap();
        let clicked = Arc::new(AtomicBool::new(false));
        let clicked_flag = clicked.clone();
        let plugin = SessionImagePlugin::new(
            root.path().to_path_buf(),
            Arc::new(move |_, _, _, _| {
                clicked_flag.store(true, Ordering::SeqCst);
            }),
        );
        let extensions = MarkdownExtensions::default()
            .plugin(plugin)
            .parser_revision(1);
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("![alt](shot.png)", cx)));
        let state_for_host = state.clone();
        let (host, cx) = cx.add_window_view(move |_, _| Host {
            state: state_for_host,
            extensions,
        });
        for _ in 0..4 {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
        }
        cx.run_until_parked();
        let rendered = state.read_with(cx, |state, _| state.rendered_text().as_str().to_string());
        assert!(rendered.contains("alt"), "{rendered}");
        let bounds = cx
            .debug_bounds("vibex-session-image")
            .expect("the local image should be claimed by the session plugin");
        cx.simulate_click(bounds.center(), Modifiers::none());
        assert!(
            clicked.load(Ordering::SeqCst),
            "clicking the inline image should open its preview",
        );
        assert!(host.read_with(cx, |host, _| host.state.entity_id() == state.entity_id()));
    }
}
