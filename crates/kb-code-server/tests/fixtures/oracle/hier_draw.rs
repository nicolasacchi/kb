// Hierarchy oracle: trait + two impls + cross-file-style calls in one module.
// Trait-object call MUST stay class=candidate.

pub trait Drawable {
    fn draw(&self);
}

pub struct Circle;
pub struct Square;

impl Drawable for Circle {
    fn draw(&self) {
        paint_circle();
    }
}

impl Drawable for Square {
    fn draw(&self) {
        paint_square();
    }
}

pub fn paint_circle() {}
pub fn paint_square() {}

pub fn use_concrete(c: &Circle) {
    c.draw();
    paint_circle();
}

pub fn use_trait_object(d: &dyn Drawable) {
    d.draw();
}

pub fn decoy_same_name() {
    // Unrelated same-name helper lives in hier_decoy.rs
}
