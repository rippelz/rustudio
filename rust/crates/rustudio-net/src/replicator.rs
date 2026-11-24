//! The CloudEdit replicator: the item stream carried over the transport.
//!
//! The wire layout here is decoded from a real, decrypted Team Create join
//!; the research decoder
//! it was derived from is a capture script.
//!
//! Layers, outermost first:
//! - [`split_stream`]: a reassembled `rbx-rtcio` QUIC stream is
//!   `06 01 <channel> <u32 BE seq | "ctrl">` then messages framed as
//!   `<u32 BE 0x80000000 | len> <bytes>`.
//! - [`Decoder::decode_frame`]: one message = a Roblox packet id byte plus a
//!   body. `0x97` carries the schema (class / property / event tables) that
//!   every later item indexes into; `0x83` (ID_DATA) carries replicator items:
//!   initial-stream chunks, instance batches, new instances, property changes,
//!   events.

use rbx_dom_weak::types::Ref;
use std::collections::HashMap;

/// Roblox top-level packet ids (the first byte of a message).
pub mod packet_id {
    /// The top-level service table [verified: capture].
    pub const SET_GLOBALS: u8 = 0x81;
    /// Replicator items [verified: capture].
    pub const DATA: u8 = 0x83;
    /// Top-level marker after the schema, before the initial place data.
    pub const MARKER: u8 = 0x84;
    /// Streamed terrain chunks.
    pub const TERRAIN: u8 = 0x8d;
    /// Flag/protocol sync: u8, u16 LE count, (u16-LE-len key, u16-LE-len value)* [verified: capture].
    pub const PROTOCOL_SYNC: u8 = 0x93;
    /// The replication schema [verified: capture].
    pub const NEW_SCHEMA: u8 = 0x97;
    /// CSG / SolidMesh geometry fragments [inferred].
    pub const SOLID_MESH: u8 = 0xa5;
}

/// The replicator item kinds. Names match the type names (`NewObjectItem`,
/// `ChangePropertyItem`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    NewObject,
    DeleteInstance,
    DestroyInstance,
    ChangeProperty,
    ChangeAttribute,
    EventInvocation,
    Marker,
    Ping,
    Tag,
    JoinData,
    Cluster,
    SharedString,
    StreamData,
    InstanceIntent,
    ResolveNewInstanceAtomicTree,
    SharedReferencePropertyChanged,
    Stats,
}

/// One terrain box in voxel coordinates. An empty payload clears the box.
#[derive(Debug, Clone)]
pub struct TerrainChunk {
    /// `ClusterItem` mode byte (`0..=5`) when streaming is enabled, or `None`
    /// when streaming is disabled (`SET_GLOBALS` flag 0 == 0).
    pub mode: Option<u8>,
    pub origin: [i32; 3],
    pub size_log2: u8,
    pub lod: u8,
    pub runs: Vec<TerrainRun>,
    /// Offset and extent within the chunk, in voxels.
    pub boxes: Vec<([u8; 3], [u8; 3])>,
}

#[derive(Debug, Clone, Copy)]
pub struct TerrainRun {
    pub length: u16,
    pub material: u8,
    pub occupancy: u8,
    pub water: u16,
}

/// A decoded replicator op in rustudio's own terms: what to do to the live
/// DataModel. Instance identity is the server's referent (a wire id); the
/// model maps it to an rbx-dom [`Ref`].
#[derive(Debug, Clone)]
pub enum RepOp {
    /// The initial place stream has begun; following ops build the DataModel.
    JoinDataBegin,
    /// A new instance: server referent, class name, parent referent (None =
    /// a top-level service/DataModel child), and its initial properties.
    NewInstance {
        id: u64,
        class: String,
        parent: Option<u64>,
        props: Vec<(String, PropValue)>,
    },
    /// A property changed on an existing instance.
    ChangeProperty {
        id: u64,
        name: String,
        value: PropValue,
    },
    /// One attribute set (or removed: empty `value`) on an instance. `value`
    /// is a single attribute body as in the `Attributes` blob (type byte,
    /// then the value), without the count or name.
    ChangeAttribute { id: u64, name: String, value: Vec<u8> },
    /// An instance was removed.
    DeleteInstance { id: u64 },
    /// An instance moved under a new parent (item 0x13: post-join batch
    /// roots arrive with a nil parent and are attached this way).
    Reparent { id: u64, parent: u64 },
    /// A collaborator presence update (camera, selection, status).
    Presence { user_id: u64 },
    /// A remote event fired on an instance (`Class.Event` and its args).
    Event {
        id: u64,
        name: String,
        args: Vec<PropValue>,
    },
    /// Anything we recognise the kind of but can't decode the body of yet.
    Unhandled { kind: ItemKind },
    /// A net asset (a union's SolidMeshHolder contents) from the 0xa5
    /// channel, keyed by the id a holder property refers to.
    NetAsset { key: NetAssetId, data: Vec<u8> },
    /// Authoritative voxel updates for a Terrain instance.
    Terrain { id: u64, chunks: Vec<TerrainChunk> },
    /// SET_GLOBALS' top-level service table: (server referent, class). Some
    /// services (ReplicatedFirst) are never sent as NewInstance, only listed
    /// here, while their children arrive with the listed referent as parent.
    Services(Vec<(u64, String)>),
}

/// A property value in transit.
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    /// Explicit int64 wire value (0x2c), including values that fit in i32.
    Int64(i64),
    Float(f64),
    Float32(f32),
    String(String),
    /// A string-typed wire value containing arbitrary non-UTF-8 bytes.
    BinaryString(Vec<u8>),
    /// A reference to another instance by server referent.
    Ref(Option<u64>),
    Enum(u32),
    Vector2([f32; 2]),
    Vector3([f32; 3]),
    Color3([f32; 3]),
    Color3uint8([u8; 3]),
    /// Position, then the row-major 3x3 rotation.
    CFrame {
        pos: [f32; 3],
        rot: [f32; 9],
    },
    Array(Vec<PropValue>),
    Dictionary(Vec<(String, PropValue)>),
    /// A 16-byte shared-string hash (ProtectedString sources, SharedString).
    SharedHash([u8; 16]),
    /// Resolved binary contents of a replicated shared/protected string.
    SharedString(Vec<u8>),
    CachedSharedString {
        hash: [u8; 16],
        bytes: Vec<u8>,
    },
    /// Decoded but not modelled further: raw bytes kept for analysis.
    Raw(Vec<u8>),
    /// A net asset handle (wire type 0x3c, e.g. SolidMeshHolder): the id of
    /// an asset whose contents arrive on the 0xa5 channel. `None` is the
    /// empty handle.
    NetAssetHandle(Option<NetAssetId>),
    /// A value of wire type `ty` whose payload the model converts to a
    /// Roblox type (UDim/UDim2, Vector3int16, NumberSequence, NumberRange,
    /// ColorSequence, PhysicalProperties, Font). `bytes` is laid out as
    /// `Raw` was for the same type (sequences without their count,
    /// PhysicalProperties without its flag byte), so the writer sends it
    /// unchanged.
    Typed { ty: u8, bytes: Vec<u8> },
}

/// A parse failure. `Incomplete` means the input ended mid-item (a record
/// crossing a chunk boundary): retry with more bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Incomplete,
    Bad(String),
}

type R<T> = Result<T, DecodeError>;
type Props = Vec<(String, PropValue)>;

fn bad<T>(msg: impl Into<String>) -> R<T> {
    Err(DecodeError::Bad(msg.into()))
}

/// A byte cursor.
struct Cur<'a> {
    d: &'a [u8],
    o: usize,
}

impl<'a> Cur<'a> {
    fn new(d: &'a [u8]) -> Self {
        Cur { d, o: 0 }
    }
    fn left(&self) -> usize {
        self.d.len() - self.o
    }
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        if self.left() < n {
            return Err(DecodeError::Incomplete);
        }
        self.o += n;
        Ok(&self.d[self.o - n..self.o])
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }
    fn peek(&self) -> R<u8> {
        self.d.get(self.o).copied().ok_or(DecodeError::Incomplete)
    }
    fn u16(&mut self) -> R<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> R<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> R<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32s<const N: usize>(&mut self) -> R<[f32; N]> {
        let mut a = [0f32; N];
        for v in &mut a {
            *v = self.f32()?;
        }
        Ok(a)
    }
    /// Unsigned LEB128.
    fn varint(&mut self) -> R<u64> {
        let (mut v, mut s) = (0u64, 0u32);
        loop {
            let b = self.u8()?;
            if s < 64 {
                v |= u64::from(b & 0x7f) << s;
            }
            s += 7;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
    }
    fn zigzag(&mut self) -> R<i64> {
        let v = self.varint()?;
        Ok((v >> 1) as i64 ^ -((v & 1) as i64))
    }
    fn str_n(&mut self, n: usize) -> R<String> {
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn varstr(&mut self) -> R<String> {
        let n = self.varint()? as usize;
        self.str_n(n)
    }
    /// `01 <u32 LE id>` or `00` (nil).
    fn reference(&mut self) -> R<Option<u64>> {
        // writes a varint scope followed by
        // the u32 instance index. Scope 1 is the server's usual namespace;
        // peer-created instances retain their own scope.
        let scope = self.varint()?;
        if scope == 0 {
            return Ok(None);
        }
        if scope > u32::MAX as u64 {
            return bad("reference scope exceeds u32");
        }
        let index = u64::from(self.u32()?);
        Ok(Some(if scope == 1 {
            index
        } else {
            (scope << 32) | index
        }))
    }
}

/// One property of a schema class.
#[derive(Debug, Clone)]
pub struct SchemaProp {
    pub name: String,
    pub ty: u8,
}

#[derive(Debug, Clone)]
pub struct SchemaEvent {
    pub name: String,
    pub arg_types: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct SchemaClass {
    pub name: String,
    pub props: Vec<SchemaProp>,
    pub events: Vec<SchemaEvent>,
}

/// The replication schema (packet 0x97). Items index classes, class-local
/// props, global props and global events through it.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub enums: Vec<String>,
    pub classes: Vec<SchemaClass>,
    /// (class index, prop index) by global prop index.
    pub global_props: Vec<(u16, u16)>,
    /// (class index, event index) by global event index.
    pub global_events: Vec<(u16, u16)>,
    pub content_prefixes: Vec<String>,
    /// The common-name dictionary used by Name values (`0x80 | index`).
    pub names: Vec<String>,
    /// Net asset handles (type 0x3c) carry a varint schema asset type before
    /// the id instead of a u64 type after it. Set by bit 5 of the header's
    /// engine configuration word (the client reads it at the end of the
    /// schema reader); off in a 2026-11-04 join (`03 01`), on in a
    /// 2026-11-18 join (`03 21`).
    pub net_asset_handles: bool,
    /// ReplicationPV (type 0x39) uses the bit-packed v2 layout
    /// instead of v1. Set by bit 53 (`0x35`) of the header's
    /// engine configuration word.
    pub replication_pv_v2: bool,
}

impl Schema {
    /// Parse the decompressed 0x97 body.
    pub fn parse(d: &[u8]) -> Result<Schema, DecodeError> {
        let mut c = Cur::new(d);
        // Header: varint schema version (the client rejects anything but 3),
        // then the u64 engine configuration word.
        let version = c.varint()?;
        if version != 3 {
            return bad(format!("schema version {version}"));
        }
        let config = c.u64()?;
        let mut s = Schema {
            net_asset_handles: (config >> 5) & 1 != 0,
            replication_pv_v2: (config >> 53) & 1 != 0,
            ..Schema::default()
        };
        for _ in 0..c.varint()? {
            s.enums.push(c.varstr()?);
            c.u8()?;
        }
        let n_classes = c.varint()?;
        c.varint()?;
        c.varint()?;
        for ci in 0..n_classes {
            let name = c.varstr()?;
            let mut props = Vec::new();
            for pi in 0..c.varint()? {
                let pname = c.varstr()?;
                let ty = c.u8()?;
                match ty {
                    0x07 => {
                        c.take(2)?;
                    }
                    0x3c => {
                        c.take(1)?;
                    }
                    _ => {}
                }
                s.global_props.push((ci as u16, pi as u16));
                props.push(SchemaProp { name: pname, ty });
            }
            c.take(2)?;
            let mut events = Vec::new();
            for ei in 0..c.varint()? {
                let ename = c.varstr()?;
                let mut arg_types = Vec::new();
                for _ in 0..c.varint()? {
                    let a = c.take(3)?;
                    arg_types.push(a[0]);
                }
                s.global_events.push((ci as u16, ei as u16));
                events.push(SchemaEvent {
                    name: ename,
                    arg_types,
                });
            }
            s.classes.push(SchemaClass {
                name,
                props,
                events,
            });
        }
        for _ in 0..c.varint()? {
            s.content_prefixes.push(c.varstr()?);
        }
        for _ in 0..c.varint()? {
            s.names.push(c.varstr()?);
        }
        Ok(s)
    }

    /// Decode one property value of schema type `ty` (with ContentId prefix
    /// compression enabled, as in `ChangePropertyItem` and `EventInvocationItem`).
    fn value(&self, ty: u8, c: &mut Cur) -> R<PropValue> {
        self.value_at_depth(ty, c, 0, true)
    }

    /// Decode one property value with explicit `compress_content` flag
    /// (`false` in `JoinData` and `NewObject` normal group; `true` in `ChangeProperty` / `EventInvocation`).
    fn value_with(&self, ty: u8, c: &mut Cur, compress_content: bool) -> R<PropValue> {
        self.value_at_depth(ty, c, 0, compress_content)
    }

    #[cfg(test)]
    pub(crate) fn decode_value_bytes(
        &self,
        ty: u8,
        bytes: &[u8],
        compress_content: bool,
    ) -> R<(PropValue, usize)> {
        let mut c = Cur::new(bytes);
        let v = self.value_with(ty, &mut c, compress_content)?;
        Ok((v, c.o))
    }

    /// Read a ContentId string. When `compress` is true, a
    /// leading tag byte selects `schema.content_prefixes[tag >> 1]` and
    /// `tag & 1` chooses zigzag i64 vs varstr suffix.
    fn content_id(&self, c: &mut Cur, compress: bool) -> R<String> {
        if !compress {
            return c.varstr();
        }
        let tag = c.u8()?;
        let idx = usize::from(tag >> 1);
        let prefix = if idx == 0 && self.content_prefixes.is_empty() {
            ""
        } else {
            self.content_prefixes
                .get(idx)
                .map(String::as_str)
                .ok_or_else(|| DecodeError::Bad(format!("content prefix index {idx}")))?
        };
        if tag & 1 != 0 {
            let id = c.zigzag()?;
            Ok(format!("{prefix}{id}"))
        } else {
            let suffix = c.varstr()?;
            Ok(format!("{prefix}{suffix}"))
        }
    }

    /// Read one type-tagged container element: `0x00` is nil,
    /// `0x07` carries a `u16 LE` enum id before the varint enum value, and
    /// every other tag dispatches to `value_at_depth`.
    fn tagged_value(&self, c: &mut Cur, depth: usize, compress_content: bool) -> R<PropValue> {
        let ety = c.u8()?;
        if ety == 0x00 {
            return Ok(PropValue::Typed {
                ty: 0x03,
                bytes: Vec::new(),
            });
        }
        if ety == 0x07 {
            c.u16()?;
        }
        self.value_at_depth(ety, c, depth + 1, compress_content)
    }

    /// Studio 0.748 master value deserializer (valid range `0x02..=0x47` excluding `0x04, 0x05, 0x17,
    /// 0x2d, 0x40`).
    fn value_at_depth(
        &self,
        ty: u8,
        c: &mut Cur,
        depth: usize,
        compress_content: bool,
    ) -> R<PropValue> {
        if depth > 32 {
            return bad("nested value depth");
        }
        Ok(match ty {
            // 0x02 string
            0x02 => {
                let n = c.varint()? as usize;
                let bytes = c.take(n)?.to_vec();
                match String::from_utf8(bytes) {
                    Ok(text) => PropValue::String(text),
                    Err(error) => PropValue::BinaryString(error.into_bytes()),
                }
            }
            // 0x03 Void / nil: 0 bytes
            0x03 => PropValue::Typed {
                ty: 0x03,
                bytes: Vec::new(),
            },
            // 0x06 ProtectedString, 0x2e SharedString,
            // 0x2f ProtectedString alt/bytecode
            0x06 | 0x2e | 0x2f => PropValue::SharedHash(c.take(16)?.try_into().unwrap()),
            // 0x07 Enum
            0x07 => PropValue::Enum(c.varint()? as u32),
            // 0x08 BinaryString
            0x08 => {
                let n = c.varint()? as usize;
                PropValue::BinaryString(c.take(n)?.to_vec())
            }
            // 0x09 bool
            0x09 => PropValue::Bool(c.u8()? != 0),
            // 0x0a int32
            0x0a => PropValue::Int(i64::from(c.zigzag()? as i32)),
            // 0x0b float
            0x0b => PropValue::Float32(c.f32()?),
            // 0x0c double
            0x0c => PropValue::Float(f64::from_le_bytes(c.take(8)?.try_into().unwrap())),
            // 0x0d UDim: f32 scale, i32 offset
            0x0d => PropValue::Typed {
                ty,
                bytes: c.take(8)?.to_vec(),
            },
            // 0x0e UDim2: 2x UDim
            0x0e => PropValue::Typed {
                ty,
                bytes: c.take(16)?.to_vec(),
            },
            // 0x0f Ray: origin 3xf32, direction 3xf32
            0x0f => PropValue::Typed {
                ty,
                bytes: c.take(24)?.to_vec(),
            },
            // 0x10 Faces, 0x11 Axes: u32 LE
            0x10 | 0x11 => PropValue::Typed {
                ty,
                bytes: c.take(4)?.to_vec(),
            },
            // 0x12 BrickColor: u16 LE
            0x12 => PropValue::Typed {
                ty,
                bytes: c.take(2)?.to_vec(),
            },
            // 0x13 Color3
            0x13 => PropValue::Color3(c.f32s()?),
            // 0x14 Color3uint8
            0x14 => PropValue::Color3uint8(c.take(3)?.try_into().unwrap()),
            // 0x15 Vector2
            0x15 => PropValue::Vector2(c.f32s()?),
            // 0x16 Vector3
            0x16 => PropValue::Vector3(c.f32s()?),
            // 0x18 Vector2int16: 2x i16 LE
            0x18 => PropValue::Typed {
                ty,
                bytes: c.take(4)?.to_vec(),
            },
            // 0x19 Vector3int16: 3x i16 LE
            0x19 => PropValue::Typed {
                ty,
                bytes: c.take(6)?.to_vec(),
            },
            // 0x1a CoordinateFrame
            0x1a => {
                let pos = c.f32s()?;
                let rot = rotation(c.u8()?, c)?;
                PropValue::CFrame { pos, rot }
            }
            // 0x1b CoordinateFrame compressed
            0x1b => {
                let pos = c.f32s()?;
                let rot = rotation_compressed(c.u8()?, c)?;
                PropValue::CFrame { pos, rot }
            }
            // 0x1c Object, 0x45 InstanceRef
            0x1c | 0x45 => PropValue::Ref(c.reference()?),
            // 0x1d Tuple, 0x1e Array,
            // 0x1f Dictionary/ValueTable, 0x20 Map/ValueMap
            0x1d..=0x20 => {
                let count = c.varint()? as usize;
                if count > c.left() {
                    return Err(DecodeError::Incomplete);
                }
                let is_map = matches!(ty, 0x1f | 0x20);
                let mut array = Vec::new();
                let mut dictionary = Vec::new();
                for _ in 0..count {
                    let key = if is_map { Some(c.varstr()?) } else { None };
                    let value = self.tagged_value(c, depth, compress_content)?;
                    if let Some(key) = key {
                        dictionary.push((key, value));
                    } else {
                        array.push(value);
                    }
                }
                if is_map {
                    PropValue::Dictionary(dictionary)
                } else {
                    PropValue::Array(array)
                }
            }
            // 0x21 ContentId
            0x21 => PropValue::String(self.content_id(c, compress_content)?),
            // 0x22 SystemAddress
            0x22 => PropValue::Int(c.varint()? as i64),
            // 0x23 NumberSequence,
            // 0x26 ColorSequence
            0x23 | 0x26 => {
                let n = c.u32()? as usize;
                if n > 1024 {
                    return bad(format!("sequence of {n}"));
                }
                let w = if ty == 0x23 { 12 } else { 20 };
                PropValue::Typed {
                    ty,
                    bytes: c.take(n * w)?.to_vec(),
                }
            }
            // 0x24 NumberSequenceKeypoint: 3x f32
            0x24 => PropValue::Raw(c.take(12)?.to_vec()),
            // 0x25 NumberRange: 2x f32
            0x25 => PropValue::Typed {
                ty,
                bytes: c.take(8)?.to_vec(),
            },
            // 0x27 ColorSequenceKeypoint: 5x f32
            0x27 => PropValue::Raw(c.take(20)?.to_vec()),
            // 0x28 Rect2D: 4x f32
            0x28 => PropValue::Typed {
                ty,
                bytes: c.take(16)?.to_vec(),
            },
            // 0x29 PhysicalProperties
            0x29 => {
                let fl = c.u8()?;
                let n = if fl & 1 == 0 {
                    0
                } else if fl >= 2 {
                    24
                } else {
                    20
                };
                PropValue::Typed {
                    ty,
                    bytes: c.take(n)?.to_vec(),
                }
            }
            // 0x2a Region3: min 3xf32, max 3xf32
            0x2a => PropValue::Typed {
                ty,
                bytes: c.take(24)?.to_vec(),
            },
            // 0x2b Region3int16: min 3xi16, max 3xi16
            0x2b => PropValue::Typed {
                ty,
                bytes: c.take(12)?.to_vec(),
            },
            // 0x2c int64
            0x2c => PropValue::Int64(c.zigzag()?),
            // 0x30 DateTime: u64 LE millis
            0x30 => PropValue::Raw(c.take(8)?.to_vec()),
            // 0x31 Name
            0x31 => {
                let b = c.u8()?;
                if b & 0x80 != 0 {
                    let lo = b & 0x7f;
                    let idx = if lo == 0x7f {
                        0x7f + (c.varint()? as usize)
                    } else {
                        usize::from(lo)
                    };
                    match self.names.get(idx) {
                        Some(n) => PropValue::String(n.clone()),
                        None => return bad(format!("name dictionary index {idx}")),
                    }
                } else if b == 0x7f {
                    PropValue::String(c.varstr()?)
                } else {
                    PropValue::String(c.str_n(usize::from(b))?)
                }
            }
            // 0x32 OptionalCoordinateFrame,
            // 0x33 OptionalCoordinateFrame compressed
            0x32 | 0x33 => {
                let b = c.u8()?;
                if b & 0x80 == 0 {
                    PropValue::Typed {
                        ty: 0x32,
                        bytes: Vec::new(),
                    }
                } else {
                    let rot = if ty == 0x32 {
                        rotation(b & 0x7f, c)?
                    } else {
                        rotation_compressed(b & 0x7f, c)?
                    };
                    let pos: [f32; 3] = c.f32s()?;
                    let mut bytes = Vec::with_capacity(48);
                    for f in pos {
                        bytes.extend_from_slice(&f.to_le_bytes());
                    }
                    for f in rot {
                        bytes.extend_from_slice(&f.to_le_bytes());
                    }
                    PropValue::Typed { ty: 0x32, bytes }
                }
            }
            // 0x34 UniqueId: u64 LE random, u32 LE time, u32 LE index
            0x34 => PropValue::Typed {
                ty,
                bytes: c.take(16)?.to_vec(),
            },
            // 0x35 PathWaypoint: 3xf32 pos, u8 action (<3), u32 len + label
            0x35 => {
                let start = c.o;
                c.take(12)?;
                let action = c.u8()?;
                if action >= 3 {
                    return bad(format!("PathWaypoint action {action}"));
                }
                let n = c.u32()? as usize;
                c.take(n)?;
                PropValue::Raw(c.d[start..c.o].to_vec())
            }
            // 0x36 Font
            0x36 => {
                let start = c.o;
                c.take(3)?;
                let n = c.u32()? as usize;
                c.take(n)?;
                let n2 = c.u32()? as usize;
                c.take(n2)?;
                PropValue::Typed {
                    ty,
                    bytes: c.d[start..c.o].to_vec(),
                }
            }
            // 0x37 SecurityCapabilities: u64 LE
            0x37 => PropValue::Typed {
                ty,
                bytes: c.take(8)?.to_vec(),
            },
            // 0x38 buffer: varint comp_len, varint decomp_len, payload
            0x38 => {
                let comp_len = c.varint()? as usize;
                let decomp_len = c.varint()? as usize;
                if comp_len == 0 {
                    PropValue::BinaryString(c.take(decomp_len)?.to_vec())
                } else {
                    let comp = c.take(comp_len)?;
                    PropValue::BinaryString(unzstd(comp, decomp_len)?)
                }
            }
            // 0x39 ReplicationPV
            0x39 => {
                let start = c.o;
                if self.replication_pv_v2 {
                    read_replication_pv_v2(c)?;
                } else {
                    read_replication_pv_v1(c)?;
                }
                PropValue::Raw(c.d[start..c.o].to_vec())
            }
            // 0x3a FacsReplicationData: u8 flags, 6B, u8 count, count bytes
            0x3a => {
                let start = c.o;
                c.take(7)?;
                let count = usize::from(c.u8()?);
                c.take(count)?;
                PropValue::Raw(c.d[start..c.o].to_vec())
            }
            // 0x3b NetAssetHandle, 0x3c NetAssetRef
            0x3b | 0x3c => PropValue::NetAssetHandle(self.net_asset_handle(c)?),
            // 0x3d Content: varint kind (0=None, 1=Uri, 2=Object, 3=Embedded/AssetUri)
            0x3d => match c.varint()? {
                0 => PropValue::String(String::new()),
                1 | 3 => PropValue::String(self.content_id(c, compress_content)?),
                2 => {
                    let start = c.o;
                    c.reference()?;
                    PropValue::Raw(c.d[start..c.o].to_vec())
                }
                k => return bad(format!("content kind {k:#x}")),
            },
            // 0x3e Random: u64 LE seed
            0x3e => PropValue::Raw(c.take(8)?.to_vec()),
            // 0x3f SlimReplicationData: u8 count, count*7B, count*1B
            0x3f => {
                let start = c.o;
                let count = usize::from(c.u8()?);
                c.take(count * 8)?;
                PropValue::Raw(c.d[start..c.o].to_vec())
            }
            // 0x41 AnimTrackPlayState: bool, f32, f64
            0x41 => PropValue::Raw(c.take(13)?.to_vec()),
            // 0x42 AnimTrackMetadata: bool, u16 LE
            0x42 => PropValue::Raw(c.take(3)?.to_vec()),
            // 0x43 AnimTrackWeight: f32, f32, f64
            0x43 => PropValue::Raw(c.take(16)?.to_vec()),
            // 0x44 User: zigzag i64, varint u32, zigzag i64, zigzag i64
            0x44 => {
                let start = c.o;
                for _ in 0..4 {
                    c.varint()?;
                }
                PropValue::Raw(c.d[start..c.o].to_vec())
            }
            // 0x46 TweenInfo v1: 15B
            0x46 => PropValue::Raw(c.take(15)?.to_vec()),
            // 0x47 TweenInfo v2: 15B + curve params for style 11/12
            0x47 => {
                let start = c.o;
                let base = c.take(15)?;
                let easing_style = base[12];
                match easing_style {
                    0x0c => {
                        c.take(8)?;
                    }
                    0x0b => {
                        let count = usize::from(c.u8()?);
                        c.take(count * 8)?;
                    }
                    _ => {}
                }
                PropValue::Raw(c.d[start..c.o].to_vec())
            }
            t => return bad(format!("value type {t:#x}")),
        })
    }
}

impl Schema {
    /// One net asset handle. With [`Schema::net_asset_handles`]: a varint
    /// schema asset type (`0xffff` = empty handle, nothing follows), then
    /// the id. Without: the id, then a u64 local asset type. The id is the
    /// same in both, and in the 0xa5 begin message.
    fn net_asset_handle(&self, c: &mut Cur) -> R<Option<NetAssetId>> {
        if !self.net_asset_handles {
            let id = NetAssetId::read(c)?;
            c.u64()?;
            return Ok(Some(id));
        }
        match c.varint()? {
            0xffff => Ok(None),
            ty if ty > 0xffff => bad(format!("net asset type {ty}")),
            _ => NetAssetId::read(c).map(Some),
        }
    }
}

/// A net asset's id: the creating peer's u64 GUID word, then a varint index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NetAssetId {
    pub guid: u64,
    pub index: u32,
}

impl NetAssetId {
    fn read(c: &mut Cur) -> R<NetAssetId> {
        let guid = c.u64()?;
        let index = c.varint()?;
        let index = u32::try_from(index).map_err(|_| DecodeError::Bad(format!("net asset index {index}")))?;
        Ok(NetAssetId { guid, index })
    }
}

/// A CFrame rotation: id 0 = nine f32 follow, else a basic (axis-aligned)
/// rotation id as in the binary place format.
fn rotation(id: u8, c: &mut Cur) -> R<[f32; 9]> {
    if id == 0 {
        return c.f32s();
    }
    match rbx_types::Matrix3::from_basic_rotation_id(id) {
        Ok(m) => Ok([
            m.x.x, m.x.y, m.x.z, m.y.x, m.y.y, m.y.z, m.z.x, m.z.y, m.z.z,
        ]),
        Err(_) => bad(format!("rotation id {id:#x}")),
    }
}

/// Compressed CFrame rotation: id 0 = 6-byte
/// smallest-three packed quaternion, else basic rotation id.
fn rotation_compressed(id: u8, c: &mut Cur) -> R<[f32; 9]> {
    if id != 0 {
        return rotation(id, c);
    }
    let a = c.u16()?;
    let b = c.u32()?;
    let max_idx = (b >> 30) as usize;
    let scale = std::f32::consts::FRAC_1_SQRT_2 / 16383.0;
    let c0 = (((a as i32) << 17) >> 17) as f32 * scale;
    let c1 = (((b as i32) << 2) >> 17) as f32 * scale;
    let c2 = (((b as i32) << 17) >> 17) as f32 * scale;
    // Index table: (1,2,3), (0,2,3), (0,1,3), (0,1,2)
    const IDX: [[usize; 3]; 4] = [[1, 2, 3], [0, 2, 3], [0, 1, 3], [0, 1, 2]];
    let mut q = [0.0f32; 4];
    let map = IDX[max_idx];
    q[map[0]] = c0;
    q[map[1]] = c1;
    q[map[2]] = c2;
    q[max_idx] = (1.0 - (c0 * c0 + c1 * c1 + c2 * c2)).max(0.0).sqrt();
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if len > 0.0 {
        for v in &mut q {
            *v /= len;
        }
    }
    let [x, y, z, w] = q;
    Ok([
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y - z * w),
        2.0 * (x * z + y * w),
        2.0 * (x * y + z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z - x * w),
        2.0 * (x * z - y * w),
        2.0 * (y * z + x * w),
        1.0 - 2.0 * (x * x + y * y),
    ])
}

/// ReplicationPV v1 (schema config bit 53 clear).
fn read_replication_pv_v1(c: &mut Cur) -> R<()> {
    let flags = c.u8()?;
    if flags & 0x80 == 0 {
        // Compressed Vector3 position
        let hdr = c.u8()?;
        let exp = hdr >> 3;
        let extra = if exp <= 4 {
            4
        } else if exp <= 10 {
            6
        } else {
            8
        };
        c.take(extra)?;
    }
    match flags & 0x1f {
        0x1f => {
            // 7x f32
            c.take(28)?;
        }
        0 => {
            // 6B packed quaternion
            c.take(6)?;
        }
        1..=24 => {}
        r => return bad(format!("ReplicationPV rotation id {r}")),
    }
    // Compressed linear velocity
    if flags & 0x40 == 0 && c.u8()? != 0 {
        c.take(4)?;
    }
    // Compressed angular velocity
    if flags & 0x20 == 0 && c.u8()? != 0 {
        c.take(4)?;
    }
    Ok(())
}

/// Bit width per component for ReplicationPV v2:
/// `ceil(exp - log2(0.01)) + 1` rounded up to `{12, 20, 28, 36, 44, 52}`.
/// Each vector reads `4 + 3 * bits` bits, which is always a whole number of bytes.
fn pv_v2_vec_bytes(exp: u8) -> usize {
    let raw = (f64::from(exp) - 0.01f64.log2()).ceil() as i32;
    let bits = raw.max(0) + 1;
    let bpc = if bits <= 12 {
        12
    } else if bits <= 20 {
        20
    } else if bits <= 28 {
        28
    } else if bits <= 36 {
        36
    } else if bits <= 44 {
        44
    } else {
        52
    };
    (4 + 3 * bpc) / 8
}

/// ReplicationPV v2 (schema config bit 53 set).
fn read_replication_pv_v2(c: &mut Cur) -> R<()> {
    let hdr = c.u8()?;
    let pos_zero = hdr & 1 != 0;
    let pos_exp_hi = (hdr >> 1) & 3;
    let rot_mode = (hdr >> 3) & 3;
    let vel_zero = (hdr >> 5) & 1 != 0;
    let avel_zero = (hdr >> 6) & 1 != 0;
    if !pos_zero {
        let first = c.peek()?;
        let exp = (pos_exp_hi << 4) | (first & 0x0f);
        c.take(pv_v2_vec_bytes(exp))?;
    }
    if !vel_zero {
        let first = c.peek()?;
        let exp = first & 0x0f;
        c.take(pv_v2_vec_bytes(exp))?;
    }
    if !avel_zero {
        let first = c.peek()?;
        let exp = first & 0x0f;
        c.take(pv_v2_vec_bytes(exp))?;
    }
    match rot_mode {
        0 => {}
        1 => {
            c.take(4)?;
        }
        2 => {
            c.take(5)?;
        }
        3 => {
            c.take(6)?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

// A local resource limit, not a claimed protocol maximum.
const MAX_NET_ASSET_BYTES: usize = 64 * 1024 * 1024;

/// A raw LZ4 block (no frame), decoded until the input ends.
pub fn lz4_block(src: &[u8]) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(src.len().min(4096));
    let mut i = 0usize;
    let len = |i: &mut usize, mut n: usize| -> Option<usize> {
        if n == 15 {
            loop {
                let v = *src.get(*i)? as usize;
                *i += 1;
                n += v;
                if v != 255 {
                    break;
                }
            }
        }
        Some(n)
    };
    while i < src.len() {
        let tok = src[i];
        i += 1;
        let ll = len(&mut i, (tok >> 4) as usize)?;
        if out.len().checked_add(ll)? > MAX_NET_ASSET_BYTES { return None; }
        out.extend_from_slice(src.get(i..i.checked_add(ll)?)?);
        i += ll;
        if i >= src.len() {
            break;
        }
        let off = u16::from_le_bytes([*src.get(i)?, *src.get(i + 1)?]) as usize;
        i += 2;
        if off == 0 || off > out.len() {
            return None;
        }
        let ml = len(&mut i, (tok & 15) as usize)? + 4;
        if out.len().checked_add(ml)? > MAX_NET_ASSET_BYTES { return None; }
        let start = out.len() - off;
        for k in 0..ml {
            let b = out[start + k];
            out.push(b);
        }
    }
    Some(out)
}

/// Split a reassembled `rbx-rtcio` stream into channel, tag and messages.
pub fn split_stream(b: &[u8]) -> Option<(u8, [u8; 4], Vec<&[u8]>)> {
    if b.len() < 7 || b[0] != 0x06 || b[1] != 0x01 {
        return None;
    }
    let mut msgs = Vec::new();
    let mut o = 7;
    while o + 4 <= b.len() {
        let w = u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        let n = (w & 0x7fff_ffff) as usize;
        if w >> 31 == 0 || o + 4 + n > b.len() {
            break;
        }
        msgs.push(&b[o + 4..o + 4 + n]);
        o += 4 + n;
    }
    Some((b[2], b[3..7].try_into().unwrap(), msgs))
}

fn unzstd(d: &[u8], size: usize) -> R<Vec<u8>> {
    let out = zstd::bulk::decompress(d, size.max(1))
        .map_err(|e| DecodeError::Bad(format!("zstd: {e}")))?;
    if out.len() != size {
        return bad("zstd size mismatch");
    }
    Ok(out)
}

/// Studio 0.748 `DeserializedClusterItem::deserialize` and
/// chunk record reader.
///
/// When streaming is enabled (the replicator's streaming flag, set by/// `SET_GLOBALS` flag 0), a `u8` mode byte (`0..=5`) follows the Terrain
/// reference; mode `5` is followed by a `u32 LE` stream/pin id.
/// When streaming is disabled, the zstd block follows the reference directly.
fn terrain_packet(body: &[u8], streaming: bool) -> R<RepOp> {
    let mut c = Cur::new(body);
    let id = c.reference()?.ok_or(DecodeError::Bad("nil Terrain reference".into()))?;
    let mode = if streaming {
        let m = c.u8()?;
        if m >= 6 {
            return bad("unsupported terrain packet mode");
        }
        if m == 5 {
            let _stream_id = c.u32()?;
        }
        Some(m)
    } else {
        None
    };
    let compressed = c.u32()? as usize;
    let expanded = c.u32()? as usize;
    if expanded > 4 << 20 {
        return bad("terrain block exceeds size limit");
    }
    let data = unzstd(c.take(compressed)?, expanded)?;
    if c.left() != 0 {
        return bad("trailing bytes in terrain packet");
    }
    let mut c = Cur::new(&data);
    let mut previous = [0i32; 3];
    let mut chunks = Vec::new();
    while c.left() != 0 {
        if chunks.len() == 4096 {
            return bad("too many terrain chunks in one block");
        }
        let header = c.u8()?;
        let size_log2 = header & 15;
        let lod = header >> 6;
        // Studio MaxTerrainClusterChunkSizeLog2 = 4.
        if size_log2 > 4 {
            return bad("unsupported terrain chunk size");
        }
        let shift = u32::from(size_log2 + lod);
        let chunk_voxels = 1usize << shift;
        let mut origin = [0; 3];
        for (pos, prev) in origin.iter_mut().zip(&mut previous) {
            let delta = match header & 0x30 {
                0 => i32::from(c.u8()? as i8),
                0x10 => i32::from(c.u16()? as i16),
                0x20 => c.u32()? as i32,
                _ => return bad("invalid terrain coordinate width"),
            };
            *prev = prev.checked_add(delta).ok_or(DecodeError::Bad("terrain coordinate overflow".into()))?;
            let max_coord = 1i32 << (29 - shift);
            if !(-max_coord + 1..max_coord).contains(prev) {
                return bad("terrain coordinate out of range");
            }
            *pos = *prev << shift;
        }
        let mut runs = Vec::new();
        match c.u8()? {
            0 => {
                let mut remaining = 1usize << (size_log2 * 3);
                while remaining != 0 {
                    let flags = c.u8()?;
                    let material = flags & 0x3f;
                    let occupancy = if flags & 0x40 != 0 { c.u8()? } else { 255 };
                    let mut length = 1;
                    let mut water = 0;
                    if flags & 0x80 != 0 {
                        let count = c.u8()?;
                        if count == 0 {
                            water = u16::from(c.u8()?) + 1;
                        } else {
                            length = u16::from(count) + 1;
                        }
                    }
                    remaining = remaining.checked_sub(usize::from(length))
                        .ok_or(DecodeError::Bad("terrain run exceeds chunk".into()))?;
                    runs.push(TerrainRun { length, material, occupancy, water });
                }
            }
            1 => {}
            _ => return bad("invalid terrain empty flag"),
        }
        let count = c.u8()?;
        let mut boxes = Vec::new();
        for _ in 0..count {
            let axes: [u8; 3] = c.take(3)?.try_into().unwrap();
            let offset = axes.map(|b| b & 15);
            let extent = axes.map(|b| (b >> 4) + 1);
            if (0..3).any(|axis| usize::from(offset[axis]) + usize::from(extent[axis]) > chunk_voxels) {
                return bad("terrain update box exceeds chunk");
            }
            boxes.push((offset, extent));
        }
        chunks.push(TerrainChunk { mode, origin, size_log2, lod, runs, boxes });
    }
    Ok(RepOp::Terrain { id, chunks })
}

/// SET_GLOBALS assigns the client's GUID namespace after seven bool flags
/// (StreamingEnabled, filtering, three third-party permissions, on-demand
/// ads, CharacterAutoLoads). Live joins can have filtering enabled.
pub fn assigned_peer_scope(body: &[u8]) -> Result<u32, String> {
    let mut c = Cur::new(body);
    c.take(7).map_err(|e| format!("{e:?}"))?;
    let scope = c.varint().map_err(|e| format!("{e:?}"))?;
    if scope <= 1 || scope > u32::MAX as u64 {
        return Err("invalid client namespace".into());
    }
    Ok(scope as u32)
}

/// SET_GLOBALS' top-level service table, after the peer scope: a u32 (zero
/// in every capture), a varint count, then per service its reference, the
/// u16 LE schema class and two flag bytes. Verified on the Place A join capture
/// (72 services; ReplicatedFirst 0x2110 is listed with flags `00 01` and never
/// sent as NewInstance, while ReplicatedFirst.First arrives with parent 0x2110).
pub fn top_services(schema: &Schema, body: &[u8]) -> Result<Vec<(u64, String)>, String> {
    let mut c = Cur::new(body);
    let e = |e: DecodeError| format!("{e:?}");
    c.u8().map_err(e)?;
    if c.u8().map_err(e)? != 0 {
        return Err("terrain globals block".into());
    }
    c.take(5).map_err(e)?;
    c.varint().map_err(e)?;
    c.take(4).map_err(e)?;
    let count = c.varint().map_err(e)?;
    let mut out = Vec::new();
    for _ in 0..count {
        let id = c.reference().map_err(e)?.ok_or("nil service reference")?;
        let class = usize::from(c.u16().map_err(e)?);
        c.take(2).map_err(e)?;
        let class = schema.classes.get(class).ok_or("service class index")?;
        out.push((id, class.name.clone()));
    }
    if c.left() != 0 {
        return Err("service table has trailing bytes".into());
    }
    Ok(out)
}

/// The stateful decoder for one replication stream (server → client).
#[derive(Default)]
pub struct Decoder {
    pub schema: Option<Schema>,
    /// Shared strings by hash (script sources and other blobs).
    pub shared: HashMap<[u8; 16], Vec<u8>>,
    /// Undecoded tails of the initial-stream stages (records cross chunks).
    stage_buf: HashMap<u8, Vec<u8>>,
    began: bool,
    pub(crate) initial_data_complete: bool,
    /// Items or values the decoder could not parse (kept for diagnostics).
    pub errors: Vec<String>,
    /// Item kinds this decoder does not know. Their length is unknown, so the
    /// rest of that message is skipped, but the session keeps going.
    pub skipped: Vec<u8>,
    /// The 0xa5 transfer in progress: its key, compressed length, chunks.
    net_asset: Option<(NetAssetId, usize, u16, Vec<u8>)>,
    /// Capability bit 56 (replicator capability flags): enables
    /// the optional third `u32` on `StreamDataInfoItem` (`0x14`).
    pub stream_data_info_v2: bool,
    /// Capability bit 51 (replicator capability flags):
    /// enables the optional `f32` on `PingItem` (`0x05`/`0x06`) when `flags & 0x10`.
    pub ping_extra_float: bool,
    /// Streaming flag (set from `SET_GLOBALS` byte 0): gates the mode byte on `0x8d` terrain packets.
    pub terrain_streaming: bool,
}

impl Decoder {
    pub fn new() -> Self {
        // Capability bit 51 is on in every Team Create session captured
        // (the f32 follows whenever flags & 0x10), and
        // nothing negotiates it here; off, every item after such a ping was
        // silently dropped (66 messages, whole instance batches among them).
        Self { ping_extra_float: true, terrain_streaming: true, ..Self::default() }
    }

    /// Decode one message (`packet_id` = its first byte, `body` = the rest).
    pub fn decode_frame(&mut self, packet_id: u8, body: &[u8]) -> Vec<RepOp> {
        let mut ops = Vec::new();
        let r = match packet_id {
            packet_id::NEW_SCHEMA => self.schema_msg(body),
            packet_id::DATA => self.data_msg(body, &mut ops),
            packet_id::SOLID_MESH => self.net_asset_msg(body, &mut ops),
            packet_id::TERRAIN => terrain_packet(body, self.terrain_streaming).map(|op| ops.push(op)),
            packet_id::SET_GLOBALS => {
                if let Some(&flag) = body.first() {
                    self.terrain_streaming = flag != 0;
                }
                if let Some(schema) = self.schema.as_ref() {
                    match top_services(schema, body) {
                        Ok(services) => ops.push(RepOp::Services(services)),
                        // The service table is optional for editing (services
                        // also arrive as NewInstance); a layout it doesn't know
                        // must not stop the session.
                        Err(e) => log::warn!("Team Create service table not read: {e}"),
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(e) = r {
            self.errors.push(format!("packet {packet_id:#x}: {e:?}"));
        }
        for op in &mut ops {
            match op {
                RepOp::NewInstance { props, .. } => {
                    for (_, value) in props {
                        self.resolve_shared(value);
                    }
                }
                RepOp::ChangeProperty { value, .. } => self.resolve_shared(value),
                _ => {}
            }
        }
        ops
    }

    /// The 0xa5 channel (2026-11-05, from the Place B [DEV] join capture): each
    /// transfer is `02 fe 04 <id> 00 <varint compressed length>` (begin; id as
    /// in a net asset handle, [`NetAssetId`]), then `01 fe <u16 LE chunk index>
    /// <≤512 bytes>` chunks, then `03 fe <varint>` (end); `06 01 <varint>`
    /// carries no geometry. The joined chunks are a varint of the decompressed
    /// size, then an LZ4 block: the holder blob without its `03 01` prefix
    /// (`<u32 size> … <u32 size> SolidMesh …`).
    fn net_asset_msg(&mut self, body: &[u8], ops: &mut Vec<RepOp>) -> R<()> {
        let mut c = Cur::new(body);
        match c.u8()? {
            2 => {
                c.u8()?; // fe
                c.u8()?; // 04
                let key = NetAssetId::read(&mut c)?;
                c.u8()?; // 00
                let len = c.varint()? as usize;
                if len > MAX_NET_ASSET_BYTES {
                    self.net_asset = None;
                    return bad("net asset compressed size exceeds limit");
                }
                self.net_asset = Some((key, len, 0, Vec::new()));
            }
            1 => {
                c.u8()?;
                let index = c.u16()?;
                if let Some((_, len, next, buf)) = &mut self.net_asset {
                    let chunk = &body[c.o..];
                    if index != *next || chunk.len() > 512 || buf.len() + chunk.len() > *len {
                        self.net_asset = None;
                        return bad("net asset chunk index or size mismatch");
                    }
                    *next = next.wrapping_add(1);
                    buf.extend_from_slice(chunk);
                }
            }
            3 => {
                if let Some((key, len, _, buf)) = self.net_asset.take() {
                    if buf.len() != len {
                        return bad("net asset truncated transfer");
                    }
                    let data = &buf[..];
                    // A varint of the decompressed size, then the LZ4 block.
                    let mut h = Cur::new(data);
                    let size = usize::try_from(h.varint()?).map_err(|_| DecodeError::Bad("net asset size overflow".into()))?;
                    if size > MAX_NET_ASSET_BYTES {
                        return bad("net asset decoded size exceeds limit");
                    }
                    match lz4_block(&data[h.o..]).filter(|o| o.len() == size) {
                        Some(mut out) => {
                            let mut holder = vec![3u8, 1];
                            holder.append(&mut out);
                            ops.push(RepOp::NetAsset { key, data: holder });
                        }
                        None => self.errors.push(format!("net asset {key:?}: LZ4 data didn't decode")),
                    }
                }
            }
            // `06 01 <varint>`: no geometry (3 in the Place B join; release or ack, not decoded).
            6 => {}
            k => return bad(format!("net asset message kind {k}")),
        }
        Ok(())
    }

    fn resolve_shared(&self, value: &mut PropValue) {
        if let PropValue::SharedHash(hash) = value {
            if let Some(bytes) = self.shared.get(hash) {
                *value = PropValue::CachedSharedString {
                    hash: *hash,
                    bytes: bytes.clone(),
                };
            }
        }
    }

    fn schema_msg(&mut self, body: &[u8]) -> R<()> {
        let mut c = Cur::new(body);
        let clen = c.u32()? as usize;
        let size = c.u32()? as usize;
        let d = unzstd(c.take(clen)?, size)?;
        self.schema = Some(Schema::parse(&d)?);
        Ok(())
    }

    fn data_msg(&mut self, body: &[u8], ops: &mut Vec<RepOp>) -> R<()> {
        let mut c = Cur::new(body);
        while c.left() > 0 && c.peek()? != 0 {
            self.item(&mut c, ops)?;
        }
        Ok(())
    }

    fn schema(&self) -> R<&Schema> {
        self.schema
            .as_ref()
            .ok_or(DecodeError::Bad("no schema yet".into()))
    }

    fn item(&mut self, c: &mut Cur, ops: &mut Vec<RepOp>) -> R<()> {
        let kind = c.u8()?;
        match kind {
            // DeleteInstanceItem (Studio 0.748): ref.
            0x01 => {
                if let Some(id) = c.reference()? {
                    ops.push(RepOp::DeleteInstance { id });
                }
            }
            // NewObjectItem (Studio 0.748): ref, u16 class,
            // early props + 0xff, normal props + 0xff, inline shared strings,
            // parent ref.
            0x02 => {
                let id = c.reference()?;
                let ci = c.u16()?;
                // NewObject carries an early property group (e.g. LocalScript
                // Source), then the ordinary properties. Player's early group
                // is empty (ff); it is not a universal fixed marker.
                let (_, mut early_props) = self.props(ci, c)?;
                let (class, props) = self.props(ci, c)?;
                early_props.extend(props);
                for (_, value) in &early_props {
                    if let PropValue::SharedHash(expected) = value {
                        self.inline_shared(c, expected)?;
                    }
                }
                let props = early_props;
                let parent = c.reference()?;
                if let Some(id) = id {
                    ops.push(RepOp::NewInstance {
                        id,
                        class,
                        parent,
                        props,
                    });
                }
            }
            // ChangePropertyItem (Studio 0.748): ref, u16 global
            // prop, u8 version, value, optional inline shared string.
            0x03 => {
                let id = c.reference()?;
                let gi = usize::from(c.u16()?);
                c.u8()?;
                let s = self.schema()?;
                // Parent is a reserved reference descriptor immediately after
                // the ordinary property dictionary (not a class-local prop).
                if gi == s.global_props.len() {
                    let parent = c.reference()?.unwrap_or(0);
                    if let Some(id) = id {
                        ops.push(RepOp::Reparent { id, parent });
                    }
                    return Ok(());
                }
                let &(ci, pi) = s
                    .global_props
                    .get(gi)
                    .ok_or(DecodeError::Bad(format!("global prop {gi}")))?;
                let cls = &s.classes[usize::from(ci)];
                let p = &cls.props[usize::from(pi)];
                let value = s.value(p.ty, c)?;
                let name = p.name.clone();
                let collaborator = cls.name == "Collaborator";
                if let PropValue::SharedHash(expected) = &value {
                    self.inline_shared(c, expected)?;
                }
                if let Some(id) = id {
                    if collaborator {
                        ops.push(RepOp::Presence { user_id: id });
                    }
                    ops.push(RepOp::ChangeProperty { id, name, value });
                }
            }
            // MarkerItem (Studio 0.748): u32 LE marker id.
            0x04 => {
                c.u32()?;
            }
            // PingItem / PingBackItem (Studio 0.748):
            // u8 flags, u64 ts, u32 t2, [flags&1: 6×u32], u32, u32, u8 mode(<2),
            // [flags&4: u32], [cap51 && flags&0x10: f32],
            // [flags&8: u8 has_trace; if 1: u32 len + len bytes].
            0x05 | 0x06 => {
                let flags = c.u8()?;
                c.u64()?;
                c.u32()?;
                if flags & 0x01 != 0 {
                    c.take(24)?;
                }
                c.u32()?;
                c.u32()?;
                let mode = c.u8()?;
                if mode >= 2 {
                    return bad(format!("ping mode {mode}"));
                }
                if flags & 0x04 != 0 {
                    c.u32()?;
                }
                if self.ping_extra_float && (flags & 0x10 != 0) {
                    c.f32()?;
                }
                if flags & 0x08 != 0 && c.u8()? == 1 {
                    let len = c.u32()? as usize;
                    c.take(len)?;
                }
                ops.push(RepOp::Unhandled {
                    kind: ItemKind::Ping,
                });
            }
            // EventInvocationItem (Studio 0.748): ref, u16 global
            // event, args typed by schema, optional inline shared strings.
            0x07 => {
                let id = c.reference()?;
                let gi = usize::from(c.u16()?);
                let s = self.schema()?;
                let &(ci, ei) = s
                    .global_events
                    .get(gi)
                    .ok_or(DecodeError::Bad(format!("global event {gi}")))?;
                let cls = &s.classes[usize::from(ci)];
                let e = &cls.events[usize::from(ei)];
                let name = format!("{}.{}", cls.name, e.name);
                let mut args = Vec::new();
                for &t in &e.arg_types {
                    args.push(s.value(t, c)?);
                }
                for value in &args {
                    if let PropValue::SharedHash(expected) = value {
                        self.inline_shared(c, expected)?;
                    }
                }
                ops.push(RepOp::Event {
                    id: id.unwrap_or(0),
                    name,
                    args,
                });
            }
            // PmcConfigItem (Studio 0.748): u8 version (must be 4).
            0x09 => {
                let version = c.u8()?;
                if version != 4 {
                    return bad(format!("pmc config version {version}"));
                }
            }
            // JoinDataItem (Studio 0.748): u32 count; if count != 0:
            // u32 clen, u32 size, zstd (count records + trailing shared strings).
            0x0b => {
                if !self.began {
                    self.began = true;
                    ops.push(RepOp::JoinDataBegin);
                }
                self.join_data_body(c, ops)?;
            }
            // DeletePureObjectItem (Studio 0.748): ref, varint.
            0x0c => {
                c.reference()?;
                c.varint()?;
            }
            // StreamDataItem (Studio 0.748):
            // embedded JoinDataItem, then compact region marker list.
            0x0d => {
                self.stream_data(c, ops)?;
            }
            // TagItem (Studio 0.748): u32 LE tag id.
            // Tag 13 follows the final JoinData chunk and marks initial data complete.
            0x10 => {
                if c.u32()? == 13 {
                    self.initial_data_complete = true;
                }
            }
            // StatsItem (Studio 0.748): versioned
            // network/server stats payload.
            0x11 => {
                Self::skip_stats_item(c)?;
            }
            // ResolveNewInstanceAtomicTreeItem (Studio 0.748):
            // child ref, new parent ref.
            0x13 => {
                let child = c.reference()?;
                let parent = c.reference()?;
                if let (Some(id), Some(parent)) = (child, parent) {
                    ops.push(RepOp::Reparent { id, parent });
                }
            }
            // StreamDataInfoItem (Studio 0.748): compact region
            // coordinate, two u32 fields, plus optional third u32 when
            // capability bit 56 is enabled.
            0x14 => {
                Self::skip_compact_region_coord(c)?;
                c.take(8)?;
                if self.stream_data_info_v2 {
                    c.u32()?;
                }
                ops.push(RepOp::Unhandled {
                    kind: ItemKind::StreamData,
                });
            }
            // ChangeAttributeItem (Studio 0.748): ref,
            // varint-length name, varint-length value (one attribute body:
            // type byte + value; the nil type 01 alone, or empty, removes it).
            0x15 => {
                let id = c.reference()?;
                let name = c.varstr()?;
                let n = c.varint()? as usize;
                let value = c.take(n)?.to_vec();
                // A trailing u8 (01 in every captured session, like
                // ChangeProperty's version byte). Without it the 01 was read
                // as a DeleteInstance whose "ref" was the next item: a bogus
                // delete per attribute change and the rest of the message lost.
                c.u8()?;
                if let Some(id) = id {
                    ops.push(RepOp::ChangeAttribute { id, name, value });
                }
            }
            // JoinDataItemV2 (Studio 0.748):
            // u8 stage, u32 clen, u32 size, zstd, [if stage != 1: u32,
            // varint count, count × (varint, varint)].
            0x16 => {
                let stage = c.u8()?;
                let clen = c.u32()? as usize;
                let size = c.u32()? as usize;
                let d = unzstd(c.take(clen)?, size)?;
                if stage != 1 {
                    c.u32()?;
                    let count = c.varint()? as usize;
                    if count > c.left() / 2 {
                        return Err(DecodeError::Incomplete);
                    }
                    for _ in 0..count {
                        c.varint()?;
                        c.varint()?;
                    }
                }
                if !self.began {
                    self.began = true;
                    ops.push(RepOp::JoinDataBegin);
                }
                if stage == 1 {
                    self.shared_strings(&d)?;
                } else {
                    let mut buf = self.stage_buf.remove(&stage).unwrap_or_default();
                    buf.extend_from_slice(&d);
                    let r = self.records(&buf, None, ops);
                    let used = match &r {
                        Ok(n) => *n,
                        // keep the stream aligned for the next chunk: drop this chunk's bytes
                        Err(_) => buf.len(),
                    };
                    buf.drain(..used);
                    self.stage_buf.insert(stage, buf);
                    r.map_err(|e| DecodeError::Bad(format!("stage {stage}: {e:?}")))?;
                }
            }
            // StreamPrefetchDataInfoItem (Studio 0.748):
            // compact region coord, u32, u32.
            0x18 => {
                Self::skip_compact_region_coord(c)?;
                c.take(8)?;
            }
            // StreamPrefetchRequestItem (Studio 0.748):
            // Vector3 (3×f32), f64, u32 (24 bytes).
            0x19 => {
                c.take(24)?;
            }
            // StreamReplicationFocusItem (Studio 0.748):
            // u32 id, compact region coord, u32, f32, u8, u8, u32.
            0x1a => {
                c.u32()?;
                Self::skip_compact_region_coord(c)?;
                c.take(14)?;
            }
            // DestroyInstanceItem (Studio 0.748):
            // u32 LE count, then count refs.
            0x1b => {
                let count = c.u32()? as usize;
                if count > c.left() {
                    return Err(DecodeError::Incomplete);
                }
                for _ in 0..count {
                    if let Some(id) = c.reference()? {
                        ops.push(RepOp::DeleteInstance { id });
                    }
                }
            }
            // StreamModelPersistentPlayerAddItem /
            // StreamModelPersistentPlayerRemoveItem: ref.
            0x1d | 0x1e => {
                c.reference()?;
            }
            // ClientQoSItem (Studio 0.748): u8 type, u8 subtype;
            // if type is 1 or 2: u32 LE length + bytes.
            0x1f => {
                let ty = c.u8()?;
                c.u8()?;
                if ty == 1 || ty == 2 {
                    let len = c.u32()? as usize;
                    c.take(len)?;
                }
            }
            // ReplicatorData (Studio 0.748): u32, u32 len, bytes.
            0x20 => {
                c.u32()?;
                let len = c.u32()? as usize;
                c.take(len)?;
            }
            // ISRTimestampItem (Studio 0.748): u32 LE timestamp.
            0x23 => {
                c.u32()?;
            }
            // StreamReplicationFocusRemoveItem (Studio 0.748): u32.
            0x24 => {
                c.u32()?;
            }
            // StreamPinDataItem (Studio 0.748):
            // embedded StreamDataItem (0x0d), then ref, u32, u8.
            0x25 => {
                self.stream_data(c, ops)?;
                c.reference()?;
                c.u32()?;
                c.u8()?;
            }
            // StreamReplicationFocusFrustumItem (Studio 0.748):
            // u32 focus id, 15×f32 LE frustum (64 bytes).
            0x27 => {
                c.take(64)?;
            }
            // StreamReplicationFocusFrustumRemoveItem (Studio 0.748): u32.
            0x28 => {
                c.u32()?;
            }
            // InstanceIntentItem (Studio 0.748):
            // u32 LE count, then count × (ref, u8).
            0x29 => {
                let count = c.u32()? as usize;
                if count > c.left() / 2 {
                    return Err(DecodeError::Incomplete);
                }
                for _ in 0..count {
                    c.reference()?;
                    c.u8()?;
                }
            }
            // An unknown kind: its length is unknown, so skip the rest of
            // this message instead of stopping the whole live session.
            k => {
                self.skipped.push(k);
                c.o = c.d.len();
            }
        }
        Ok(())
    }

    /// Compact region coordinate: `u8` mode (`0` → 3×i8,
    /// nonzero → 3×i32).
    fn skip_compact_region_coord(c: &mut Cur) -> R<()> {
        if c.u8()? == 0 {
            c.take(3)?;
        } else {
            c.take(12)?;
        }
        Ok(())
    }

    /// `JoinDataItem` body, shared by `0x0b`, `0x0d`, and `0x25`.
    fn join_data_body(&mut self, c: &mut Cur, ops: &mut Vec<RepOp>) -> R<()> {
        let count = c.u32()? as usize;
        if count != 0 {
            let clen = c.u32()? as usize;
            let size = c.u32()? as usize;
            let d = unzstd(c.take(clen)?, size)?;
            let used = self
                .records(&d, Some(count), ops)
                .map_err(|e| DecodeError::Bad(format!("instance batch: {e:?}")))?;
            self.shared_strings(&d[used..])?;
        }
        Ok(())
    }

    /// `StreamDataItem` body, shared by `0x0d` and `0x25`.
    fn stream_data(&mut self, c: &mut Cur, ops: &mut Vec<RepOp>) -> R<()> {
        self.join_data_body(c, ops)?;
        // Compact region marker list: bit 3 (0x08) means more
        // markers follow; `(flags & !8) == 0` carries a compact region coord.
        loop {
            let flags = c.u8()?;
            if (flags & !0x08) == 0 {
                Self::skip_compact_region_coord(c)?;
            }
            if flags & 0x08 == 0 {
                break;
            }
        }
        Ok(())
    }

    /// `StatsItem` (`0x11`, Studio 0.748).
    fn skip_stats_item(c: &mut Cur) -> R<()> {
        let skip_u32_str = |c: &mut Cur| -> R<()> {
            let len = c.u32()? as usize;
            c.take(len)?;
            Ok(())
        };
        let skip_zstd = |c: &mut Cur| -> R<()> {
            let clen = c.u32()? as usize;
            let _size = c.u32()?;
            c.take(clen)?;
            Ok(())
        };
        let version = c.u32()? as i32;
        if version >= 9 {
            c.take(8)?; // 2 × f32
        }
        if version >= 8 && c.u8()? != 0 {
            skip_zstd(c)?;
        }
        if version >= 7 {
            c.u32()?;
            skip_zstd(c)?;
        }
        if version >= 6 {
            c.take(8)?; // f64
            for _ in 0..2 {
                let count = c.u32()? as usize;
                if count > c.left() / 12 {
                    return Err(DecodeError::Incomplete);
                }
                for _ in 0..count {
                    skip_u32_str(c)?;
                    c.take(8)?; // f64
                }
            }
        }
        if version >= 3 && c.u8()? != 0 {
            c.take(24)?; // 6 × u32
        }
        while c.u8()? == 0 {
            skip_u32_str(c)?;
            c.take(12)?; // 3 × f32
        }
        while c.u8()? == 0 {
            skip_u32_str(c)?;
            c.take(8)?; // f32 + u32
        }
        c.take(20)?; // 5 × f32
        Ok(())
    }

    /// The inline copy of a shared string after its hash: `<hash> 00
    /// <varint len> <bytes>`. The server leaves it out when this peer already
    /// holds the string (a live place, 2026-11-21: a 37-byte frame echoing a Source our
    /// File Sync had just sent: hash only, then the next item), so it is read
    /// only when the next 16 bytes repeat the hash.
    fn inline_shared(&mut self, c: &mut Cur, expected: &[u8; 16]) -> R<()> {
        if c.left() < 16 || c.d[c.o..c.o + 16] != expected[..] {
            return Ok(());
        }
        c.take(16)?;
        if c.u8()? != 0 {
            return bad("shared-string encoding");
        }
        let len = c.varint()? as usize;
        self.shared.insert(*expected, c.take(len)?.to_vec());
        Ok(())
    }

    fn shared_strings(&mut self, d: &[u8]) -> R<()> {
        let mut c = Cur::new(d);
        while c.left() > 0 {
            let h: [u8; 16] = c.take(16)?.try_into().unwrap();
            c.u8()?;
            let n = c.varint()? as usize;
            self.shared.insert(h, c.take(n)?.to_vec());
        }
        Ok(())
    }

    /// Class-local props until `ff`.
    fn props(&self, ci: u16, c: &mut Cur) -> R<(String, Props)> {
        let s = self.schema()?;
        let cls = s
            .classes
            .get(usize::from(ci))
            .ok_or(DecodeError::Bad(format!("class {ci}")))?;
        let mut props = Vec::new();
        loop {
            let pi = c.u8()?;
            if pi == 0xff {
                break;
            }
            let p = cls
                .props
                .get(usize::from(pi))
                .ok_or(DecodeError::Bad(format!("{}: prop {pi}", cls.name)))?;
            let v = s.value_with(p.ty, c, false)?;
            props.push((p.name.clone(), v));
        }
        Ok((cls.name.clone(), props))
    }

    /// Parse complete `01 <u32 id> <u16 class> props ff <parent>` records
    /// (at most `max`); returns the bytes consumed (a trailing partial record
    /// is left).
    fn records(&self, d: &[u8], max: Option<usize>, ops: &mut Vec<RepOp>) -> R<usize> {
        let mut c = Cur::new(d);
        let mut done = 0;
        let mut n = 0;
        while c.left() > 0 && max.is_none_or(|m| n < m) {
            let r = (|| {
                let id = c
                    .reference()?
                    .ok_or_else(|| DecodeError::Bad("nil instance record".into()))?;
                let ci = c.u16()?;
                let (class, props) = self.props(ci, &mut c)?;
                let parent = c.reference()?;
                Ok(RepOp::NewInstance {
                    id,
                    class,
                    parent,
                    props,
                })
            })();
            match r {
                Ok(op) => {
                    ops.push(op);
                    done = c.o;
                    n += 1;
                }
                Err(DecodeError::Incomplete) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }
}

/// Maps the server's instance referents to rbx-dom [`Ref`]s as instances
/// arrive. Pending parents (a child seen before its parent) are not resolved
/// here; the model holds them.
#[derive(Default)]
pub struct RefMap {
    fwd: std::collections::HashMap<u64, Ref>,
    reverse: std::collections::HashMap<Ref, u64>,
    /// Instances whose referent another instance took (an id handed out
    /// twice): they still exist on the server, but can't be named there.
    lost: std::collections::HashSet<Ref>,
}

impl RefMap {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, server_id: u64) -> Option<Ref> {
        self.fwd.get(&server_id).copied()
    }
    pub fn insert(&mut self, server_id: u64, r: Ref) {
        if let Some(old_id) = self.reverse.insert(r, server_id) {
            self.fwd.remove(&old_id);
        }
        if let Some(old_ref) = self.fwd.insert(server_id, r) {
            if old_ref != r {
                self.reverse.remove(&old_ref);
                self.lost.insert(old_ref);
            }
        }
        self.lost.remove(&r);
    }
    /// Whether `r` lost its referent to another instance (see `lost`).
    pub fn lost(&self, r: Ref) -> bool {
        self.lost.contains(&r)
    }
    /// Marks `r` as lost without binding anything to its referent.
    pub fn mark_lost(&mut self, r: Ref) {
        self.remove_ref(r);
        self.lost.insert(r);
    }
    pub fn server_id(&self, reference: Ref) -> Option<u64> {
        self.reverse.get(&reference).copied()
    }
    /// Drops `r`'s server referent (both directions).
    pub fn remove_ref(&mut self, r: Ref) {
        if let Some(id) = self.reverse.remove(&r) {
            self.fwd.remove(&id);
        }
    }
    pub fn len(&self) -> usize {
        self.fwd.len()
    }
    pub fn is_empty(&self) -> bool {
        self.fwd.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_terrain_packet_decodes_and_checks_its_size() {
        // The first server terrain packet from the Studio 0.748 scratch join.
        let frame = [
            0x8d, 1, 0x81, 0x2f, 0, 0, 3, 15, 0, 0, 0, 6, 0, 0, 0,
            0x28, 0xb5, 0x2f, 0xfd, 0x20, 6, 0x31, 0, 0, 0xc1, 0xfc, 0xfe, 0xfc, 1, 0,
        ];
        let mut decoder = Decoder::new();
        let ops = decoder.decode_frame(frame[0], &frame[1..]);
        let [RepOp::Terrain { id, chunks }] = ops.as_slice() else { panic!("{ops:?}") };
        assert_eq!(*id, 0x2f81);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].origin, [-64, -32, -64]);
        assert_eq!((chunks[0].size_log2, chunks[0].lod), (1, 3));
        assert!(chunks[0].runs.is_empty());
        assert!(decoder.errors.is_empty());

        assert!(decoder.decode_frame(frame[0], &frame[1..frame.len() - 1]).is_empty());
        assert_eq!(decoder.errors.len(), 1);
    }

    #[test]
    fn globals_assign_the_client_namespace_after_flags() {
        assert_eq!(
            assigned_peer_scope(&[1, 0, 0, 0, 0, 0, 1, 0xac, 2, 0, 0, 0]).unwrap(),
            300
        );
        // Filtering on (every live game) still assigns the namespace.
        assert_eq!(assigned_peer_scope(&[1, 1, 0, 0, 0, 0, 1, 5]).unwrap(), 5);
        assert!(assigned_peer_scope(&[1, 0, 0, 0, 0, 0, 1, 1]).is_err());
    }

    #[test]
    fn peer_scoped_delete_preserves_following_marker() {
        let mut dec = decoder();
        let body = [1, 5, 0xf5, 0xce, 0x0e, 0, 0x10, 123, 0, 0, 0, 0];
        let ops = dec.decode_frame(0x83, &body);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        assert!(matches!(ops[0], RepOp::DeleteInstance { id } if id == (5u64 << 32) | 0xecef5));
    }

    #[test]
    fn destroy_instance_list_preserves_following_marker() {
        let mut dec = decoder();
        // 0x1b: count 3, refs (scope 1, 0x30aa), nil, (scope 5, 0xecef5); marker 123.
        let body = [
            0x1b, 3, 0, 0, 0, 1, 0xaa, 0x30, 0, 0, 0, 5, 0xf5, 0xce, 0x0e, 0, 0x10, 123, 0, 0,
            0, 0,
        ];
        let ops = dec.decode_frame(0x83, &body);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        let ids: Vec<u64> = ops
            .iter()
            .filter_map(|op| match op {
                RepOp::DeleteInstance { id } => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![0x30aa, (5u64 << 32) | 0xecef5]);
    }

    #[test]
    fn change_attribute_then_marker() {
        let mut dec = decoder();
        // 0x15: ref (1, 0x30aa), name "Hp", value: Float64 (type 0x03) 50.0.
        let mut body = vec![0x15, 1, 0xaa, 0x30, 0, 0, 2, b'H', b'p', 9, 0x03];
        body.extend_from_slice(&50f64.to_le_bytes());
        // trailing byte (01 on the wire), then marker 123
        body.extend_from_slice(&[0x01, 0x10, 123, 0, 0, 0, 0]);
        let ops = dec.decode_frame(0x83, &body);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        assert!(dec.skipped.is_empty(), "{:?}", dec.skipped);
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert!(matches!(&ops[0], RepOp::ChangeAttribute { id: 0x30aa, name, value } if name == "Hp" && value.len() == 9));
    }

    #[test]
    fn unknown_item_skips_rest_of_message_without_error() {
        let mut dec = decoder();
        let body = [1, 1, 0xaa, 0x30, 0, 0, 0x7e, 1, 2, 3];
        let ops = dec.decode_frame(0x83, &body);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        assert_eq!(dec.skipped, vec![0x7e]);
        assert!(matches!(ops[0], RepOp::DeleteInstance { id: 0x30aa }));
    }

    #[test]
    fn empty_stream_batch_regions_preserve_next_item() {
        let mut dec = decoder();
        // Zero records, full region coordinates, then a compact delta region.
        let mut body = vec![0x0d, 0, 0, 0, 0, 8, 1];
        for v in [-64i32, -64, -64] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        body.push(3);
        body.push(0x10);
        body.extend_from_slice(&123u32.to_le_bytes());
        body.push(0);
        dec.decode_frame(0x83, &body);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
    }

    fn varstr(out: &mut Vec<u8>, s: &str) {
        out.push(s.len() as u8);
        out.extend_from_slice(s.as_bytes());
    }

    /// A tiny schema in the 0x97 wire layout: enum `Material`; classes
    /// `Collaborator` (Status enum, Username string) and `Part`
    /// (CFrame, Name, Anchored); event `Part.Touched(Instance)`; name dict
    /// ["Baseplate"].
    fn schema_body() -> Vec<u8> {
        let mut d = vec![0x03, 0x01, 0, 0, 0, 0, 0, 0, 0];
        d.push(1);
        varstr(&mut d, "Material");
        d.push(11);
        d.extend_from_slice(&[2, 5, 1]);
        varstr(&mut d, "Collaborator");
        d.push(2);
        varstr(&mut d, "Status");
        d.extend_from_slice(&[0x07, 0x00, 0x00]);
        varstr(&mut d, "Username");
        d.push(0x02);
        d.extend_from_slice(&[0xff, 0xff, 0]);
        varstr(&mut d, "Part");
        d.push(3);
        varstr(&mut d, "CFrame");
        d.push(0x1a);
        varstr(&mut d, "Name");
        d.push(0x31);
        varstr(&mut d, "Anchored");
        d.push(0x09);
        d.extend_from_slice(&[0xff, 0xff, 1]);
        varstr(&mut d, "Touched");
        d.extend_from_slice(&[1, 0x1c, 0xff, 0xff]);
        d.push(1);
        varstr(&mut d, "rbxassetid://");
        d.push(1);
        varstr(&mut d, "Baseplate");
        d
    }

    fn zmsg(prefix: &[u8], d: &[u8], counts: bool) -> Vec<u8> {
        let z = zstd::bulk::compress(d, 3).unwrap();
        let mut m = prefix.to_vec();
        if counts {
            m.extend_from_slice(&(z.len() as u32).to_le_bytes());
            m.extend_from_slice(&(d.len() as u32).to_le_bytes());
        }
        m.extend_from_slice(&z);
        m
    }

    fn decoder() -> Decoder {
        let mut dec = Decoder::new();
        let m = zmsg(&[], &schema_body(), true);
        assert!(dec.decode_frame(0x97, &m).is_empty());
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        dec
    }

    #[test]
    fn parses_schema_and_global_indices() {
        let dec = decoder();
        let s = dec.schema.as_ref().unwrap();
        assert_eq!(s.classes.len(), 2);
        assert_eq!(s.global_props.len(), 5);
        assert_eq!(s.global_props[3], (1, 1)); // Part.Name
        assert_eq!(s.global_events, vec![(1, 0)]);
        assert_eq!(s.names, vec!["Baseplate"]);
    }

    /// The header's engine configuration word selects the net asset handle
    /// layout: bit 5 clear in an older join (`03 01`), set in a newer
    /// one (`03 21`).
    #[test]
    fn schema_header_selects_net_asset_handle_layout() {
        assert!(!decoder().schema.unwrap().net_asset_handles);
        let mut d = schema_body();
        d[1] = 0x21;
        assert!(Schema::parse(&d).unwrap().net_asset_handles);
        d[0] = 2;
        assert!(Schema::parse(&d).is_err());
    }

    fn handle(net_asset_handles: bool, bytes: &[u8]) -> (Option<NetAssetId>, usize) {
        let s = Schema { net_asset_handles, ..Schema::default() };
        let mut c = Cur::new(bytes);
        let v = s.net_asset_handle(&mut c).unwrap();
        (v, c.o)
    }

    /// SolidMeshHolder values as captured: the id (GUID word, varint index)
    /// is the same in both layouts; only the asset type moves.
    #[test]
    fn net_asset_handles_in_both_layouts() {
        let guid = 0xf000_0001_0000_0001;
        // Newer join (2026-11-18): schema type 0, id, then the next
        // property's index (0x0b).
        let newer = [0x00, 1, 0, 0, 0, 1, 0, 0, 0xf0, 0x05, 0x0b];
        assert_eq!(handle(true, &newer), (Some(NetAssetId { guid, index: 5 }), 10));
        assert_eq!(handle(true, &[0xff, 0xff, 0x03, 0x0b]), (None, 3));
        // Older join (2026-11-04): id, then u64 local type 3.
        let older = [1, 0, 0, 0, 1, 0, 0, 0xf0, 0x11, 3, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(handle(false, &older), (Some(NetAssetId { guid, index: 0x11 }), 17));
        let older_wide = [1, 0, 0, 0, 1, 0, 0, 0xf0, 0xc9, 0x13, 3, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(handle(false, &older_wide), (Some(NetAssetId { guid, index: 0x9c9 }), 18));
    }

    #[test]
    fn splits_rtcio_stream() {
        let mut b = vec![0x06, 0x01, 0x04, 0, 0, 0, 1];
        b.extend_from_slice(&[0x80, 0, 0, 2, 0x84, 0x01]);
        b.extend_from_slice(&[0x80, 0, 0, 1, 0x93]);
        let (ch, tag, msgs) = split_stream(&b).unwrap();
        assert_eq!((ch, tag), (4, [0, 0, 0, 1]));
        assert_eq!(msgs, vec![&[0x84u8, 0x01][..], &[0x93u8][..]]);
    }

    /// Instance records split across two 0x16 chunks (as on the wire), with
    /// a full-matrix CFrame, a dictionary Name and a plain Name.
    #[test]
    fn initial_stream_records_cross_chunks() {
        let mut dec = decoder();
        let mut recs = vec![1, 0x80, 0x65, 0, 0, 1, 0]; // id 0x6580, class Part
        recs.push(0); // CFrame
        for f in [1.0f32, 2.0, 3.0] {
            recs.extend_from_slice(&f.to_le_bytes());
        }
        recs.push(0); // full rotation follows
        for f in [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
            recs.extend_from_slice(&f.to_le_bytes());
        }
        recs.extend_from_slice(&[1, 0x80, 2, 1, 0xff, 1, 13, 0, 0, 0]); // Name=dict[0], Anchored, parent 13
        recs.extend_from_slice(&[1, 0x81, 0x65, 0, 0, 1, 0, 1, 3]);
        recs.extend_from_slice(b"Pad");
        recs.extend_from_slice(&[0xff, 1, 0x80, 0x65, 0, 0]);
        let (a, b) = recs.split_at(30);
        let mut ops = Vec::new();
        for part in [a, b] {
            let mut m = zmsg(&[0x16, 0x04], part, true);
            m.extend_from_slice(&932u32.to_le_bytes());
            // A real populated JoinDataItemV2 metadata trailer. Both chunks
            // must leave the following item boundary intact.
            for value in [2, 91032, 197, 83668, 602] {
                crate::handshake::put_varint(&mut m, value);
            }
            m.push(0);
            ops.extend(dec.decode_frame(0x83, &m));
        }
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        let news: Vec<_> = ops
            .iter()
            .filter_map(|o| match o {
                RepOp::NewInstance {
                    id,
                    class,
                    parent,
                    props,
                } => Some((*id, class.clone(), *parent, props.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(news.len(), 2);
        assert_eq!(news[0].0, 0x6580);
        assert_eq!(news[0].2, Some(13));
        assert_eq!(
            news[0].3[1],
            ("Name".into(), PropValue::String("Baseplate".into()))
        );
        assert_eq!(news[0].3[2], ("Anchored".into(), PropValue::Bool(true)));
        match &news[0].3[0].1 {
            PropValue::CFrame { pos, rot } => {
                assert_eq!(*pos, [1.0, 2.0, 3.0]);
                assert_eq!(rot[4], 1.0);
            }
            v => panic!("{v:?}"),
        }
        assert_eq!(
            news[1].3[0],
            ("Name".into(), PropValue::String("Pad".into()))
        );
        assert_eq!(news[1].2, Some(0x6580));
    }

    /// The Collaborator.Status change from the capture, re-indexed to the
    /// test schema: `03 <ref> <u16 global prop> 01 <enum>`, then an event.
    #[test]
    fn property_change_event_and_batch() {
        let mut dec = decoder();
        let mut m = vec![0x03, 0x01, 0x96, 0x38, 0x05, 0x00, 0x00, 0x00, 0x01, 0x01];
        m.extend_from_slice(&[
            0x07, 0x01, 0x80, 0x65, 0, 0, 0x00, 0x00, 0x01, 0x81, 0x65, 0, 0,
        ]);
        // a batch holding one new instance: Collaborator with Username "testuser1"
        m.extend_from_slice(&[
            0x10, 1, 0, 0, 0, 0x02, 0x01, 0x96, 0x38, 0x05, 0x00, 0x00, 0x00, 0xff, 0x01, 9,
        ]);
        m.extend_from_slice(b"testuser1");
        m.extend_from_slice(&[0xff, 0x01, 0x94, 0x36, 0x05, 0x00]);
        m.extend_from_slice(&[0x13, 0x01, 0x81, 0x65, 0, 0, 0x01, 0x80, 0x65, 0, 0]); // reparent
        m.push(0x00);
        let ops = dec.decode_frame(0x83, &m);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        assert!(matches!(ops[0], RepOp::Presence { user_id: 0x053896 }));
        assert!(
            matches!(&ops[1], RepOp::ChangeProperty { id: 0x053896, name, value: PropValue::Enum(1) } if name == "Status")
        );
        assert!(
            matches!(&ops[2], RepOp::Event { id: 0x6580, name, args } if name == "Part.Touched" && args == &vec![PropValue::Ref(Some(0x6581))])
        );
        assert!(
            matches!(&ops[3], RepOp::NewInstance { id: 0x053896, class, parent: Some(0x053694), props }
            if class == "Collaborator" && props[0].1 == PropValue::String("testuser1".into()))
        );
        assert!(matches!(
            ops[4],
            RepOp::Reparent {
                id: 0x6581,
                parent: 0x6580
            }
        ));
    }

    #[test]
    fn new_object_early_source_group_includes_inline_shared_string() {
        let mut dec = decoder();
        dec.schema.as_mut().unwrap().classes[1]
            .props
            .push(SchemaProp {
                name: "Source".into(),
                ty: 0x06,
            });
        let hash = [0x55; 16];
        let mut body = vec![2, 1, 7, 0, 0, 0, 1, 0, 3];
        body.extend_from_slice(&hash);
        body.extend_from_slice(&[0xff, 1, 4]);
        body.extend_from_slice(b"Code");
        body.push(0xff);
        body.extend_from_slice(&hash);
        body.extend_from_slice(&[0, 3]);
        body.extend_from_slice(b"abc");
        body.extend_from_slice(&[0, 0]); // nil parent, end of items
        let ops = dec.decode_frame(0x83, &body);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        assert!(matches!(&ops[0], RepOp::NewInstance { id: 7, props, .. }
            if props.contains(&("Source".into(), PropValue::CachedSharedString {hash: [0x55; 16], bytes: b"abc".to_vec()}))
                && props.contains(&("Name".into(), PropValue::String("Code".into())))));
    }

    #[test]
    fn stream_region_metadata_does_not_consume_following_marker() {
        let mut dec = decoder();
        let ops = dec.decode_frame(
            0x83,
            &[
                0x14, 0, 0, 0, 0, 63, 0, 1, 0, 0, 0, 0, 0, 0x23, 1, 0, 0, 0, 0,
            ],
        );
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        assert!(matches!(
            ops[0],
            RepOp::Unhandled {
                kind: ItemKind::StreamData
            }
        ));
    }

    #[test]
    fn content_object_kind_is_consumed() {
        let s = Schema::default();
        // kind 2, reference scope 1, index 0x1234, then a following byte.
        let d = [2u8, 1, 0x34, 0x12, 0, 0, 0xaa];
        let mut c = Cur::new(&d);
        assert!(matches!(s.value(0x3d, &mut c), Ok(PropValue::Raw(_))));
        assert_eq!(c.o, 6);
        // nil object
        let d = [2u8, 0];
        let mut c = Cur::new(&d);
        assert!(s.value(0x3d, &mut c).is_ok());
        assert_eq!(c.o, 2);
    }

    #[test]
    fn bad_input_is_reported_not_panicking() {
        let mut dec = decoder();
        assert!(dec.decode_frame(0x83, &[0x7e, 0x00]).is_empty());
        assert!(dec.errors.is_empty());
        assert_eq!(dec.skipped, vec![0x7e]);
        let mut dec = Decoder::new();
        dec.decode_frame(0x83, &[0x03, 0x01, 0, 0, 0, 0, 0, 0, 1, 1, 0]);
        assert!(dec.errors[0].contains("no schema"));
    }

    /// Helper: append TagItem 13 (`0x10`, u32 13) + `0x00` sentinel, decode,
    /// and assert zero errors, zero skipped kinds, and `initial_data_complete`
    /// (proving the preceding item's length was exact to the byte).
    fn decode_then_marker(dec: &mut Decoder, mut item_bytes: Vec<u8>) -> Vec<RepOp> {
        item_bytes.extend_from_slice(&[0x10, 13, 0, 0, 0, 0]);
        let ops = dec.decode_frame(0x83, &item_bytes);
        assert!(dec.errors.is_empty(), "errors: {:?}", dec.errors);
        assert!(dec.skipped.is_empty(), "skipped: {:?}", dec.skipped);
        assert!(
            dec.initial_data_complete,
            "trailing 0x10 marker 13 was not reached (item length mismatch)"
        );
        ops
    }

    #[test]
    fn item_0x01_delete_instance_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x01, 1, 0x22, 0x11, 0, 0]);
        assert!(matches!(ops.as_slice(), [RepOp::DeleteInstance { id: 0x1122 }]));
    }

    #[test]
    fn item_0x02_new_object_exact_length() {
        let mut dec = decoder();
        let mut b = vec![0x02, 1, 0x10, 0, 0, 0, 1, 0, 0xff, 1, 3];
        b.extend_from_slice(b"Box");
        b.extend_from_slice(&[0xff, 1, 13, 0, 0, 0]);
        let ops = decode_then_marker(&mut dec, b);
        assert!(matches!(ops.as_slice(), [RepOp::NewInstance { id: 0x10, class, parent: Some(13), .. }] if class == "Part"));
    }

    #[test]
    fn item_0x03_change_property_exact_length() {
        let mut dec = decoder();
        // global prop 4 = Part.Anchored (bool)
        let ops = decode_then_marker(&mut dec, vec![0x03, 1, 0x10, 0, 0, 0, 4, 0, 1, 1]);
        assert!(matches!(ops.as_slice(), [RepOp::ChangeProperty { id: 0x10, name, value: PropValue::Bool(true) }] if name == "Anchored"));
    }

    #[test]
    fn item_0x04_marker_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x04, 0x78, 0x56, 0x34, 0x12]);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x05_ping_flags_exact_length() {
        let mut dec = decoder();
        // flags = 0x04 | 0x08 (has u32 + trace context: has_trace=1, len=5, 5 bytes)
        let mut b = vec![0x05, 0x0c];
        b.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        b.extend_from_slice(&100u32.to_le_bytes());
        b.extend_from_slice(&200u32.to_le_bytes());
        b.extend_from_slice(&300u32.to_le_bytes());
        b.push(1); // mode < 2
        b.extend_from_slice(&400u32.to_le_bytes()); // flags & 4
        b.push(1); // has_trace = 1
        b.extend_from_slice(&5u32.to_le_bytes());
        b.extend_from_slice(&[1, 2, 3, 4, 5]);
        let ops = decode_then_marker(&mut dec, b);
        assert!(matches!(ops.as_slice(), [RepOp::Unhandled { kind: ItemKind::Ping }]));
    }

    #[test]
    fn item_0x06_ping_back_exact_length() {
        let mut dec = decoder();
        dec.ping_extra_float = true;
        // flags = 0x01 | 0x10 (6×u32 + optional f32 when cap bit 51 enabled)
        let mut b = vec![0x06, 0x11];
        b.extend_from_slice(&1u64.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 24]); // 6 × u32
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&4u32.to_le_bytes());
        b.push(0); // mode
        b.extend_from_slice(&1.5f32.to_le_bytes()); // cap51 && flags&0x10
        let ops = decode_then_marker(&mut dec, b);
        assert!(matches!(ops.as_slice(), [RepOp::Unhandled { kind: ItemKind::Ping }]));
    }

    #[test]
    fn item_0x07_event_invocation_exact_length() {
        let mut dec = decoder();
        // global event 0 = Part.Touched(Instance)
        let ops = decode_then_marker(
            &mut dec,
            vec![0x07, 1, 0x10, 0, 0, 0, 0, 0, 1, 0x11, 0, 0, 0],
        );
        assert!(matches!(ops.as_slice(), [RepOp::Event { id: 0x10, name, .. }] if name == "Part.Touched"));
    }

    #[test]
    fn item_0x09_pmc_config_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x09, 4]);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x0b_join_data_exact_length() {
        let mut dec = decoder();
        let rec = vec![1, 0x33, 0, 0, 0, 1, 0, 1, 0x80, 0xff, 1, 13, 0, 0, 0];
        let mut b = vec![0x0b];
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&zmsg(&[], &rec, true));
        let ops = decode_then_marker(&mut dec, b);
        assert!(matches!(ops[0], RepOp::JoinDataBegin));
        assert!(matches!(&ops[1], RepOp::NewInstance { id: 0x33, class, .. } if class == "Part"));
    }

    #[test]
    fn item_0x0c_delete_pure_object_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x0c, 1, 0x44, 0, 0, 0, 0xac, 0x02]);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x0d_stream_data_exact_length() {
        let mut dec = decoder();
        // 0 records, two region markers: (flags 0x08 -> mode 0 compact 3B + more), (flags 0x05 -> non-zero mode, stop)
        let b = vec![0x0d, 0, 0, 0, 0, 0x08, 0, 1, 2, 3, 0x05];
        let ops = decode_then_marker(&mut dec, b);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x10_tag_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x10, 7, 0, 0, 0]);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x11_stats_exact_length() {
        let mut dec = decoder();
        let mut b = vec![0x11];
        b.extend_from_slice(&9u32.to_le_bytes()); // version 9
        b.extend_from_slice(&[0u8; 8]); // >=9: 2×f32
        b.push(1); // >=8: has_block = 1
        b.extend_from_slice(&zmsg(&[], b"\x00", true));
        b.extend_from_slice(&42u32.to_le_bytes()); // >=7: u32 + zstd
        b.extend_from_slice(&zmsg(&[], b"\x00", true));
        b.extend_from_slice(&1.0f64.to_le_bytes()); // >=6: f64
        b.extend_from_slice(&1u32.to_le_bytes()); // list1 len=1
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(b"k1");
        b.extend_from_slice(&2.0f64.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // list2 len=0
        b.push(1); // >=3: has_v3 = 1 -> 6×u32
        b.extend_from_slice(&[0u8; 24]);
        // loop 1: one entry then terminator 1
        b.push(0);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.push(b'a');
        b.extend_from_slice(&[0u8; 12]);
        b.push(1);
        // loop 2: one entry then terminator 1
        b.push(0);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.push(b'b');
        b.extend_from_slice(&[0u8; 8]);
        b.push(1);
        // 5 × f32 tail
        b.extend_from_slice(&[0u8; 20]);
        let ops = decode_then_marker(&mut dec, b);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x13_resolve_atomic_tree_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x13, 1, 5, 0, 0, 0, 1, 6, 0, 0, 0]);
        assert!(matches!(ops.as_slice(), [RepOp::Reparent { id: 5, parent: 6 }]));
    }

    #[test]
    fn item_0x14_stream_data_info_exact_length() {
        let mut dec = decoder();
        dec.stream_data_info_v2 = true;
        // mode 1 (3×i32 = 12B) + 2×u32 + optional 3rd u32 (cap bit 56)
        let mut b = vec![0x14, 1];
        b.extend_from_slice(&[0u8; 12]);
        b.extend_from_slice(&[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0]);
        let ops = decode_then_marker(&mut dec, b);
        assert!(matches!(ops.as_slice(), [RepOp::Unhandled { kind: ItemKind::StreamData }]));
    }

    #[test]
    fn item_0x15_change_attribute_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x15, 1, 9, 0, 0, 0, 1, b'x', 2, 0x02, 1, 0x01]);
        assert!(matches!(ops.as_slice(), [RepOp::ChangeAttribute { id: 9, name, value }] if name == "x" && value == &[0x02, 1]));
    }

    #[test]
    fn item_0x16_join_data_v2_exact_length() {
        let mut dec = decoder();
        let mut b = zmsg(&[0x16, 1], &[], true);
        let ops = decode_then_marker(&mut dec, b.drain(..).collect());
        assert!(matches!(ops.as_slice(), [RepOp::JoinDataBegin]));
    }

    #[test]
    fn item_0x18_stream_prefetch_data_info_exact_length() {
        let mut dec = decoder();
        let b = vec![0x18, 0, 1, 2, 3, 10, 0, 0, 0, 20, 0, 0, 0];
        let ops = decode_then_marker(&mut dec, b);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x19_stream_prefetch_request_exact_length() {
        let mut dec = decoder();
        let mut b = vec![0x19];
        b.extend_from_slice(&[0u8; 24]);
        let ops = decode_then_marker(&mut dec, b);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x1a_stream_replication_focus_exact_length() {
        let mut dec = decoder();
        let mut b = vec![0x1a, 1, 0, 0, 0, 0, 1, 2, 3];
        b.extend_from_slice(&[0u8; 14]);
        let ops = decode_then_marker(&mut dec, b);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x1b_destroy_instance_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x1b, 1, 0, 0, 0, 1, 0x77, 0, 0, 0]);
        assert!(matches!(ops.as_slice(), [RepOp::DeleteInstance { id: 0x77 }]));
    }

    #[test]
    fn item_0x1d_stream_persistent_player_add_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x1d, 1, 0x20, 0, 0, 0]);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x1e_stream_persistent_player_remove_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x1e, 1, 0x20, 0, 0, 0]);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x1f_client_qos_exact_length() {
        let mut dec = decoder();
        let mut b = vec![0x1f, 1, 0, 3, 0, 0, 0];
        b.extend_from_slice(b"qos");
        let ops = decode_then_marker(&mut dec, b);
        assert!(ops.is_empty());
        // type 0 has no trailing string
        let mut dec2 = decoder();
        let ops2 = decode_then_marker(&mut dec2, vec![0x1f, 0, 7]);
        assert!(ops2.is_empty());
    }

    #[test]
    fn item_0x20_replicator_data_exact_length() {
        let mut dec = decoder();
        let mut b = vec![0x20, 1, 0, 0, 0, 4, 0, 0, 0];
        b.extend_from_slice(b"data");
        let ops = decode_then_marker(&mut dec, b);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x23_isr_timestamp_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x23, 0x10, 0x20, 0x30, 0x40]);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x24_stream_replication_focus_remove_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x24, 9, 0, 0, 0]);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x25_stream_pin_data_exact_length() {
        let mut dec = decoder();
        let rec = vec![1, 0x55, 0, 0, 0, 1, 0, 1, 0x80, 0xff, 1, 13, 0, 0, 0];
        let mut b = vec![0x25];
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&zmsg(&[], &rec, true));
        b.push(4); // region marker (mode != 0, stop)
        b.extend_from_slice(&[1, 0x55, 0, 0, 0]); // pinned ref
        b.extend_from_slice(&7u32.to_le_bytes()); // pin id
        b.push(1); // bool
        let ops = decode_then_marker(&mut dec, b);
        assert!(matches!(ops.as_slice(), [RepOp::NewInstance { id: 0x55, .. }]));
    }

    #[test]
    fn item_0x27_stream_replication_focus_frustum_exact_length() {
        let mut dec = decoder();
        let mut b = vec![0x27];
        b.extend_from_slice(&[0u8; 64]); // u32 + 15×f32
        let ops = decode_then_marker(&mut dec, b);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x28_stream_replication_focus_frustum_remove_exact_length() {
        let mut dec = decoder();
        let ops = decode_then_marker(&mut dec, vec![0x28, 3, 0, 0, 0]);
        assert!(ops.is_empty());
    }

    #[test]
    fn item_0x29_instance_intent_exact_length() {
        let mut dec = decoder();
        let b = vec![0x29, 2, 0, 0, 0, 1, 0x10, 0, 0, 0, 1, 0, 2];
        let ops = decode_then_marker(&mut dec, b);
        assert!(ops.is_empty());
    }
}
