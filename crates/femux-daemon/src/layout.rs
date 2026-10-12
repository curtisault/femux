//! The tiled layout tree of a tab (REQ-SESS-002): binary splits with rational ratios over
//! panes. Floating panes are kept beside the tree by the tab, not in it.
//!
//! Geometry: a split gives its first child `ratio` of the space left after a one-cell
//! separator, so panes never overlap and the client can draw borders in the gaps.

use femux_proto::msg::{self as m, Direction, SplitDir};

pub type PaneId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    fn right(&self) -> u16 {
        self.x + self.w
    }
    fn bottom(&self) -> u16 {
        self.y + self.h
    }
}

impl From<Rect> for m::Rect {
    fn from(r: Rect) -> Self {
        m::Rect {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}

impl From<&m::Rect> for Rect {
    fn from(r: &m::Rect) -> Self {
        Rect {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}

/// A rational in [0, 1], kept reduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ratio {
    pub num: u32,
    pub den: u32,
}

impl Ratio {
    pub const HALF: Ratio = Ratio { num: 1, den: 2 };

    pub fn new(num: u32, den: u32) -> Option<Ratio> {
        if den == 0 || num > den {
            return None;
        }
        let g = gcd(num, den).max(1);
        Some(Ratio {
            num: num / g,
            den: den / g,
        })
    }

    fn complement(self) -> Ratio {
        Ratio {
            num: self.den - self.num,
            den: self.den,
        }
    }

    /// `self` of `total`, rounded to nearest.
    fn of(self, total: u16) -> u16 {
        ((total as u64 * self.num as u64 * 2 + self.den as u64) / (self.den as u64 * 2)) as u16
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Pane(PaneId),
    Split {
        dir: SplitDir,
        ratio: Ratio,
        a: Box<Node>,
        b: Box<Node>,
    },
}

/// Splits `len` cells into (first, second) around a one-cell separator.
fn divide(len: u16, ratio: Ratio) -> (u16, u16) {
    let avail = len.saturating_sub(1);
    if avail < 2 {
        return (avail.max(1), 1);
    }
    let a = ratio.of(avail).clamp(1, avail - 1);
    (a, avail - a)
}

impl Node {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane(p) => out.push(p.clone()),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    pub fn contains(&self, pane: &str) -> bool {
        match self {
            Node::Pane(p) => p == pane,
            Node::Split { a, b, .. } => a.contains(pane) || b.contains(pane),
        }
    }

    /// Each pane's rectangle within `area`.
    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.layout(area, &mut out);
        out
    }

    fn layout(&self, r: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Node::Pane(p) => out.push((p.clone(), r)),
            Node::Split {
                dir: SplitDir::H,
                ratio,
                a,
                b,
            } => {
                let (wa, wb) = divide(r.w, *ratio);
                a.layout(Rect { w: wa, ..r }, out);
                b.layout(
                    Rect {
                        x: r.x + wa + 1,
                        w: wb,
                        ..r
                    },
                    out,
                );
            }
            Node::Split {
                dir: SplitDir::V,
                ratio,
                a,
                b,
            } => {
                let (ha, hb) = divide(r.h, *ratio);
                a.layout(Rect { h: ha, ..r }, out);
                b.layout(
                    Rect {
                        y: r.y + ha + 1,
                        h: hb,
                        ..r
                    },
                    out,
                );
            }
        }
    }

    /// Replaces pane `target` with a split of `target` and `new`. `keep` is the share kept by
    /// `target`; `new_first` puts the new pane left/above instead of right/below.
    pub fn split(
        &mut self,
        target: &str,
        new: PaneId,
        dir: SplitDir,
        keep: Ratio,
        new_first: bool,
    ) -> bool {
        match self {
            Node::Pane(p) if p == target => {
                let old = Box::new(Node::Pane(p.clone()));
                let new = Box::new(Node::Pane(new));
                *self = if new_first {
                    Node::Split {
                        dir,
                        ratio: keep.complement(),
                        a: new,
                        b: old,
                    }
                } else {
                    Node::Split {
                        dir,
                        ratio: keep,
                        a: old,
                        b: new,
                    }
                };
                true
            }
            Node::Pane(_) => false,
            Node::Split { a, b, .. } => {
                a.split(target, new.clone(), dir, keep, new_first)
                    || b.split(target, new, dir, keep, new_first)
            }
        }
    }

    /// Removes `pane`; its sibling takes the parent's place. Returns the remaining tree, or
    /// `None` if `pane` was the only pane.
    pub fn remove(self, pane: &str) -> Option<Node> {
        match self {
            Node::Pane(p) if p == pane => None,
            Node::Pane(p) => Some(Node::Pane(p)),
            Node::Split { dir, ratio, a, b } => match (a.remove(pane), b.remove(pane)) {
                (Some(a), Some(b)) => Some(Node::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(n), None) | (None, Some(n)) => Some(n),
                (None, None) => None,
            },
        }
    }

    pub fn swap(&mut self, x: &str, y: &str) {
        match self {
            Node::Pane(p) if p == x => *p = y.to_owned(),
            Node::Pane(p) if p == y => *p = x.to_owned(),
            Node::Pane(_) => {}
            Node::Split { a, b, .. } => {
                a.swap(x, y);
                b.swap(x, y);
            }
        }
    }

    /// Flips the direction of the split directly containing `pane`.
    pub fn rotate(&mut self, pane: &str) -> bool {
        match self {
            Node::Pane(_) => false,
            Node::Split { dir, a, b, .. } => {
                if matches!(**a, Node::Pane(ref p) if p == pane)
                    || matches!(**b, Node::Pane(ref p) if p == pane)
                {
                    *dir = if *dir == SplitDir::H {
                        SplitDir::V
                    } else {
                        SplitDir::H
                    };
                    true
                } else {
                    a.rotate(pane) || b.rotate(pane)
                }
            }
        }
    }

    fn leaves(&self) -> u32 {
        match self {
            Node::Pane(_) => 1,
            Node::Split { a, b, .. } => a.leaves() + b.leaves(),
        }
    }

    /// Ratios proportional to pane counts, so same-direction chains share space equally.
    pub fn equalize(&mut self) {
        if let Node::Split { ratio, a, b, .. } = self {
            a.equalize();
            b.equalize();
            let (la, lb) = (a.leaves(), b.leaves());
            *ratio = Ratio::new(la, la + lb).unwrap();
        }
    }

    /// Moves the nearest separator in `dir`'s axis that bounds `pane` by `delta` cells
    /// (positive: right/down for l/j, left/up for h/k).
    pub fn resize(&mut self, pane: &str, dir: Direction, delta: i32, area: Rect) -> bool {
        let axis = match dir {
            Direction::H | Direction::L => SplitDir::H,
            Direction::J | Direction::K => SplitDir::V,
        };
        let shift = match dir {
            Direction::L | Direction::J => delta,
            Direction::H | Direction::K => -delta,
        };
        self.resize_in(pane, axis, shift, area).unwrap_or(false)
    }

    /// `None`: pane not in this subtree. `Some(done)`: pane found; `done` once a split moved.
    fn resize_in(&mut self, pane: &str, axis: SplitDir, shift: i32, r: Rect) -> Option<bool> {
        match self {
            Node::Pane(p) => (p == pane).then_some(false),
            Node::Split { dir, ratio, a, b } => {
                let len = if *dir == SplitDir::H { r.w } else { r.h };
                let (la, lb) = divide(len, *ratio);
                let (ra, rb) = match dir {
                    SplitDir::H => (
                        Rect { w: la, ..r },
                        Rect {
                            x: r.x + la + 1,
                            w: lb,
                            ..r
                        },
                    ),
                    SplitDir::V => (
                        Rect { h: la, ..r },
                        Rect {
                            y: r.y + la + 1,
                            h: lb,
                            ..r
                        },
                    ),
                };
                let found = match a.resize_in(pane, axis, shift, ra) {
                    Some(d) => Some(d),
                    None => b.resize_in(pane, axis, shift, rb),
                }?;
                if found || *dir != axis {
                    return Some(found);
                }
                let avail = len.saturating_sub(1) as i32;
                if avail < 2 {
                    return Some(true);
                }
                let new = (la as i32 + shift).clamp(1, avail - 1);
                *ratio = Ratio::new(new as u32, avail as u32).unwrap();
                Some(true)
            }
        }
    }

    pub fn to_proto(&self) -> m::LayoutNode {
        match self {
            Node::Pane(p) => m::LayoutNode::PaneNode(m::PaneNode { id: p.clone() }),
            Node::Split { dir, ratio, a, b } => m::LayoutNode::SplitNode(m::SplitNode {
                dir: *dir,
                ratio: m::Ratio {
                    num: ratio.num,
                    den: ratio.den,
                },
                children: vec![a.to_proto(), b.to_proto()],
            }),
        }
    }

    pub fn from_proto(n: &m::LayoutNode) -> Result<Node, String> {
        match n {
            m::LayoutNode::PaneNode(p) => Ok(Node::Pane(p.id.clone())),
            m::LayoutNode::SplitNode(s) => {
                let [a, b] = s.children.as_slice() else {
                    return Err("a split needs exactly two children".into());
                };
                let ratio = Ratio::new(s.ratio.num, s.ratio.den)
                    .ok_or("ratio must be in [0, 1] with den > 0")?;
                Ok(Node::Split {
                    dir: s.dir,
                    ratio,
                    a: Box::new(Node::from_proto(a)?),
                    b: Box::new(Node::from_proto(b)?),
                })
            }
        }
    }
}

/// Builds an equalized chain of `panes` split along `dir`.
fn chain(panes: &[PaneId], dir: SplitDir) -> Option<Node> {
    let (first, rest) = panes.split_first()?;
    let mut node = Node::Pane(first.clone());
    let mut last = first.clone();
    for p in rest {
        node.split(&last, p.clone(), dir, Ratio::HALF, false);
        last = p.clone();
    }
    node.equalize();
    Some(node)
}

/// `master` scheme (REQ-UI-010): the first pane takes the left half, the rest stack on the
/// right with equal heights.
pub fn master_rects(panes: &[PaneId], area: Rect) -> Vec<(PaneId, Rect)> {
    let Some((master, stack)) = panes.split_first() else {
        return Vec::new();
    };
    let Some(stack) = chain(stack, SplitDir::V) else {
        return vec![(master.clone(), area)];
    };
    let node = Node::Split {
        dir: SplitDir::H,
        ratio: Ratio::HALF,
        a: Box::new(Node::Pane(master.clone())),
        b: Box::new(stack),
    };
    node.rects(area)
}

/// How many panes the `scroll` scheme shows side by side.
pub const SCROLL_VISIBLE: usize = 2;

/// `scroll` scheme (REQ-UI-010): panes form a horizontal strip of equal widths; the strip is
/// positioned so the focused pane is visible, and panes off the strip get no rectangle.
pub fn scroll_rects(panes: &[PaneId], focus: &str, area: Rect) -> Vec<(PaneId, Rect)> {
    let n = panes.len();
    let k = n.min(SCROLL_VISIBLE);
    if k == 0 {
        return Vec::new();
    }
    let f = panes.iter().position(|p| p == focus).unwrap_or(0);
    let start = f.min(n - k);
    chain(&panes[start..start + k], SplitDir::H)
        .map(|c| c.rects(area))
        .unwrap_or_default()
}

/// The pane adjacent to `from` in `dir`, choosing the largest overlap on the other axis.
pub fn neighbor(rects: &[(PaneId, Rect)], from: &str, dir: Direction) -> Option<PaneId> {
    let (_, f) = rects.iter().find(|(p, _)| p == from)?;
    let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a1.min(b1).saturating_sub(a0.max(b0));
    rects
        .iter()
        .filter(|(p, _)| p != from)
        .filter_map(|(p, r)| {
            let (adjacent, ov) = match dir {
                Direction::H => (
                    r.right() + 1 == f.x,
                    overlap(f.y, f.bottom(), r.y, r.bottom()),
                ),
                Direction::L => (
                    f.right() + 1 == r.x,
                    overlap(f.y, f.bottom(), r.y, r.bottom()),
                ),
                Direction::K => (
                    r.bottom() + 1 == f.y,
                    overlap(f.x, f.right(), r.x, r.right()),
                ),
                Direction::J => (
                    f.bottom() + 1 == r.y,
                    overlap(f.x, f.right(), r.x, r.right()),
                ),
            };
            (adjacent && ov > 0).then_some((ov, p))
        })
        // Largest overlap wins; ties go to the first (top-most / left-most) pane.
        .fold(None, |best: Option<(u16, &PaneId)>, c| match best {
            Some(b) if b.0 >= c.0 => Some(b),
            _ => Some(c),
        })
        .map(|(_, p)| p.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: u16, h: u16) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    fn r(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    /// a | (b / c)
    fn three() -> Node {
        let mut t = Node::Pane("a".into());
        assert!(t.split("a", "b".into(), SplitDir::H, Ratio::HALF, false));
        assert!(t.split("b", "c".into(), SplitDir::V, Ratio::HALF, false));
        t
    }

    #[test]
    fn splits_leave_separators() {
        let t = three();
        assert_eq!(t.panes(), ["a", "b", "c"]);
        assert_eq!(
            t.rects(area(81, 25)),
            [
                ("a".into(), r(0, 0, 40, 25)),
                ("b".into(), r(41, 0, 40, 12)),
                ("c".into(), r(41, 13, 40, 12))
            ]
        );
    }

    #[test]
    fn ratios_are_rationals() {
        assert_eq!(Ratio::new(2, 4), Some(Ratio::HALF));
        assert_eq!(Ratio::new(3, 2), None);
        assert_eq!(Ratio::new(1, 0), None);
        let mut t = Node::Pane("a".into());
        t.split(
            "a",
            "b".into(),
            SplitDir::V,
            Ratio::new(2, 3).unwrap(),
            true,
        );
        // New pane first (above) gets the remaining third.
        assert_eq!(
            t.rects(area(10, 31)),
            [
                ("b".into(), r(0, 0, 10, 10)),
                ("a".into(), r(0, 11, 10, 20))
            ]
        );
    }

    #[test]
    fn remove_promotes_sibling() {
        let t = three().remove("b").unwrap();
        assert_eq!(
            t.rects(area(81, 25)),
            [
                ("a".into(), r(0, 0, 40, 25)),
                ("c".into(), r(41, 0, 40, 25))
            ]
        );
        assert_eq!(Node::Pane("a".into()).remove("a"), None);
    }

    #[test]
    fn neighbors_by_direction() {
        let t = three();
        let rects = t.rects(area(81, 25));
        assert_eq!(neighbor(&rects, "a", Direction::L).as_deref(), Some("b"));
        assert_eq!(neighbor(&rects, "c", Direction::K).as_deref(), Some("b"));
        assert_eq!(neighbor(&rects, "c", Direction::H).as_deref(), Some("a"));
        assert_eq!(neighbor(&rects, "a", Direction::H), None);
    }

    #[test]
    fn resize_moves_nearest_separator() {
        let mut t = three();
        assert!(t.resize("c", Direction::H, 5, area(81, 25)));
        assert_eq!(t.rects(area(81, 25))[0].1, r(0, 0, 35, 25));
        assert!(t.resize("b", Direction::J, 2, area(81, 25)));
        assert_eq!(t.rects(area(81, 25))[1].1.h, 14);
        // Clamped to keep both sides at least one cell.
        assert!(t.resize("a", Direction::L, 500, area(81, 25)));
        assert_eq!(t.rects(area(81, 25))[0].1.w, 79);
    }

    #[test]
    fn swap_rotate_equalize() {
        let mut t = three();
        t.swap("a", "c");
        assert_eq!(t.panes(), ["c", "b", "a"]);
        assert!(t.rotate("b"));
        assert_eq!(t.rects(area(81, 25))[1].1, r(41, 0, 20, 25));
        let mut chain = Node::Pane("a".into());
        chain.split("a", "b".into(), SplitDir::H, Ratio::HALF, false);
        chain.split("b", "c".into(), SplitDir::H, Ratio::HALF, false);
        chain.equalize();
        let widths: Vec<u16> = chain.rects(area(32, 5)).iter().map(|(_, r)| r.w).collect();
        assert_eq!(widths, [10, 10, 10]);
    }

    #[test]
    fn master_and_scroll_schemes_follow_pane_order() {
        let panes: Vec<PaneId> = ["a", "b", "c"].map(String::from).to_vec();
        assert_eq!(
            master_rects(&panes, area(81, 25)),
            [
                ("a".into(), r(0, 0, 40, 25)),
                ("b".into(), r(41, 0, 40, 12)),
                ("c".into(), r(41, 13, 40, 12))
            ]
        );
        assert_eq!(
            master_rects(&panes[..1], area(81, 25)),
            [("a".into(), r(0, 0, 81, 25))]
        );
        // Two panes fit the strip; focusing the third scrolls it into view.
        assert_eq!(
            scroll_rects(&panes, "a", area(81, 25)),
            [
                ("a".into(), r(0, 0, 40, 25)),
                ("b".into(), r(41, 0, 40, 25))
            ]
        );
        assert_eq!(
            scroll_rects(&panes, "c", area(81, 25)),
            [
                ("b".into(), r(0, 0, 40, 25)),
                ("c".into(), r(41, 0, 40, 25))
            ]
        );
        assert!(scroll_rects(&[], "a", area(81, 25)).is_empty());
    }

    #[test]
    fn proto_roundtrip_and_validation() {
        let t = three();
        assert_eq!(Node::from_proto(&t.to_proto()).unwrap(), t);
        let bad = m::LayoutNode::SplitNode(m::SplitNode {
            dir: SplitDir::H,
            ratio: m::Ratio { num: 1, den: 2 },
            children: vec![m::LayoutNode::PaneNode(m::PaneNode { id: "a".into() })],
        });
        assert!(Node::from_proto(&bad).is_err());
    }
}
