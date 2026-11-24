//! rbx_dom_weak is a common representation of the Roblox DOM for Rust. It's
//! designed to play nicely with the borrow checker and allows accessing
//! instances by ID in constant time.
//!
//! Constructing a new tree of instances is accomplished by first creating an
//! [`InstanceBuilder`] object that describes a tree of instances and then
//! wrapping it with an [`WeakDom`]:
//!
//! ```
//! use rbx_dom_weak::{InstanceBuilder, WeakDom};
//!
//! let dm = InstanceBuilder::new("DataModel");
//!
//! let mut dom = WeakDom::new(dm);
//!
//! println!("ID of DOM root is {:?}", dom.root_ref());
//! ```
//!
//! Once we have a tree, we can use [`WeakDom::insert`] and
//! [`WeakDom::get_by_ref`] to add instances to the tree and retrieve them.
//!
//! ```
//! use rbx_dom_weak::{InstanceBuilder, WeakDom};
//!
//! let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
//!
//! // We can define properties using any type that can be converted to an
//! // rbx_dom_weak::types::Variant.
//! let http_service = InstanceBuilder::new("HttpService")
//!     .with_property("HttpEnabled", true);
//!
//! let http_service_id = dom.insert(dom.root_ref(), http_service);
//!
//! println!("HttpService has ID {:?}", http_service_id);
//! ```
//!
//! To change properties on an instance that's already present in the tree, use
//! [`WeakDom::get_by_ref_mut`]. Note that it isn't possible to add or remove
//! children through this method, use [`WeakDom::insert`] and
//! [`WeakDom::destroy`] instead.

#![deny(missing_docs)]

mod dom;
mod instance;
mod props;
mod viewer;

pub use rbx_types as types;

/// A ref's value as a number (0 for none), for ordering: under
/// `RUSTUDIO_DETERMINISTIC=1` ref order is creation order.
pub fn ref_order(r: types::Ref) -> u128 {
    // (Ref is a one-field struct around Option<NonZeroU128>: 0 = none.)
    const _: () = assert!(std::mem::size_of::<types::Ref>() == 16);
    unsafe { std::mem::transmute::<types::Ref, u128>(r) }
}

/// A new referent for an instance. Random (`Ref::new`), except under
/// `RUSTUDIO_DETERMINISTIC=1` (test runs): then a process-wide counter, so
/// refs follow creation order and are the same on every run of the same place.
pub fn deterministic() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("RUSTUDIO_DETERMINISTIC").is_ok_and(|v| v == "1"))
}

thread_local! {
    static SCRATCH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Runs `f` with refs that do not come from the counter (random, as without
/// `RUSTUDIO_DETERMINISTIC`): for throwaway documents, so how many of them a
/// frame makes does not shift the refs of the instances that follow.
pub fn with_scratch_refs<T>(f: impl FnOnce() -> T) -> T {
    SCRATCH.with(|c| c.set(c.get() + 1));
    let out = f();
    SCRATCH.with(|c| c.set(c.get() - 1));
    out
}

/// See `deterministic`.
pub fn new_ref() -> types::Ref {
    if SCRATCH.with(|c| c.get()) > 0 {
        return types::Ref::new();
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    if deterministic() {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        types::Ref::some((1u128 << 100) | n as u128)
    } else {
        types::Ref::new()
    }
}

pub use ahash::AHashMap;
pub use ustr::{ustr, Ustr, UstrMap, UstrSet};

pub use crate::{
    dom::WeakDom,
    instance::{Instance, InstanceBuilder},
    props::PropMap,
    viewer::{DomViewer, ViewedInstance},
};

/// Helper trait that provides convenience methods for `AHashMap` and `UstrMap`.
pub trait HashMapExt {
    /// Constructs an empty map.
    fn new() -> Self;

    /// Constructs an empty map with at least the specified capacity.
    fn with_capacity(capacity: usize) -> Self;
}

impl<V> HashMapExt for UstrMap<V> {
    /// Creates an empty `UstrMap` using the default value for its hasher.
    fn new() -> Self {
        UstrMap::default()
    }

    /// Creates an empty `UstrMap` with at least the specified capacity using
    /// the default value for its hasher.
    fn with_capacity(capacity: usize) -> Self {
        UstrMap::with_capacity_and_hasher(capacity, Default::default())
    }
}

impl<K, V> HashMapExt for AHashMap<K, V> {
    /// Creates an empty `AHashMap` using the default value for its hasher.
    fn new() -> Self {
        AHashMap::default()
    }

    /// Creates an empty `AHashMap` with at least the specified capacity using
    /// the default value for its hasher.
    fn with_capacity(capacity: usize) -> Self {
        AHashMap::with_capacity_and_hasher(capacity, Default::default())
    }
}
