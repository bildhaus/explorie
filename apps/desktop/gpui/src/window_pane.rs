//! Cached child views for the file listing and the sidebar.
//!
//! [`DirectoryWindow`] renders the whole window, so anything that notifies
//! it rebuilds every area. A [`WindowPane`] is a thin view that renders one
//! area with the window's own render method, which lets the window embed
//! that area as a cached view: GPUI then reuses the area's previous layout
//! and paint unless the pane itself was notified. A media player tick, for
//! example, re-renders the player and the window root but not the listing
//! or the sidebar.
//!
//! Each pane observes the window, so every notification of the window still
//! re-renders both panes; elements inside a pane that notify their view
//! (hover, scrolling, image loads) re-render just that pane. Listeners in
//! the pane are created with the window's context and keep acting on the
//! window.
//!
//! Two things the window has to keep in mind:
//!
//! * A notification sent while rendering does not run observers, so code
//!   that renders inside a pane must defer a notification that asks for
//!   another render (see the column view's scroll-to-leaf retries).
//! * Values the window computes while rendering and the panes read are
//!   listed in [`PaneInputs`]; when they change without a notification the
//!   panes render uncached for that frame.

use gpui::{Element, GlobalElementId, InspectorElementId, LayoutId, Position, Style, WeakEntity};

use crate::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaneKind {
    Listing,
    Sidebar,
}

pub(crate) struct WindowPane {
    window: WeakEntity<DirectoryWindow>,
    kind: PaneKind,
    _observe_window: Subscription,
}

impl WindowPane {
    pub(crate) fn new(
        window: &Entity<DirectoryWindow>,
        kind: PaneKind,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            window: window.downgrade(),
            kind,
            _observe_window: cx.observe(window, |_, _, cx| cx.notify()),
        }
    }
}

impl Render for WindowPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(window) = self.window.upgrade() else {
            return div().into_any_element();
        };
        let kind = self.kind;
        let content = window.update(cx, |window, cx| {
            window.panes.record_render(kind);
            match kind {
                PaneKind::Listing => window.render_listing(cx),
                PaneKind::Sidebar => window.render_sidebar(cx),
            }
        });
        // The pane is laid out as its own root within the bounds its cached
        // style gives it; this container recreates the flex parent the area
        // was a child of.
        match kind {
            PaneKind::Listing => div().size_full().flex().flex_col().child(content),
            PaneKind::Sidebar => div().size_full().flex().child(content),
        }
        .into_any_element()
    }
}

/// What the panes read that the window computes during its own render.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PaneInputs {
    pub(crate) palette: UiPalette,
    pub(crate) listing_viewport_width: f32,
}

/// The window's listing and sidebar panes.
pub(crate) struct WindowPanes {
    pub(crate) listing: Entity<WindowPane>,
    pub(crate) sidebar: Entity<WindowPane>,
    /// The inputs of the window render in progress.
    inputs: Option<PaneInputs>,
    /// The inputs each pane last rendered with.
    listing_rendered_with: Option<PaneInputs>,
    sidebar_rendered_with: Option<PaneInputs>,
    /// Set by [`AccessibilityProbe`] when the last frame built an
    /// accessibility tree.
    pub(crate) accessibility_seen: Rc<Cell<bool>>,
    /// Whether the window render in progress embeds the panes uncached.
    uncached: bool,
}

impl WindowPanes {
    pub(crate) fn new(window: &Entity<DirectoryWindow>, cx: &mut App) -> Self {
        Self {
            listing: cx.new(|cx| WindowPane::new(window, PaneKind::Listing, cx)),
            sidebar: cx.new(|cx| WindowPane::new(window, PaneKind::Sidebar, cx)),
            inputs: None,
            listing_rendered_with: None,
            sidebar_rendered_with: None,
            accessibility_seen: Rc::new(Cell::new(false)),
            uncached: false,
        }
    }

    fn record_render(&mut self, kind: PaneKind) {
        match kind {
            PaneKind::Listing => self.listing_rendered_with = self.inputs,
            PaneKind::Sidebar => self.sidebar_rendered_with = self.inputs,
        }
    }

    /// Start a window render with `inputs`. The returned probe goes anywhere
    /// in the window's element tree outside the panes.
    ///
    /// GPUI builds its accessibility tree while elements prepaint, and a
    /// reused cached view does not prepaint, so a frame that reuses a pane
    /// would drop the pane's contents from the tree an assistive client
    /// (such as VoiceOver) sees. While the last frame built a tree, the panes
    /// therefore render uncached.
    pub(crate) fn begin_render(&mut self, inputs: PaneInputs) -> AccessibilityProbe {
        self.inputs = Some(inputs);
        self.uncached = self.accessibility_seen.replace(false);
        AccessibilityProbe {
            seen: self.accessibility_seen.clone(),
        }
    }

    /// Embed `kind`'s pane, laid out with `style`. The pane is cached unless
    /// an accessibility tree is being built or the pane last rendered with
    /// other inputs, as after a change of system appearance, which does not
    /// notify the window. (The first cached frame after an uncached one
    /// renders the pane once more, to record the layout and paint it will
    /// reuse.)
    pub(crate) fn element(&self, kind: PaneKind, style: StyleRefinement) -> AnyElement {
        let (pane, rendered_with) = match kind {
            PaneKind::Listing => (&self.listing, self.listing_rendered_with),
            PaneKind::Sidebar => (&self.sidebar, self.sidebar_rendered_with),
        };
        let view = AnyView::from(pane.clone());
        if !self.uncached && rendered_with.is_some() && rendered_with == self.inputs {
            view.cached(style).into_any_element()
        } else {
            // Render the pane this frame, inside a container laid out the
            // way the cached view would be.
            let mut container = div();
            *container.style() = style;
            container.child(view).into_any_element()
        }
    }
}

/// An invisible element that records whether GPUI is building an
/// accessibility tree: GPUI asks elements for their accessibility role only
/// while an assistive client is connected.
pub(crate) struct AccessibilityProbe {
    seen: Rc<Cell<bool>>,
}

impl IntoElement for AccessibilityProbe {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AccessibilityProbe {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        // GPUI only asks elements with an id.
        Some("pane-accessibility-probe".into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn a11y_role(&self) -> Option<Role> {
        self.seen.set(true);
        // No node: the probe is not part of the tree.
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            position: Position::Absolute,
            ..Style::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}
