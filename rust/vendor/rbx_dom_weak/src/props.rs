//! An instance's properties, stored compactly.
//!
//! `Instance::properties` was a `UstrMap<Variant>`: a hash table per
//! instance, about 5.4 kB each on a real place (~45 properties of 72 bytes,
//! in tables sized to a power of two), 1 GB for 180 000 instances. Most of
//! those values are the same on every instance of a class, so a `PropMap`
//! holds only what differs from a `Template` its class shares
//! (`WeakDom::share_properties`), in two flat arrays. It behaves as the map
//! did: the same lookups, inserts, removals and iteration (in no promised
//! order, as before).

use std::sync::Arc;

use rbx_types::{CFrame, Variant, Vector3};
use ustr::Ustr;

fn addr(key: Ustr) -> usize {
    key.as_char_ptr() as usize
}

/// Values many instances of one class share: keys sorted by address.
#[derive(Debug)]
pub(crate) struct Template {
    keys: Vec<Ustr>,
    vals: Vec<Variant>,
}

impl Template {
    fn pos(&self, key: Ustr) -> Option<usize> {
        let a = addr(key);
        self.keys.binary_search_by(|k| addr(*k).cmp(&a)).ok()
    }

    fn get(&self, key: Ustr) -> Option<&Variant> {
        self.pos(key).map(|i| &self.vals[i])
    }
}

#[derive(Clone)]
enum Slot {
    Value(Variant),
    /// The template has this key and the instance does not.
    Removed,
}

/// The properties of one instance: see the module notes.
#[derive(Clone, Default)]
pub struct PropMap {
    keys: Vec<Ustr>,
    vals: Vec<Slot>,
    base: Option<Arc<Template>>,
}

impl PropMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty map with room for `capacity` properties.
    pub fn with_capacity(capacity: usize) -> Self {
        PropMap {
            keys: Vec::with_capacity(capacity),
            vals: Vec::with_capacity(capacity),
            base: None,
        }
    }

    #[inline]
    fn find(&self, key: Ustr) -> Option<usize> {
        self.keys.iter().position(|k| *k == key)
    }

    #[inline]
    fn base_get(&self, key: Ustr) -> Option<&Variant> {
        self.base.as_ref().and_then(|b| b.get(key))
    }

    /// The value of a property.
    #[inline]
    pub fn get(&self, key: &Ustr) -> Option<&Variant> {
        match self.find(*key) {
            Some(i) => match &self.vals[i] {
                Slot::Value(v) => Some(v),
                Slot::Removed => None,
            },
            None => self.base_get(*key),
        }
    }

    /// Whether the property is there.
    pub fn contains_key(&self, key: &Ustr) -> bool {
        self.get(key).is_some()
    }

    /// The value of a property, to change in place.
    pub fn get_mut(&mut self, key: &Ustr) -> Option<&mut Variant> {
        let i = match self.find(*key) {
            Some(i) => i,
            None => {
                let v = self.base_get(*key)?.clone();
                self.keys.push(*key);
                self.vals.push(Slot::Value(v));
                self.keys.len() - 1
            }
        };
        match &mut self.vals[i] {
            Slot::Value(v) => Some(v),
            Slot::Removed => None,
        }
    }

    /// Sets a property; the value it had, if any.
    pub fn insert(&mut self, key: Ustr, value: Variant) -> Option<Variant> {
        if let Some(i) = self.find(key) {
            return match std::mem::replace(&mut self.vals[i], Slot::Value(value)) {
                Slot::Value(old) => Some(old),
                Slot::Removed => None,
            };
        }
        if let Some(shared) = self.base_get(key) {
            // Set to what the class shares anyway: nothing to store.
            if same(shared, &value) {
                return Some(value);
            }
            let old = shared.clone();
            self.keys.push(key);
            self.vals.push(Slot::Value(value));
            return Some(old);
        }
        self.keys.push(key);
        self.vals.push(Slot::Value(value));
        None
    }

    /// Removes a property; the value it had, if any.
    pub fn remove(&mut self, key: &Ustr) -> Option<Variant> {
        let shared = self.base_get(*key).is_some();
        match self.find(*key) {
            Some(i) if shared => match std::mem::replace(&mut self.vals[i], Slot::Removed) {
                Slot::Value(old) => Some(old),
                Slot::Removed => None,
            },
            Some(i) => {
                self.keys.swap_remove(i);
                match self.vals.swap_remove(i) {
                    Slot::Value(old) => Some(old),
                    Slot::Removed => None,
                }
            }
            None if shared => {
                let old = self.base_get(*key).cloned();
                self.keys.push(*key);
                self.vals.push(Slot::Removed);
                old
            }
            None => None,
        }
    }

    /// How many properties there are.
    pub fn len(&self) -> usize {
        let own = self.vals.iter().filter(|s| matches!(s, Slot::Value(_))).count();
        let shared = self.base.as_ref().map_or(0, |b| b.keys.iter().filter(|k| self.find(**k).is_none()).count());
        own + shared
    }

    /// Whether there are no properties.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every property (in no particular order).
    pub fn iter(&self) -> Iter<'_> {
        Iter { map: self, own: 0, shared: 0 }
    }

    /// Every property name.
    pub fn keys(&self) -> impl Iterator<Item = &Ustr> + '_ {
        self.iter().map(|(k, _)| k)
    }

    /// Every property value.
    pub fn values(&self) -> impl Iterator<Item = &Variant> + '_ {
        self.iter().map(|(_, v)| v)
    }

    /// Every property, to change in place. (The map stops sharing its
    /// class's values: prefer `get_mut` / `insert` for single properties.)
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Ustr, &mut Variant)> + '_ {
        self.own_everything();
        self.keys.iter().zip(self.vals.iter_mut()).filter_map(|(k, s)| match s {
            Slot::Value(v) => Some((k, v)),
            Slot::Removed => None,
        })
    }

    /// Every value, to change in place (see `iter_mut`).
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Variant> + '_ {
        self.iter_mut().map(|(_, v)| v)
    }

    /// Removes every property.
    pub fn clear(&mut self) {
        *self = PropMap::default();
    }

    /// Keeps the properties `keep` says to.
    pub fn retain(&mut self, mut keep: impl FnMut(&Ustr, &mut Variant) -> bool) {
        self.own_everything();
        let mut i = 0;
        while i < self.keys.len() {
            let stay = match &mut self.vals[i] {
                Slot::Value(v) => keep(&self.keys[i], v),
                Slot::Removed => false,
            };
            if stay {
                i += 1;
            } else {
                self.keys.swap_remove(i);
                self.vals.swap_remove(i);
            }
        }
    }

    /// Copies the shared values in: the map holds everything itself.
    fn own_everything(&mut self) {
        let Some(base) = self.base.take() else { return };
        for (k, v) in base.keys.iter().zip(&base.vals) {
            if self.find(*k).is_none() {
                self.keys.push(*k);
                self.vals.push(Slot::Value(v.clone()));
            }
        }
        let mut i = 0;
        while i < self.keys.len() {
            if matches!(self.vals[i], Slot::Removed) {
                self.keys.swap_remove(i);
                self.vals.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Whether the map shares values with its class (`WeakDom::share_properties`).
    pub fn is_shared(&self) -> bool {
        self.base.is_some()
    }

    /// Bytes this map holds itself (not the shared values), roughly.
    pub fn own_bytes(&self) -> usize {
        self.keys.capacity() * std::mem::size_of::<Ustr>() + self.vals.capacity() * std::mem::size_of::<Slot>()
    }

    /// Holds only what differs from `template`.
    fn rebase(&mut self, template: &Arc<Template>) {
        debug_assert!(self.base.is_none());
        let keys = std::mem::take(&mut self.keys);
        let vals = std::mem::take(&mut self.vals);
        let mut out_keys = Vec::new();
        let mut out_vals = Vec::new();
        for tk in &template.keys {
            if !keys.contains(tk) {
                out_keys.push(*tk);
                out_vals.push(Slot::Removed);
            }
        }
        for (k, slot) in keys.into_iter().zip(vals) {
            let Slot::Value(v) = slot else { continue };
            if template.get(k).is_some_and(|t| same(t, &v)) {
                continue;
            }
            out_keys.push(k);
            out_vals.push(Slot::Value(v));
        }
        out_keys.shrink_to_fit();
        out_vals.shrink_to_fit();
        self.keys = out_keys;
        self.vals = out_vals;
        self.base = Some(template.clone());
    }
}

/// Finds what the maps of one class have in common and makes them share it.
/// `maps` is walked twice; only maps not sharing yet take part.
pub(crate) fn share<'a>(maps: &mut [&'a mut PropMap]) {
    const CANDIDATES: usize = 4;
    struct Seen {
        key: Ustr,
        present: u32,
        values: Vec<(Variant, u32)>,
    }
    let n = maps.len() as u32;
    let mut index: ustr::UstrMap<usize> = Default::default();
    let mut seen: Vec<Seen> = Vec::new();
    for map in maps.iter() {
        for (k, v) in map.iter() {
            let at = *index.entry(*k).or_insert_with(|| {
                seen.push(Seen { key: *k, present: 0, values: Vec::new() });
                seen.len() - 1
            });
            let s = &mut seen[at];
            s.present += 1;
            if let Some(c) = s.values.iter_mut().find(|(c, _)| same(c, v)) {
                c.1 += 1;
            } else if s.values.len() < CANDIDATES && same(v, v) {
                s.values.push((v.clone(), 1));
            }
        }
    }
    // A shared value costs every instance without it one slot (another
    // value, or the note that it has none): worth it when more have it
    // than lack the property.
    let mut pairs: Vec<(Ustr, Variant)> = Vec::new();
    for s in seen {
        let absent = n - s.present;
        if let Some((value, count)) = s.values.into_iter().max_by_key(|(_, c)| *c) {
            if count >= 2 && count > absent {
                pairs.push((s.key, value));
            }
        }
    }
    if pairs.len() < 4 {
        return;
    }
    pairs.sort_by_key(|(k, _)| addr(*k));
    let (keys, vals) = pairs.into_iter().unzip();
    let template = Arc::new(Template { keys, vals });
    for map in maps.iter_mut() {
        map.rebase(&template);
    }
}

/// Exactly the same value, bit for bit where it holds floats (0.0 and -0.0
/// differ, as do NaNs: a shared value must come back as it was written).
/// Types not listed never count as the same and are simply not shared.
fn same(a: &Variant, b: &Variant) -> bool {
    fn f(a: f32, b: f32) -> bool {
        a.to_bits() == b.to_bits()
    }
    fn v3(a: &Vector3, b: &Vector3) -> bool {
        f(a.x, b.x) && f(a.y, b.y) && f(a.z, b.z)
    }
    fn cf(a: &CFrame, b: &CFrame) -> bool {
        v3(&a.position, &b.position)
            && v3(&a.orientation.x, &b.orientation.x)
            && v3(&a.orientation.y, &b.orientation.y)
            && v3(&a.orientation.z, &b.orientation.z)
    }
    use Variant as V;
    match (a, b) {
        (V::Bool(a), V::Bool(b)) => a == b,
        (V::Int32(a), V::Int32(b)) => a == b,
        (V::Int64(a), V::Int64(b)) => a == b,
        (V::Float32(a), V::Float32(b)) => f(*a, *b),
        (V::Float64(a), V::Float64(b)) => a.to_bits() == b.to_bits(),
        (V::String(a), V::String(b)) => a == b,
        (V::BinaryString(a), V::BinaryString(b)) => a == b,
        (V::Enum(a), V::Enum(b)) => a == b,
        (V::BrickColor(a), V::BrickColor(b)) => a == b,
        (V::Color3uint8(a), V::Color3uint8(b)) => a == b,
        (V::ContentId(a), V::ContentId(b)) => a == b,
        (V::Content(a), V::Content(b)) => a == b,
        (V::Axes(a), V::Axes(b)) => a == b,
        (V::Faces(a), V::Faces(b)) => a == b,
        (V::Font(a), V::Font(b)) => a == b,
        (V::Tags(a), V::Tags(b)) => a == b,
        (V::SharedString(a), V::SharedString(b)) => a == b,
        (V::SecurityCapabilities(a), V::SecurityCapabilities(b)) => a == b,
        (V::Vector2int16(a), V::Vector2int16(b)) => a == b,
        (V::Vector3int16(a), V::Vector3int16(b)) => a == b,
        // (a Ref to an instance is the instance's own business)
        (V::Ref(a), V::Ref(b)) => a.is_none() && b.is_none(),
        (V::Attributes(a), V::Attributes(b)) => a.is_empty() && b.is_empty(),
        (V::Vector2(a), V::Vector2(b)) => f(a.x, b.x) && f(a.y, b.y),
        (V::Vector3(a), V::Vector3(b)) => v3(a, b),
        (V::Color3(a), V::Color3(b)) => f(a.r, b.r) && f(a.g, b.g) && f(a.b, b.b),
        (V::CFrame(a), V::CFrame(b)) => cf(a, b),
        (V::OptionalCFrame(a), V::OptionalCFrame(b)) => match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => cf(a, b),
            _ => false,
        },
        (V::UDim(a), V::UDim(b)) => f(a.scale, b.scale) && a.offset == b.offset,
        (V::UDim2(a), V::UDim2(b)) => {
            f(a.x.scale, b.x.scale) && a.x.offset == b.x.offset && f(a.y.scale, b.y.scale) && a.y.offset == b.y.offset
        }
        (V::NumberRange(a), V::NumberRange(b)) => f(a.min, b.min) && f(a.max, b.max),
        (V::Rect(a), V::Rect(b)) => {
            f(a.min.x, b.min.x) && f(a.min.y, b.min.y) && f(a.max.x, b.max.x) && f(a.max.y, b.max.y)
        }
        (V::PhysicalProperties(a), V::PhysicalProperties(b)) => {
            matches!((a, b), (rbx_types::PhysicalProperties::Default, rbx_types::PhysicalProperties::Default))
        }
        _ => false,
    }
}

/// `PropMap::iter`.
pub struct Iter<'a> {
    map: &'a PropMap,
    own: usize,
    shared: usize,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Ustr, &'a Variant);

    fn next(&mut self) -> Option<Self::Item> {
        let map = self.map;
        while self.own < map.keys.len() {
            let i = self.own;
            self.own += 1;
            if let Slot::Value(v) = &map.vals[i] {
                return Some((&map.keys[i], v));
            }
        }
        let base = map.base.as_ref()?;
        while self.shared < base.keys.len() {
            let i = self.shared;
            self.shared += 1;
            if map.find(base.keys[i]).is_none() {
                return Some((&base.keys[i], &base.vals[i]));
            }
        }
        None
    }
}

impl<'a> IntoIterator for &'a PropMap {
    type Item = (&'a Ustr, &'a Variant);
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl IntoIterator for PropMap {
    type Item = (Ustr, Variant);
    type IntoIter = std::vec::IntoIter<(Ustr, Variant)>;

    fn into_iter(mut self) -> Self::IntoIter {
        self.own_everything();
        self.keys
            .into_iter()
            .zip(self.vals)
            .filter_map(|(k, s)| match s {
                Slot::Value(v) => Some((k, v)),
                Slot::Removed => None,
            })
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl std::iter::FromIterator<(Ustr, Variant)> for PropMap {
    fn from_iter<I: IntoIterator<Item = (Ustr, Variant)>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let mut map = PropMap::with_capacity(iter.size_hint().0);
        map.extend(iter);
        map
    }
}

impl Extend<(Ustr, Variant)> for PropMap {
    fn extend<I: IntoIterator<Item = (Ustr, Variant)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

impl std::ops::Index<&Ustr> for PropMap {
    type Output = Variant;

    /// The property's value; panics when there is none (as the hash map did).
    fn index(&self, key: &Ustr) -> &Variant {
        self.get(key).expect("no such property")
    }
}

impl PartialEq for PropMap {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl std::fmt::Debug for PropMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_types::Ref;
    use ustr::ustr;

    fn part(size: f32, name: &str) -> PropMap {
        let mut m = PropMap::new();
        m.insert(ustr("Anchored"), Variant::Bool(true));
        m.insert(ustr("Transparency"), Variant::Float32(0.0));
        m.insert(ustr("Material"), Variant::Enum(rbx_types::Enum::from_u32(256)));
        m.insert(ustr("CanCollide"), Variant::Bool(true));
        m.insert(ustr("Reflectance"), Variant::Float32(0.0));
        m.insert(ustr("Size"), Variant::Vector3(Vector3::new(size, 1.0, 2.0)));
        m.insert(ustr("Tag"), Variant::String(name.to_string()));
        m
    }

    fn as_sorted(m: &PropMap) -> Vec<(String, String)> {
        let mut v: Vec<_> = m.iter().map(|(k, v)| (k.to_string(), format!("{v:?}"))).collect();
        v.sort();
        v
    }

    fn shared_set() -> Vec<PropMap> {
        let mut maps: Vec<PropMap> = (0..10).map(|i| part(i as f32, &format!("p{i}"))).collect();
        // One without a common property, one with another value for it.
        maps[3].remove(&ustr("CanCollide"));
        maps[4].insert(ustr("Anchored"), Variant::Bool(false));
        maps[5].insert(ustr("Extra"), Variant::Int32(5));
        let before: Vec<_> = maps.iter().map(as_sorted).collect();
        {
            let mut refs: Vec<&mut PropMap> = maps.iter_mut().collect();
            share(&mut refs);
        }
        for (m, b) in maps.iter().zip(&before) {
            assert!(m.is_shared());
            assert_eq!(&as_sorted(m), b);
            assert_eq!(m.len(), b.len());
        }
        maps
    }

    #[test]
    fn sharing_keeps_every_map_as_it_was() {
        let maps = shared_set();
        // The common values are no longer held per map.
        assert!(maps[0].keys.len() <= 2, "{:?}", maps[0].keys);
        assert_eq!(maps[3].get(&ustr("CanCollide")), None);
        assert!(!maps[3].contains_key(&ustr("CanCollide")));
        assert_eq!(maps[4].get(&ustr("Anchored")), Some(&Variant::Bool(false)));
        assert_eq!(maps[5].get(&ustr("Extra")), Some(&Variant::Int32(5)));
        assert_eq!(maps[0].get(&ustr("Extra")), None);
    }

    #[test]
    fn shared_maps_change_like_plain_ones() {
        let mut maps = shared_set();
        let mut plain = part(0.0, "p0");
        let m = &mut maps[0];
        assert_eq!(*m, plain);
        // insert over a shared value, over an own value, and a new key
        for (k, v) in [
            ("Anchored", Variant::Bool(false)),
            ("Size", Variant::Vector3(Vector3::new(9.0, 9.0, 9.0))),
            ("New", Variant::Int32(1)),
            ("Anchored", Variant::Bool(true)),
            ("Transparency", Variant::Float32(0.0)),
        ] {
            assert_eq!(m.insert(ustr(k), v.clone()), plain.insert(ustr(k), v), "{k}");
            assert_eq!(*m, plain);
        }
        // remove a shared value, an own value, a missing key; twice
        for k in ["CanCollide", "Size", "Nope", "CanCollide", "New"] {
            assert_eq!(m.remove(&ustr(k)), plain.remove(&ustr(k)), "{k}");
            assert_eq!(*m, plain);
            assert_eq!(m.len(), plain.len());
        }
        // put a removed shared key back
        assert_eq!(m.insert(ustr("CanCollide"), Variant::Bool(false)), None);
        plain.insert(ustr("CanCollide"), Variant::Bool(false));
        assert_eq!(*m, plain);
        // get_mut on a shared value changes this map only
        *m.get_mut(&ustr("Reflectance")).unwrap() = Variant::Float32(0.5);
        assert_eq!(m.get(&ustr("Reflectance")), Some(&Variant::Float32(0.5)));
        assert_eq!(maps[1].get(&ustr("Reflectance")), Some(&Variant::Float32(0.0)));
        assert!(maps[0].get_mut(&ustr("Nope")).is_none());
        // values_mut / retain / into_iter see everything once
        let mut m = maps[2].clone();
        let n = m.len();
        assert_eq!(m.values_mut().count(), n);
        assert_eq!(as_sorted(&m), as_sorted(&maps[2]));
        m.retain(|k, _| k.as_str() != "Material");
        assert_eq!(m.len(), n - 1);
        assert!(!m.contains_key(&ustr("Material")));
        let all: PropMap = maps[3].clone().into_iter().collect();
        assert_eq!(all, maps[3]);
        assert!(!all.contains_key(&ustr("CanCollide")));
        assert_eq!(maps[3].keys().count(), maps[3].len());
    }

    #[test]
    fn floats_share_only_bit_for_bit_and_refs_never() {
        assert!(same(&Variant::Float32(0.0), &Variant::Float32(0.0)));
        assert!(!same(&Variant::Float32(0.0), &Variant::Float32(-0.0)));
        assert!(same(&Variant::Float32(f32::NAN), &Variant::Float32(f32::NAN)));
        assert!(same(&Variant::Ref(Ref::none()), &Variant::Ref(Ref::none())));
        let r = Ref::new();
        assert!(!same(&Variant::Ref(r), &Variant::Ref(r)));
        // too few common values: left alone
        let mut maps: Vec<PropMap> = (0..10)
            .map(|i| {
                let mut m = PropMap::new();
                m.insert(ustr("A"), Variant::Int32(i));
                m.insert(ustr("B"), Variant::Bool(true));
                m
            })
            .collect();
        let mut refs: Vec<&mut PropMap> = maps.iter_mut().collect();
        share(&mut refs);
        assert!(maps.iter().all(|m| !m.is_shared()));
    }
    /// Random edits on a population of maps and on plain hash maps side by
    /// side, sharing at random moments: they must always agree.
    #[test]
    fn random_edits_match_a_plain_hash_map() {
        use std::collections::HashMap;
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0
            }
            fn below(&mut self, n: usize) -> usize {
                (self.next() % n as u64) as usize
            }
        }
        fn show(v: Option<&Variant>) -> String {
            format!("{v:?}")
        }
        fn sorted<'a>(it: impl Iterator<Item = (&'a Ustr, &'a Variant)>) -> Vec<(String, String)> {
            let mut v: Vec<_> = it.map(|(k, v)| (k.to_string(), format!("{v:?}"))).collect();
            v.sort();
            v
        }
        let keys: Vec<Ustr> = (0..14).map(|i| ustr(&format!("FuzzKey{i}"))).collect();
        let r = Ref::new();
        let values = [
            Variant::Bool(true),
            Variant::Bool(false),
            Variant::Float32(0.0),
            Variant::Float32(-0.0),
            Variant::Float32(f32::NAN),
            Variant::Float32(1.5),
            Variant::Int32(7),
            Variant::String("a".into()),
            Variant::String("b".into()),
            Variant::Vector3(Vector3::new(1.0, 2.0, 3.0)),
            Variant::Vector3(Vector3::new(1.0, -0.0, 3.0)),
            Variant::Ref(Ref::none()),
            Variant::Ref(r),
            Variant::Float64(2.0),
        ];
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let n = 12;
            let mut maps: Vec<PropMap> = vec![PropMap::new(); n];
            let mut plain: Vec<HashMap<Ustr, Variant>> = vec![HashMap::new(); n];
            // Start alike, so sharing has something to find.
            for i in 0..n {
                for (j, k) in keys.iter().enumerate().take(10) {
                    let v = values[if rng.below(5) == 0 { rng.below(values.len()) } else { j % values.len() }].clone();
                    maps[i].insert(*k, v.clone());
                    plain[i].insert(*k, v);
                }
            }
            for step in 0..600 {
                let i = rng.below(n);
                let k = keys[rng.below(keys.len())];
                let v = values[rng.below(values.len())].clone();
                let what = format!("seed {seed} step {step} map {i} key {k}");
                match rng.below(12) {
                    0 | 1 | 2 => {
                        let (a, b) = (maps[i].insert(k, v.clone()), plain[i].insert(k, v));
                        assert_eq!(show(a.as_ref()), show(b.as_ref()), "insert, {what}");
                    }
                    3 | 4 => {
                        let (a, b) = (maps[i].remove(&k), plain[i].remove(&k));
                        assert_eq!(show(a.as_ref()), show(b.as_ref()), "remove, {what}");
                    }
                    5 => {
                        let (a, b) = (maps[i].get_mut(&k), plain[i].get_mut(&k));
                        assert_eq!(a.is_some(), b.is_some(), "get_mut, {what}");
                        if let (Some(a), Some(b)) = (a, b) {
                            *a = v.clone();
                            *b = v;
                        }
                    }
                    6 => {
                        let j = rng.below(n);
                        maps[j] = maps[i].clone();
                        plain[j] = plain[i].clone();
                    }
                    7 => {
                        let mut refs: Vec<&mut PropMap> = maps.iter_mut().filter(|m| !m.is_shared()).collect();
                        if refs.len() >= 3 {
                            share(&mut refs);
                        }
                    }
                    8 => match rng.below(4) {
                        0 => {
                            for v in maps[i].values_mut() {
                                if let Variant::Bool(b) = v {
                                    *b = !*b;
                                }
                            }
                            for v in plain[i].values_mut() {
                                if let Variant::Bool(b) = v {
                                    *b = !*b;
                                }
                            }
                        }
                        1 => {
                            maps[i].retain(|x, _| *x != k);
                            plain[i].retain(|x, _| *x != k);
                        }
                        2 => maps[i] = maps[i].clone().into_iter().collect(),
                        _ => {
                            // (== as the hash maps have it: a NaN equals nothing)
                            let j = rng.below(n);
                            assert_eq!(maps[i] == maps[j], plain[i] == plain[j], "eq with {j}, {what}");
                        }
                    },
                    _ => {
                        assert_eq!(show(maps[i].get(&k)), show(plain[i].get(&k)), "get, {what}");
                        assert_eq!(maps[i].contains_key(&k), plain[i].contains_key(&k), "contains, {what}");
                    }
                }
                assert_eq!(maps[i].len(), plain[i].len(), "len, {what}");
                assert_eq!(sorted(maps[i].iter()), sorted(plain[i].iter()), "contents, {what}");
                assert_eq!(maps[i].keys().count(), plain[i].len(), "keys, {what}");
            }
            for i in 0..n {
                assert_eq!(sorted(maps[i].iter()), sorted(plain[i].iter()), "seed {seed} end, map {i}");
            }
        }
    }
}
