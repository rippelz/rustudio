use std::any::{Any, TypeId};
use std::cell::{BorrowError, BorrowMutError, Cell, Ref, RefCell, RefMut, UnsafeCell};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::result::Result as StdResult;

use rustc_hash::FxHashMap;

use super::MaybeSend;
use crate::state::LuaGuard;

#[cfg(not(feature = "send"))]
type Container = UnsafeCell<FxHashMap<TypeId, RefCell<Box<dyn Any>>>>;

#[cfg(feature = "send")]
type Container = UnsafeCell<FxHashMap<TypeId, RefCell<Box<dyn Any + Send>>>>;

#[cfg(not(feature = "send"))]
type Slot = RefCell<Box<dyn Any>>;
#[cfg(feature = "send")]
type Slot = RefCell<Box<dyn Any + Send>>;

/// rustudio: entries recently looked up, in front of the map (a runtime
/// looks the same few types up thousands of times a frame; the map probe
/// and the `Any` downcast were ~140 instructions an access). Each holds a
/// type and its slot in the map, valid until the map changes (`insert` /
/// `remove` clear them; no entry moves otherwise).
const RECENT: usize = 8;

/// A container for arbitrary data associated with the Lua state.
#[derive(Debug, Default)]
pub struct AppData {
    container: Container,
    borrow: Cell<usize>,
    recent: [Cell<Option<(TypeId, *const Slot)>>; RECENT],
    next: Cell<usize>,
}

impl AppData {
    #[track_caller]
    pub(crate) fn insert<T: MaybeSend + 'static>(&self, data: T) -> Option<T> {
        match self.try_insert(data) {
            Ok(data) => data,
            Err(_) => panic!("cannot mutably borrow app data container"),
        }
    }

    pub(crate) fn try_insert<T: MaybeSend + 'static>(&self, data: T) -> StdResult<Option<T>, T> {
        if self.borrow.get() != 0 {
            return Err(data);
        }
        self.forget_recent();
        // SAFETY: we checked that there are no other references to the container
        Ok(unsafe { &mut *self.container.get() }
            .insert(TypeId::of::<T>(), RefCell::new(Box::new(data)))
            .and_then(|data| data.into_inner().downcast::<T>().ok().map(|data| *data)))
    }

    #[inline]
    #[track_caller]
    pub(crate) fn borrow<T: 'static>(&self, guard: Option<LuaGuard>) -> Option<AppDataRef<'_, T>> {
        match self.try_borrow(guard) {
            Ok(data) => data,
            Err(err) => panic!("already mutably borrowed: {err:?}"),
        }
    }

    fn forget_recent(&self) {
        for c in &self.recent {
            c.set(None);
        }
    }

    /// The map's slot of `T` (`recent` first).
    #[inline]
    fn slot<T: 'static>(&self) -> Option<&Slot> {
        let id = TypeId::of::<T>();
        for c in &self.recent {
            match c.get() {
                // SAFETY: a recent slot is in the map, which hasn't changed
                // since (insert / remove clear `recent`).
                Some((t, p)) if t == id => return Some(unsafe { &*p }),
                Some(_) => {}
                None => break,
            }
        }
        let slot = unsafe { &*self.container.get() }.get(&id)?;
        let i = self.next.get();
        self.recent[i % RECENT].set(Some((id, slot as *const Slot)));
        self.next.set(i.wrapping_add(1));
        Some(slot)
    }

    pub(crate) fn try_borrow<T: 'static>(
        &self,
        guard: Option<LuaGuard>,
    ) -> Result<Option<AppDataRef<'_, T>>, BorrowError> {
        let data = self
            .slot::<T>()
            .map(|c| c.try_borrow())
            .transpose()?
            .and_then(|data| Ref::filter_map(data, |data| data.downcast_ref()).ok());
        match data {
            Some(data) => {
                self.borrow.set(self.borrow.get() + 1);
                Ok(Some(AppDataRef {
                    data,
                    borrow: &self.borrow,
                    _guard: guard,
                }))
            }
            None => Ok(None),
        }
    }

    #[inline]
    #[track_caller]
    pub(crate) fn borrow_mut<T: 'static>(&self, guard: Option<LuaGuard>) -> Option<AppDataRefMut<'_, T>> {
        match self.try_borrow_mut(guard) {
            Ok(data) => data,
            Err(err) => panic!("already borrowed: {err:?}"),
        }
    }

    pub(crate) fn try_borrow_mut<T: 'static>(
        &self,
        guard: Option<LuaGuard>,
    ) -> Result<Option<AppDataRefMut<'_, T>>, BorrowMutError> {
        let data = self
            .slot::<T>()
            .map(|c| c.try_borrow_mut())
            .transpose()?
            .and_then(|data| RefMut::filter_map(data, |data| data.downcast_mut()).ok());
        match data {
            Some(data) => {
                self.borrow.set(self.borrow.get() + 1);
                Ok(Some(AppDataRefMut {
                    data,
                    borrow: &self.borrow,
                    _guard: guard,
                }))
            }
            None => Ok(None),
        }
    }

    #[track_caller]
    pub(crate) fn remove<T: 'static>(&self) -> Option<T> {
        if self.borrow.get() != 0 {
            panic!("cannot mutably borrow app data container");
        }
        self.forget_recent();
        // SAFETY: we checked that there are no other references to the container
        unsafe { &mut *self.container.get() }
            .remove(&TypeId::of::<T>())?
            .into_inner()
            .downcast::<T>()
            .ok()
            .map(|data| *data)
    }
}

/// A wrapper type for an immutably borrowed value from an app data container.
///
/// This type is similar to [`Ref`].
pub struct AppDataRef<'a, T: ?Sized + 'a> {
    data: Ref<'a, T>,
    borrow: &'a Cell<usize>,
    _guard: Option<LuaGuard>,
}

impl<T: ?Sized> Drop for AppDataRef<'_, T> {
    fn drop(&mut self) {
        self.borrow.set(self.borrow.get() - 1);
    }
}

impl<T: ?Sized> Deref for AppDataRef<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for AppDataRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for AppDataRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

/// A wrapper type for a mutably borrowed value from an app data container.
///
/// This type is similar to [`RefMut`].
pub struct AppDataRefMut<'a, T: ?Sized + 'a> {
    data: RefMut<'a, T>,
    borrow: &'a Cell<usize>,
    _guard: Option<LuaGuard>,
}

impl<T: ?Sized> Drop for AppDataRefMut<'_, T> {
    fn drop(&mut self) {
        self.borrow.set(self.borrow.get() - 1);
    }
}

impl<T: ?Sized> Deref for AppDataRefMut<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T: ?Sized> DerefMut for AppDataRefMut<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for AppDataRefMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for AppDataRefMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

#[cfg(test)]
mod assertions {
    use super::*;

    #[cfg(not(feature = "send"))]
    static_assertions::assert_not_impl_any!(AppData: Send);
    #[cfg(feature = "send")]
    static_assertions::assert_impl_all!(AppData: Send);

    // Must be !Send
    static_assertions::assert_not_impl_any!(AppDataRef<()>: Send);
    static_assertions::assert_not_impl_any!(AppDataRefMut<()>: Send);
}
