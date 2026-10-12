//! The session navigator (REQ-UI-040), the window navigator (REQ-UI-041) and the side nav
//! (REQ-UI-042).

use crate::app::App;
use crate::layout::Layout;
use crate::render::Canvas;
use crate::theme::Theme;

/// An open navigator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavState {}

pub(crate) fn draw_sidenav(_app: &mut App, _c: &mut Canvas, _theme: &Theme, _l: &Layout) {}

pub(crate) fn draw_navigator(
    _app: &App,
    _nav: &NavState,
    _c: &mut Canvas,
    _theme: &Theme,
    _cursor: &mut Option<(usize, usize)>,
) {
}
