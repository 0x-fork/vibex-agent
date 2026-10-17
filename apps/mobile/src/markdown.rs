//! Markdown rendering for the mobile timeline.
//!
//! Rendering belongs to the gpui-component text view, exactly as it does on the
//! desktop: this module only holds the state a streamed row keeps across
//! frames — the source it was last handed and the revision that produced it —
//! and hands the text view the themed presentation.

use std::sync::Arc;

use gpui::{
    App, ClickEvent, Context, ElementId, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, div,
};
use gpui_base::text::{TextView, TextViewStyle};
use vibex_ui::markdown::plain_text;

use crate::theme;

/// What a click on a rendered link does.
///
/// The text view is a shared component, so the client that owns the document
/// hands it the behaviour: the timeline's prose lets the toolkit open URLs,
/// while a user message's attachment links point at their own message and are
/// opened by the page.
pub type MarkdownLinkHandler =
    Arc<dyn Fn(&SharedString, &ClickEvent, &mut Window, &mut App) + Send + Sync + 'static>;

pub struct MarkdownView {
    source: Arc<str>,
    revision: u64,
    muted: bool,
    link_handler: Option<MarkdownLinkHandler>,
}

impl MarkdownView {
    pub fn new(source: Arc<str>, revision: u64, _cx: &mut Context<Self>) -> Self {
        Self {
            source,
            revision,
            muted: false,
            link_handler: None,
        }
    }

    /// Hands the view its document and the behaviour of its links.
    ///
    /// The handler is replaced on every frame without a repaint of its own: it
    /// closes over the attachments of the row it was built for, and a row whose
    /// source has not changed does not need the text view told twice, while the
    /// handler must follow whatever the newest frame resolved.
    pub fn set_document(
        &mut self,
        source: Arc<str>,
        revision: u64,
        link_handler: Option<MarkdownLinkHandler>,
        cx: &mut Context<Self>,
    ) {
        if self.revision == revision && self.source.as_ref() == source.as_ref() {
            self.link_handler = link_handler;
            return;
        }
        self.source = source;
        self.revision = revision;
        self.link_handler = link_handler;
        cx.notify();
    }

    pub fn set_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        if self.muted != muted {
            self.muted = muted;
            cx.notify();
        }
    }

    pub fn set_source(&mut self, source: Arc<str>, revision: u64, cx: &mut Context<Self>) {
        self.set_document(source, revision, self.link_handler.clone(), cx);
    }
}

/// The plain text a Markdown source projects to, for reasoning summaries.
pub fn markdown_plain_text(source: &str) -> String {
    plain_text(source)
}

/// Commentary and answers use the body foreground; reasoning stays secondary.
fn text_view_style(muted: bool, cx: &App) -> TextViewStyle {
    TextViewStyle::from_theme(&gpui_base::Theme::global(cx)).with_foreground(if muted {
        theme::text_muted()
    } else {
        theme::text_primary()
    })
}

pub fn render(
    source: Arc<str>,
    revision: u64,
    muted: bool,
    cx: &mut Context<MarkdownView>,
) -> MarkdownView {
    let mut view = MarkdownView::new(source, revision, cx);
    view.muted = muted;
    view
}

impl Render for MarkdownView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(theme::font_body())
            .line_height(theme::line_height_for(theme::FONT_BODY))
            .text_color(if self.muted {
                theme::text_muted()
            } else {
                theme::text_primary()
            })
            .child({
                let mut view = TextView::markdown(
                    ElementId::Name(SharedString::from(format!(
                        "mobile-markdown:{}",
                        cx.entity_id()
                    ))),
                    self.source.clone(),
                )
                .style(text_view_style(self.muted, cx));
                if let Some(handler) = self.link_handler.clone() {
                    view = view.on_link_click(move |url, event, window, cx| {
                        handler(url, event, window, cx)
                    });
                }
                view
            })
    }
}
