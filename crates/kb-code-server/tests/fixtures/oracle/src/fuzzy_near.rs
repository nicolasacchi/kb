//! Calls orphan_only_here WITHOUT importing fuzzy_far — filter empties,
//! fuzzy fallback should yield class=candidate for the name match.

pub fn call_orphan() {
    orphan_only_here();
}
