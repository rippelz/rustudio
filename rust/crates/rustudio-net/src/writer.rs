//! Client replication messages, indexed by the negotiated server schema.
use crate::handshake::{put_varint, put_varstr, zigzag};
use crate::replicator::{PropValue, Schema};

/// Encode one property change using the class's actual schema index.
/// Unsupported value types are rejected before any bytes are sent.
pub fn property(
    schema: &Schema,
    id: u64,
    class: &str,
    name: &str,
    value: &PropValue,
) -> Result<Vec<u8>, String> {
    property_with(schema, id, class, name, value, &mut SharedSent::default())
}

/// Shared-string hashes whose contents this connection already sent. The
/// contents go on the wire only the first time a hash is written; later
/// items carry the bare hash, as the server's own stream does
/// (`Decoder::inline_shared`). Sending them again reset the session (0x104):
/// a second pasted rig whose Animate / Respawn Source matched the first's
/// (a test place, 2026-11-21).
pub type SharedSent = std::collections::HashSet<[u8; 16]>;

/// [`property`], with this connection's [`SharedSent`].
pub fn property_with(
    schema: &Schema,
    id: u64,
    class: &str,
    name: &str,
    value: &PropValue,
    sent: &mut SharedSent,
) -> Result<Vec<u8>, String> {
    // The wire renames apply per class (a Part's Color is Color3uint8, a
    // light's is Color; a Part's Size is size, a GUI's is Size): the renamed
    // property is used when the class has it, else the editor's name.
    let find = |name: &str| {
        schema.global_props.iter().enumerate().find_map(|(global, &(ci, pi))| {
            let cls = &schema.classes[ci as usize];
            let prop = &cls.props[pi as usize];
            (cls.name == class && prop.name == name).then_some((global, prop.ty))
        })
    };
    let (global, ty) = find(wire_name(name))
        .or_else(|| find(name))
        .ok_or_else(|| format!("{class}.{name} is not replicated by this server"))?;

    let global = u16::try_from(global).map_err(|_| "Property index exceeds the protocol range")?;
    let mut out = vec![0x83, 3];
    put_reference(&mut out, id);
    out.extend_from_slice(&global.to_le_bytes());
    out.push(0); // Client-origin version byte (verified against Studio c2s).
    let value = prepare_value(ty, value)?;
    encode_value(&mut out, schema, ty, &value, true)?;
    append_shared(&mut out, &value, sent);
    out.push(0); // End of item list.
    Ok(out)
}

/// Invoke a replicated event using the server's negotiated argument types.
pub fn event(schema: &Schema, id: u64, class: &str, name: &str, args: &[PropValue]) -> Result<Vec<u8>, String> {
    let (global, descriptor)=schema.global_events.iter().enumerate().find_map(|(global,&(ci,ei))| {
        let cls=&schema.classes[ci as usize];let event=&cls.events[ei as usize];
        (cls.name==class && event.name==name).then_some((global,event))
    }).ok_or_else(|| format!("{class}.{name} is not replicated by this server"))?;
    if args.len()!=descriptor.arg_types.len() {return Err(format!("{class}.{name} argument count"));}
    let mut out=vec![0x83,7];put_reference(&mut out,id);
    out.extend_from_slice(&u16::try_from(global).map_err(|_| "event index range")?.to_le_bytes());
    for (value,ty) in args.iter().zip(&descriptor.arg_types) {encode_value(&mut out,schema,*ty,value,true)?;}
    out.push(0);Ok(out)
}

fn encode_tagged(out: &mut Vec<u8>, schema: &Schema, value: &PropValue, compress_content: bool) -> Result<(), String> {
    let ty=match value {
        PropValue::Bool(_)=>0x09,PropValue::Int(_) | PropValue::Int64(_)=>0x2c,PropValue::Float(_)=>0x0c,PropValue::Float32(_)=>0x0b,
        PropValue::String(_)=>0x02,PropValue::BinaryString(_)=>0x08,PropValue::Array(_)=>0x1e,PropValue::Dictionary(_)=>0x1f,
        PropValue::Ref(_)=>0x1c,PropValue::Enum(_)=>0x07,
        PropValue::Color3(_)=>0x13,PropValue::Color3uint8(_)=>0x14,
        PropValue::Vector3(_)=>0x16,PropValue::Vector2(_)=>0x15,PropValue::CFrame {..}=>0x1a,
        PropValue::Typed { ty, .. } => *ty,
        _=>return Err("Unsupported event container value".into()),
    };
    out.push(ty);
    // Container elements with tag 0x07 carry a u16 LE enum id before the varint.
    if ty == 0x07 {
        out.extend_from_slice(&0xffffu16.to_le_bytes());
    }
    encode_value(out,schema,ty,value,compress_content)
}

pub fn wire_name(name: &str) -> &str {
    match name {
        "Attributes"=>"AttributesReplicate",
        "Size" => "size",
        "Shape" => "shape",
        "FormFactor" => "formFactorRaw",
        "Color" => "Color3uint8",
        "MaterialVariant" => "MaterialVariantSerialized",
        "CollisionGroup" => "CollisionGroupReplicate",
        "RenderFidelity" => "RenderFidelityReplicate",
        "MeshId" => "MeshContent",
        "TextureID" => "TextureContent",
        // Script capabilities: the reflection DB's canonical Sandboxed is
        // DefinesCapabilities in files and on the wire (the server's own
        // NewObject carries DefinesCapabilities = true).
        "Sandboxed" => "DefinesCapabilities",
        other => other,
    }
}

fn prepare_value(ty: u8, value: &PropValue) -> Result<PropValue, String> {
    use blake2::digest::{Update,VariableOutput};
    let bytes=match (ty,value) {
        (0x06 | 0x2f,PropValue::String(source))=>Some(source.as_bytes()),
        (0x06 | 0x2e | 0x2f,PropValue::SharedString(bytes))=>Some(bytes.as_slice()),
        _=>None,
    };
    if let Some(bytes)=bytes {
        let mut hash=[0;16];let mut state=blake2::Blake2bVar::new(16).map_err(|_| "shared hash size")?;
        state.update(bytes);state.finalize_variable(&mut hash).map_err(|_| "shared hash output")?;
        Ok(PropValue::CachedSharedString {hash,bytes:bytes.to_vec()})
    } else {Ok(value.clone())}
}

fn append_shared(out: &mut Vec<u8>, value: &PropValue, sent: &mut SharedSent) {
    if let PropValue::CachedSharedString {hash,bytes}=value {
        if sent.insert(*hash) {
            out.extend_from_slice(hash);out.push(0);put_varstr(out,bytes);
        }
    }
}

/// Encode a ContentId string.
fn put_content_id(out: &mut Vec<u8>, schema: &Schema, uri: &str, compress: bool) {
    if !compress {
        put_varstr(out, uri.as_bytes());
        return;
    }
    // Match the highest-index non-empty prefix (1..len, descending, as Studio does).
    for idx in (1..schema.content_prefixes.len().min(128)).rev() {
        let prefix = &schema.content_prefixes[idx];
        if !prefix.is_empty() {
            if let Some(rest) = uri.strip_prefix(prefix.as_str()) {
                if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                    if let Ok(id) = rest.parse::<i64>() {
                        out.push(((idx as u8) << 1) | 1);
                        put_varint(out, zigzag(id));
                        return;
                    }
                }
                out.push((idx as u8) << 1);
                put_varstr(out, rest.as_bytes());
                return;
            }
        }
    }
    out.push(0);
    put_varstr(out, uri.as_bytes());
}

pub(crate) fn encode_value(
    out: &mut Vec<u8>,
    schema: &Schema,
    ty: u8,
    value: &PropValue,
    compress_content: bool,
) -> Result<(), String> {
    if let PropValue::Typed { bytes, .. } = value {
        return encode_value(out, schema, ty, &PropValue::Raw(bytes.clone()), compress_content);
    }
    if let PropValue::Float32(v) = value {
        return encode_value(out, schema, ty, &PropValue::Float(f64::from(*v)), compress_content);
    }
    match (ty, value) {
        (0x03, PropValue::Raw(bytes)) if bytes.is_empty() => {}
        (0x1d | 0x1e, PropValue::Array(values)) => {
            put_varint(out, values.len() as u64);
            for value in values {
                encode_tagged(out, schema, value, compress_content)?;
            }
        }
        (0x1f | 0x20, PropValue::Dictionary(values)) => {
            put_varint(out, values.len() as u64);
            for (key, value) in values {
                put_varstr(out, key.as_bytes());
                encode_tagged(out, schema, value, compress_content)?;
            }
        }
        (0x09, PropValue::Bool(v)) => out.push(u8::from(*v)),
        (0x0a | 0x2c, PropValue::Int(v) | PropValue::Int64(v)) => put_varint(out, zigzag(*v)),
        (0x07, PropValue::Enum(v)) => put_varint(out, u64::from(*v)),
        (0x0b, PropValue::Float(v)) => out.extend_from_slice(&(*v as f32).to_le_bytes()),
        (0x0c, PropValue::Float(v)) => out.extend_from_slice(&v.to_le_bytes()),
        (0x02, PropValue::BinaryString(v)) => put_varstr(out, v),
        (0x02, PropValue::String(v)) => put_varstr(out, v.as_bytes()),
        (0x21, PropValue::BinaryString(v)) => {
            let s = String::from_utf8_lossy(v);
            put_content_id(out, schema, &s, compress_content);
        }
        (0x21, PropValue::String(v)) => put_content_id(out, schema, v, compress_content),
        // 0x31 Name serializer: dictionary match (0x80|idx or 0xff + varint(idx-0x7f)),
        // else len < 0x7f -> u8(len) + bytes, else 0x7f + varstr(bytes).
        (0x31, PropValue::String(v)) => {
            if let Some(idx) = schema.names.iter().position(|n| n == v) {
                if idx < 0x7f {
                    out.push(0x80 | (idx as u8));
                } else {
                    out.push(0xff);
                    put_varint(out, (idx - 0x7f) as u64);
                }
            } else if v.len() < 0x7f {
                out.push(v.len() as u8);
                out.extend_from_slice(v.as_bytes());
            } else {
                out.push(0x7f);
                put_varstr(out, v.as_bytes());
            }
        }
        (0x14, PropValue::Color3uint8(v)) => out.extend_from_slice(v),
        (0x14, PropValue::Color3(v)) => {
            for c in v {
                out.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
        (0x13, PropValue::Color3uint8(v)) => {
            for c in v {
                out.extend_from_slice(&(f32::from(*c) / 255.0).to_le_bytes());
            }
        }
        (0x3d, PropValue::String(uri)) => {
            if uri.is_empty() {
                out.push(0);
            } else {
                out.push(1);
                put_content_id(out, schema, uri, compress_content);
            }
        }
        (0x13, PropValue::Color3(v)) | (0x16, PropValue::Vector3(v)) => {
            for f in v {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        (0x15, PropValue::Vector2(v)) => {
            for f in v {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        (0x1a, PropValue::CFrame { pos, rot }) => {
            for f in pos {
                out.extend_from_slice(&f.to_le_bytes());
            }
            out.push(0); // Full matrix, avoiding compressed-rotation assumptions.
            for f in rot {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        (0x1c | 0x45, PropValue::Ref(reference)) => match reference {
            Some(id) => {
                put_reference(out, *id);
            }
            None => out.push(0),
        },
        (0x06 | 0x2e | 0x2f, PropValue::CachedSharedString { hash, .. } | PropValue::SharedHash(hash)) => {
            out.extend_from_slice(hash)
        }
        (0x08, PropValue::Raw(bytes) | PropValue::BinaryString(bytes)) => {
            put_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        (0x38, PropValue::BinaryString(bytes) | PropValue::Raw(bytes)) => {
            // Uncompressed buffer: comp_len = 0, decomp_len = len, raw bytes.
            put_varint(out, 0);
            put_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        (0x12, PropValue::Int(v)) => out.extend_from_slice(
            &u16::try_from(*v)
                .map_err(|_| "u16 property range")?
                .to_le_bytes(),
        ),
        (0x12, PropValue::Raw(bytes)) if bytes.len() == 2 => out.extend_from_slice(bytes),
        (0x22, PropValue::Int(v)) => put_varint(
            out,
            u64::try_from(*v).map_err(|_| "unsigned property range")?,
        ),
        (0x23 | 0x26, PropValue::Raw(bytes)) => {
            let width = if ty == 0x23 { 12 } else { 20 };
            if bytes.len() % width != 0 {
                return Err("sequence byte length".into());
            }
            out.extend_from_slice(&((bytes.len() / width) as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        (0x29, PropValue::Raw(bytes)) => {
            out.push(match bytes.len() {
                0 => 0,
                20 => 1,
                24 => 3,
                _ => return Err("physical properties byte length".into()),
            });
            out.extend_from_slice(bytes);
        }
        (0x32, PropValue::Raw(bytes)) if bytes.is_empty() => out.push(0),
        (0x32, PropValue::Raw(bytes)) if bytes.len() == 48 => {
            // Typed 0x32 stores [pos(12B), rot(36B)]; wire order is 0x80, rot(36B), pos(12B).
            out.push(0x80);
            out.extend_from_slice(&bytes[12..48]);
            out.extend_from_slice(&bytes[0..12]);
        }
        (0x32, PropValue::CFrame { pos, rot }) => {
            out.push(0x80);
            for f in rot {
                out.extend_from_slice(&f.to_le_bytes());
            }
            for f in pos {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        (0x3b | 0x3c, PropValue::NetAssetHandle(handle)) => {
            if schema.net_asset_handles {
                match handle {
                    None => put_varint(out, 0xffff),
                    Some(id) => {
                        put_varint(out, 0);
                        out.extend_from_slice(&id.guid.to_le_bytes());
                        put_varint(out, u64::from(id.index));
                    }
                }
            } else if let Some(id) = handle {
                out.extend_from_slice(&id.guid.to_le_bytes());
                put_varint(out, u64::from(id.index));
                out.extend_from_slice(&3u64.to_le_bytes());
            } else {
                return Err("empty net asset handle requires schema config bit 5".into());
            }
        }
        (
            0x0d | 0x0e | 0x0f | 0x10 | 0x11 | 0x18 | 0x19 | 0x1d..=0x20 | 0x24 | 0x25 | 0x27
            | 0x28 | 0x2a | 0x2b | 0x30 | 0x34 | 0x35 | 0x36 | 0x37 | 0x39 | 0x3a | 0x3b | 0x3c
            | 0x3e | 0x3f | 0x41..=0x44 | 0x46 | 0x47,
            PropValue::Raw(bytes),
        ) => out.extend_from_slice(bytes),
        _ => return Err(format!("Wire value type {ty:#x} is not supported yet")),
    }
    Ok(())
}

/// NewObjectItem with schema-indexed properties and an empty early group.
/// Every property must be sent; see [`create_partial`] to drop the ones this
/// server cannot take.
pub fn create(
    schema: &Schema,
    id: u64,
    class: &str,
    parent: u64,
    props: &[(String, PropValue)],
) -> Result<Vec<u8>, String> {
    let (frame, skipped) = create_partial(schema, id, class, parent, props)?;
    match skipped.into_iter().next() {
        Some((_, reason)) => Err(reason),
        None => Ok(frame),
    }
}

/// Properties a create left out: (name, reason).
pub type Skipped = Vec<(String, String)>;

/// [`create`], leaving out each property the server's schema doesn't
/// replicate for the class or whose value has no wire form here: those are
/// returned as (name, reason) and the rest of the instance is still created.
/// An unknown class fails the whole item.
pub fn create_partial(
    schema: &Schema,
    id: u64,
    class: &str,
    parent: u64,
    props: &[(String, PropValue)],
) -> Result<(Vec<u8>, Skipped), String> {
    create_partial_with(schema, id, class, parent, props, &mut SharedSent::default())
}

/// [`create_partial`], with this connection's [`SharedSent`].
pub fn create_partial_with(
    schema: &Schema,
    id: u64,
    class: &str,
    parent: u64,
    props: &[(String, PropValue)],
    sent: &mut SharedSent,
) -> Result<(Vec<u8>, Skipped), String> {
    let (ci, cls) = schema
        .classes
        .iter()
        .enumerate()
        .find(|(_, c)| c.name == class)
        .ok_or_else(|| format!("Class {class} is not replicated by this server"))?;
    let ci = u16::try_from(ci).map_err(|_| "Class index exceeds protocol range")?;
    let mut out = vec![0x83, 2];
    put_reference(&mut out, id);
    out.extend_from_slice(&ci.to_le_bytes());
    let mut encoded=Vec::new();
    let mut skipped=Vec::new();
    for (name,value) in props {
        let find=|n: &str| cls.props.iter().enumerate().find(|(_,p)| p.name==n);
        let Some((pi,prop))=find(wire_name(name)).or_else(|| find(name)) else {
            skipped.push((name.clone(),format!("{class}.{name} is not replicated by this server")));
            continue;
        };
        if pi>=255 {skipped.push((name.clone(),"Property index exceeds protocol range".into()));continue;}
        // Any Ref-typed property in a NewObject, nil included, resets the
        // session (0x104); as a property change after the create it is
        // applied (live probes on a test place, 2026-11-21). Callers send
        // references that way (live_sync defers them past every create).
        if prop.ty==0x1c {skipped.push((name.clone(),format!("{class}.{name} is a reference: send it as a property change after the create")));continue;}
        // Encode once here so a value without a wire form drops only itself.
        let value=match prepare_value(prop.ty,value) {Ok(v)=>v,Err(e)=>{skipped.push((name.clone(),e));continue;}};
        if let Err(e)=encode_value(&mut Vec::new(),schema,prop.ty,&value,false) {skipped.push((name.clone(),format!("{class}.{name}: {e}")));continue;}
        encoded.push((pi as u8,prop.ty,value));
    }
    // Each group goes in ascending schema index, as the server's own
    // NewObject items do. The CloudEdit server drops the session ("Error
    // while receiving data", 0x104) on a property whose index is below the
    // one before it: rustudio sent Name first, so a SurfaceAppearance with
    // AlphaMode / EmissiveStrength / ColorMapContent (all below Name) and a
    // LocalScript with Disabled / RunContext reset Place A's session (verified
    // live on a test place, 2026-11-20: Name alone is accepted, Name + any of them
    // resets).
    encoded.sort_by_key(|(pi,_,_)| *pi);
    // Protected source is serialized in NewObject's early group.
    for &(pi,ty,ref value) in encoded.iter().filter(|(_,ty,_)| *ty==0x06) {
        out.push(pi);encode_value(&mut out,schema,ty,value,true)?;
    }
    out.push(0xff);
    for &(pi,ty,ref value) in encoded.iter().filter(|(_,ty,_)| *ty!=0x06) {
        out.push(pi);encode_value(&mut out,schema,ty,value,false)?;
    }
    out.push(0xff);
    for (_,_,value) in encoded.iter().filter(|(_,ty,_)| *ty==0x06).chain(encoded.iter().filter(|(_,ty,_)| *ty!=0x06)) {
        append_shared(&mut out,value,sent);
    }
    put_reference(&mut out, parent);
    out.push(0);
    Ok((out, skipped))
}

pub fn delete(id: u64) -> Vec<u8> {
    let mut out = vec![0x83, 1];
    put_reference(&mut out, id);
    out.push(0);
    out
}

/// Parent uses the reserved global descriptor after all ordinary properties.
/// Verified against a cross-account Studio reparent message.
pub fn reparent(schema: &Schema, id: u64, parent: u64) -> Result<Vec<u8>, String> {
    let descriptor =
        u16::try_from(schema.global_props.len()).map_err(|_| "Parent descriptor range")?;
    let mut out = vec![0x83, 3];
    put_reference(&mut out, id);
    out.extend_from_slice(&descriptor.to_le_bytes());
    out.push(0);
    // Parent 0 is nil (no instance has referent 0).
    if parent == 0 {
        out.push(0);
    } else {
        put_reference(&mut out, parent);
    }
    out.push(0);
    Ok(out)
}

fn put_reference(out: &mut Vec<u8>, id: u64) {
    let scope = if id <= u32::MAX as u64 { 1 } else { id >> 32 };
    put_varint(out, scope);
    out.extend_from_slice(&(id as u32).to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replicator::{Decoder, RepOp, SchemaClass, SchemaProp};

    /// Every editor value type the live session sends survives the wire:
    /// the editor value → wire value → frame → decoder → model value.
    #[test]
    fn struct_values_round_trip_through_the_wire() {
        use rbx_types::{self as t, Variant as V};
        let cases: Vec<(&str, &str, u8, V)> = vec![
            ("Frame", "Size", 0x0e, V::UDim2(t::UDim2::new(t::UDim::new(0.5, -10), t::UDim::new(0.25, 40)))),
            ("UIPadding", "PaddingLeft", 0x0d, V::UDim(t::UDim::new(0.1, 8))),
            ("ImageLabel", "SliceCenter", 0x28, V::Rect(t::Rect::new(t::Vector2::new(10.0, 12.0), t::Vector2::new(90.0, 88.0)))),
            ("ParticleEmitter", "Size", 0x23, V::NumberSequence(t::NumberSequence { keypoints: vec![
                t::NumberSequenceKeypoint::new(0.0, 1.0, 0.0), t::NumberSequenceKeypoint::new(1.0, 3.5, 0.5)] })),
            ("ParticleEmitter", "Color", 0x26, V::ColorSequence(t::ColorSequence { keypoints: vec![
                t::ColorSequenceKeypoint::new(0.0, t::Color3::new(1.0, 0.0, 0.0)), t::ColorSequenceKeypoint::new(1.0, t::Color3::new(0.0, 0.0, 1.0))] })),
            ("ParticleEmitter", "Lifetime", 0x25, V::NumberRange(t::NumberRange::new(1.0, 2.5))),
            ("Part", "CustomPhysicalProperties", 0x29, V::PhysicalProperties(t::PhysicalProperties::Custom(t::CustomPhysicalProperties::new(0.7, 0.3, 0.5, 1.0, 1.0, 0.2)))),
            ("Part", "CustomPhysicalProperties", 0x29, V::PhysicalProperties(t::PhysicalProperties::Default)),
            ("TextLabel", "FontFace", 0x36, V::Font(t::Font::new("rbxasset://fonts/families/GothamSSm.json", t::FontWeight::Bold, t::FontStyle::Italic))),
            ("Decal", "TextureContent", 0x3d, V::Content(t::Content::from_uri("rbxassetid://123"))),
            ("Decal", "TextureContent", 0x3d, V::Content(t::Content::default())),
            ("PointLight", "Color", 0x13, V::Color3(t::Color3::new(1.0, 0.5, 0.25))),
            ("TerrainRegion", "ExtentsMin", 0x19, V::Vector3int16(t::Vector3int16::new(-4, 8, 300))),
        ];
        for (class, name, ty, value) in cases {
            let schema = Schema {
                classes: vec![SchemaClass { name: class.into(), props: vec![SchemaProp { name: name.into(), ty }], events: vec![] }],
                global_props: vec![(0, 0)],
                ..Default::default()
            };
            let wire = crate::model::typed_value(&value)
                .or_else(|| match &value { V::Color3(c) => Some(PropValue::Color3([c.r, c.g, c.b])), _ => None })
                .unwrap_or_else(|| panic!("{class}.{name}: no wire value"));
            let mut dec = Decoder::new();
            dec.schema = Some(schema.clone());
            let mut model = crate::model::LiveModel::new();
            model.apply(RepOp::NewInstance { id: 1, class: "DataModel".into(), parent: None, props: vec![] });
            let frame = create(&schema, 7, class, 1, &[]).unwrap();
            for op in dec.decode_frame(frame[0], &frame[1..]) { model.apply(op); }
            let frame = property(&schema, 7, class, name, &wire).unwrap_or_else(|e| panic!("{class}.{name}: {e}"));
            for op in dec.decode_frame(frame[0], &frame[1..]) { model.apply(op); }
            assert!(dec.errors.is_empty(), "{class}.{name}: {:?}", dec.errors);
            let inst = model.dom().get_by_ref(model.reference(7).unwrap()).unwrap();
            let got = inst.properties.iter().find(|(k, _)| k.as_str() == name).map(|(_, v)| v.clone());
            assert_eq!(got.as_ref(), Some(&value), "{class}.{name}");
        }
    }

    /// A Part's Color goes as Color3uint8 bytes; a GUI's Size keeps its name.
    #[test]
    fn renames_apply_only_where_the_class_has_them() {
        let schema = Schema {
            classes: vec![
                SchemaClass { name: "Part".into(), props: vec![SchemaProp { name: "Color3uint8".into(), ty: 0x14 }, SchemaProp { name: "size".into(), ty: 0x16 }], events: vec![] },
                SchemaClass { name: "Frame".into(), props: vec![SchemaProp { name: "Size".into(), ty: 0x0e }], events: vec![] },
            ],
            global_props: vec![(0, 0), (0, 1), (1, 0)],
            ..Default::default()
        };
        let frame = property(&schema, 7, "Part", "Color", &PropValue::Color3([1.0, 0.5, 0.0])).unwrap();
        assert!(frame.windows(3).any(|w| w == [255, 128, 0]));
        assert!(property(&schema, 7, "Part", "Size", &PropValue::Vector3([1.0, 2.0, 3.0])).is_ok());
        assert!(property(&schema, 8, "Frame", "Size", &crate::model::typed_value(&rbx_types::Variant::UDim2(rbx_types::UDim2::new(rbx_types::UDim::new(1.0, 0), rbx_types::UDim::new(0.0, 50)))).unwrap()).is_ok());
    }
    #[test]
    fn binary_attributes_survive_wire_and_become_editable_attributes() {
        let attributes=rbx_types::Attributes::new().with("RustudioSyncProbe",true).with("non_ascii",rbx_types::BinaryString::from("🦊".as_bytes())).with("value",123.456f64);
        let mut bytes=Vec::new();attributes.to_writer(&mut bytes).unwrap();
        assert!(String::from_utf8(bytes.clone()).is_err());
        let schema=Schema {classes:vec![SchemaClass {name:"ModuleScript".into(),props:vec![SchemaProp {name:"AttributesReplicate".into(),ty:2}],events:vec![]}],global_props:vec![(0,0)],..Default::default()};
        let frame=create(&schema,7,"ModuleScript",1,&[("Attributes".into(),PropValue::BinaryString(bytes))]).unwrap();
        let mut decoder=Decoder::new();decoder.schema=Some(schema);
        let mut model=crate::model::LiveModel::new();
        model.apply(RepOp::NewInstance {id:1,class:"DataModel".into(),parent:None,props:vec![]});
        for op in decoder.decode_frame(frame[0],&frame[1..]) {model.apply(op);}
        assert!(decoder.errors.is_empty());
        let script=model.dom().get_by_ref(model.reference(7).unwrap()).unwrap();
        assert_eq!(script.properties.get(&"Attributes".into()),Some(&rbx_types::Variant::Attributes(attributes)));
    }

    #[test]
    fn source_hash_matches_studio_and_inline_record_is_consumed() {
        let schema=Schema {classes:vec![SchemaClass {name:"ModuleScript".into(),props:vec![SchemaProp{name:"Source".into(),ty:6}],events:vec![]}],global_props:vec![(0,0)],..Default::default()};
        let source="-- rustudio synchronization probe\nreturn 2\n";
        let frame=property(&schema,(5u64<<32)|7,"ModuleScript","Source",&PropValue::String(source.into())).unwrap();
        let hash=[0x69,0xac,0xbd,0x1f,0x4b,0x55,0x3f,0xe1,0xe1,0x33,0x24,0xeb,0xcd,0x80,0x88,0xbf];
        assert_eq!(&frame[10..26],&hash);
        let mut dec=Decoder::new();dec.schema=Some(schema.clone());
        let ops=dec.decode_frame(frame[0],&frame[1..]);
        assert!(dec.errors.is_empty(),"{:?}",dec.errors);
        assert!(matches!(&ops[0],RepOp::ChangeProperty {value:PropValue::CachedSharedString {hash:h,bytes},..} if h==&hash && bytes==source.as_bytes()));
        let frame=create(&schema,(5u64<<32)|8,"ModuleScript",1,&[("Source".into(),PropValue::String(source.into()))]).unwrap();
        let ops=dec.decode_frame(frame[0],&frame[1..]);
        assert!(dec.errors.is_empty(),"{:?}",dec.errors);
        assert!(matches!(&ops[0],RepOp::NewInstance {props,..} if matches!(&props[0].1,PropValue::CachedSharedString {hash:h,bytes} if h==&hash && bytes==source.as_bytes())));
    }

    #[test]
    fn source_hash_without_inline_copy_is_followed_by_the_next_item() {
        // A live place, 2026-11-21: after File Sync pushed Sources, the server echoed one
        // as `03 <ref> <prop> 00 <hash>` with no inline copy, then another
        // item; the decoder used to read that item as the copy and stop TC.
        let schema=Schema {classes:vec![SchemaClass {name:"ModuleScript".into(),props:vec![SchemaProp{name:"Source".into(),ty:6}],events:vec![]}],global_props:vec![(0,0)],..Default::default()};
        let first=property(&schema,7,"ModuleScript","Source",&PropValue::String("return 1\n".into())).unwrap();
        let second=property(&schema,8,"ModuleScript","Source",&PropValue::String("return 2\n".into())).unwrap();
        let mut frame=first[..26].to_vec();
        assert_eq!(&frame[10..26],&first[26..42]);
        frame.extend_from_slice(&second[1..]);
        let mut dec=Decoder::new();dec.schema=Some(schema);
        let ops=dec.decode_frame(frame[0],&frame[1..]);
        assert!(dec.errors.is_empty(),"{:?}",dec.errors);
        assert_eq!(ops.len(),2);
        assert!(matches!(&ops[0],RepOp::ChangeProperty {id:7,value:PropValue::SharedHash(h),..} if h[..]==frame[10..26]));
        assert!(matches!(&ops[1],RepOp::ChangeProperty {id:8,value:PropValue::CachedSharedString {bytes,..},..} if bytes==b"return 2\n"));
    }

    /// A test place, 2026-11-21: a second pasted rig re-sent the Animate
    /// Source the server already had and the session reset. The contents go
    /// out once per connection; the second create carries the bare hash, and
    /// one decoder reading both frames (as the server does) has the Source.
    #[test]
    fn a_shared_string_is_sent_once_per_connection() {
        let schema=Schema {classes:vec![SchemaClass {name:"LocalScript".into(),props:vec![SchemaProp{name:"Name".into(),ty:2},SchemaProp{name:"Source".into(),ty:6}],events:vec![]}],global_props:vec![(0,0),(0,1)],..Default::default()};
        let props=vec![("Name".to_string(),PropValue::String("Animate".into())),("Source".to_string(),PropValue::String("-- animate\n".repeat(50)))];
        let mut sent=SharedSent::default();
        let (first,_)=create_partial_with(&schema,0x501,"LocalScript",0xe,&props,&mut sent).unwrap();
        let (second,_)=create_partial_with(&schema,0x502,"LocalScript",0xe,&props,&mut sent).unwrap();
        assert!(second.len()+500<first.len(),"{} vs {}",second.len(),first.len());
        let mut dec=Decoder::new();dec.schema=Some(schema.clone());
        let mut ops=dec.decode_frame(first[0],&first[1..]);
        ops.extend(dec.decode_frame(second[0],&second[1..]));
        assert!(dec.errors.is_empty(),"{:?}",dec.errors);
        assert_eq!(ops.len(),2,"{ops:?}");
        // A fresh connection sends the contents again.
        let (third,_)=create_partial_with(&schema,0x503,"LocalScript",0xe,&props,&mut SharedSent::default()).unwrap();
        assert_eq!(third.len(),first.len());
    }

    #[test]
    fn publish_event_preserves_typed_options_and_response_metadata() {
        use crate::replicator::SchemaEvent;
        let schema=Schema {classes:vec![SchemaClass{name:"TeamCreatePublishService".into(),props:vec![],events:vec![
            SchemaEvent{name:"TeamCreatePlaceUploadRequest".into(),arg_types:vec![0x1f]},
            SchemaEvent{name:"TeamCreatePlaceUploadResponse".into(),arg_types:vec![9,0x1f]},
        ]}],global_events:vec![(0,0),(0,1)],..Default::default()};
        let options=PropValue::Dictionary(vec![("publish".into(),PropValue::Bool(false)),("upload_to_edge".into(),PropValue::Bool(false))]);
        let frame=event(&schema,341653,"TeamCreatePublishService","TeamCreatePlaceUploadRequest",&[options.clone()]).unwrap();
        assert!(frame.windows(7).any(|w| w==b"publish"));
        let mut dec=Decoder::new();dec.schema=Some(schema.clone());
        let ops=dec.decode_frame(frame[0],&frame[1..]);
        assert!(dec.errors.is_empty());
        assert!(matches!(&ops[0],RepOp::Event {args,..} if args==&vec![options]));
        let metadata=PropValue::Dictionary(vec![("version_number".into(),PropValue::Int64(456))]);
        let frame=event(&schema,341653,"TeamCreatePublishService","TeamCreatePlaceUploadResponse",&[PropValue::Bool(true),metadata.clone()]).unwrap();
        let ops=dec.decode_frame(frame[0],&frame[1..]);
        assert!(matches!(&ops[0],RepOp::Event {args,..} if args==&vec![PropValue::Bool(true),metadata]));
        assert!(event(&schema,1,"TeamCreatePublishService","TeamCreatePlaceUploadRequest",&[]).is_err());
    }

    /// Properties go out in ascending schema index, whatever order the
    /// caller gives (Name first used to come before lower indices).
    #[test]
    fn create_writes_properties_in_schema_order() {
        let prop = |name: &str, ty| SchemaProp { name: name.into(), ty };
        let schema = Schema {
            classes: vec![SchemaClass {
                name: "SurfaceAppearance".into(),
                props: vec![prop("AlphaMode", 0x07), prop("ColorMapContent", 0x3d), prop("EmissiveStrength", 0x0b), prop("Name", 0x31)],
                events: vec![],
            }],
            ..Default::default()
        };
        let props = vec![
            ("Name".to_string(), PropValue::String("Look".into())),
            ("EmissiveStrength".to_string(), PropValue::Float(0.0)),
            ("AlphaMode".to_string(), PropValue::Enum(1)),
            ("ColorMapContent".to_string(), PropValue::String("rbxassetid://1".into())),
        ];
        let frame = create(&schema, 7, "SurfaceAppearance", 1, &props).unwrap();
        // 83 02, reference (01 + u32), u16 class, empty early group (ff),
        // then the normal group: AlphaMode (index 0) first.
        assert_eq!(frame[2 + 5 + 2 + 1], 0);
        let mut dec = Decoder::new();
        dec.schema = Some(schema);
        let ops = dec.decode_frame(frame[0], &frame[1..]);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        let RepOp::NewInstance { props: got, .. } = &ops[0] else { panic!() };
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["AlphaMode", "ColorMapContent", "EmissiveStrength", "Name"]);
    }

    /// Parent 0 is written as nil (scope 0), not as referent 0.
    #[test]
    fn reparent_to_nil() {
        let schema = Schema { global_props: vec![(0, 0); 3], ..Default::default() };
        let frame = reparent(&schema, (5u64 << 32) | 2, 0).unwrap();
        assert_eq!(&frame[frame.len() - 2..], &[0, 0], "nil reference, then the end of the item list");
        let mut dec = Decoder::new();
        dec.schema = Some(schema);
        let ops = dec.decode_frame(frame[0], &frame[1..]);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
    }

    #[test]
    fn parent_uses_the_reserved_descriptor() {
        let schema = Schema {
            global_props: vec![(0, 0); 12259],
            ..Default::default()
        };
        let frame = reparent(&schema, (5u64 << 32) | 0xed2ea, (5u64 << 32) | 0xecef4).unwrap();
        assert_eq!(&frame[7..10], &[0xe3, 0x2f, 0]);
        let mut dec = Decoder::new();
        dec.schema = Some(schema);
        let ops = dec.decode_frame(frame[0], &frame[1..]);
        assert!(dec.errors.is_empty());
        assert!(
            matches!(ops[0],RepOp::Reparent {id,parent} if id==(5u64<<32)|0xed2ea && parent==(5u64<<32)|0xecef4)
        );
    }

    #[test]
    fn create_and_delete_preserve_properties_and_scoped_references() {
        let schema = Schema {
            classes: vec![SchemaClass {
                name: "Part".into(),
                props: vec![
                    SchemaProp {
                        name: "Name".into(),
                        ty: 0x31,
                    },
                    SchemaProp {
                        name: "size".into(),
                        ty: 0x16,
                    },
                    SchemaProp {
                        name: "UniqueId".into(),
                        ty: 0x34,
                    },
                ],
                events: vec![],
            }],
            ..Default::default()
        };
        let id = (12u64 << 32) | 1;
        let parent = (5u64 << 32) | 7;
        let props = vec![
            ("Name".into(), PropValue::String("Kept".into())),
            ("size".into(), PropValue::Vector3([2., 3., 4.])),
            ("UniqueId".into(), PropValue::Typed { ty: 0x34, bytes: vec![42; 16] }),
        ];
        let frame = create(&schema, id, "Part", parent, &props).unwrap();
        let mut dec = Decoder::new();
        dec.schema = Some(schema);
        let ops = dec.decode_frame(frame[0], &frame[1..]);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        assert!(
            matches!(&ops[0],RepOp::NewInstance{id:actual,parent:Some(p),props:actual_props,..}
            if *actual==id && *p==parent && actual_props==&props)
        );
        let frame = delete(id);
        assert!(
            matches!(dec.decode_frame(frame[0],&frame[1..])[0],RepOp::DeleteInstance{id:actual} if actual==id)
        );
    }

    #[test]
    fn property_update_round_trips_through_negotiated_schema() {
        let schema = Schema {
            classes: vec![SchemaClass {
                name: "Part".into(),
                props: vec![
                    SchemaProp {
                        name: "size".into(),
                        ty: 0x16,
                    },
                    SchemaProp {
                        name: "Name".into(),
                        ty: 0x31,
                    },
                ],
                events: vec![],
            }],
            global_props: vec![(0, 0), (0, 1)],
            ..Default::default()
        };
        for (name, value) in [
            ("Size", PropValue::Vector3([3.0, 4.0, 5.0])),
            ("Name", PropValue::String("Synced".into())),
        ] {
            let id = (42u64 << 32) | 77;
            let frame = property(&schema, id, "Part", name, &value).unwrap();
            let mut decoder = Decoder::new();
            decoder.schema = Some(schema.clone());
            let ops = decoder.decode_frame(frame[0], &frame[1..]);
            assert!(decoder.errors.is_empty());
            assert!(
                matches!(&ops[0], RepOp::ChangeProperty { id: actual_id, value: actual, .. } if *actual_id == id && actual == &value)
            );
        }
        assert!(property(
            &schema,
            77,
            "Part",
            "Source",
            &PropValue::String("x".into())
        )
        .is_err());
    }

    /// Every valid Studio 0.748 value type byte (0x02..=0x47, excluding the 5
    /// rejected holes) decodes with exact length (verified by trailing sentinel
    /// byte 0xee), converts to the expected rbx_types Variant where applicable,
    /// and re-encodes byte-identically via `writer::encode_value`.
    #[test]
    fn all_studio_0748_value_types_round_trip_with_sentinel() {
        use crate::model::{typed_value, typed_variant};
        use crate::replicator::NetAssetId;
        use rbx_types::{self as t, Variant as V};

        const SENTINEL: u8 = 0xee;
        let mut names = vec!["Workspace".to_string()];
        for i in 1..130 {
            names.push(format!("DictName{i}"));
        }
        let schema = Schema {
            content_prefixes: vec!["".into(), "rbxassetid://".into(), "rbxasset://".into()],
            names,
            net_asset_handles: true,
            replication_pv_v2: false,
            ..Default::default()
        };

        let check_raw = |s: &Schema, ty: u8, payload: &[u8], compress: bool| -> PropValue {
            let mut buf = payload.to_vec();
            buf.push(SENTINEL);
            let (val, consumed) = s
                .decode_value_bytes(ty, &buf, compress)
                .unwrap_or_else(|e| panic!("type {ty:#x} decode failed: {e:?}"));
            assert_eq!(
                consumed,
                payload.len(),
                "type {ty:#x}: consumed {consumed} bytes, expected {}",
                payload.len()
            );
            assert_eq!(buf[consumed], SENTINEL, "type {ty:#x}: sentinel mismatch");
            val
        };

        let check_roundtrip = |s: &Schema, ty: u8, payload: &[u8], compress: bool| -> PropValue {
            let val = check_raw(s, ty, payload, compress);
            let mut reencoded = Vec::new();
            encode_value(&mut reencoded, s, ty, &val, compress)
                .unwrap_or_else(|e| panic!("type {ty:#x} encode failed: {e}"));
            assert_eq!(reencoded, payload, "type {ty:#x}: wire round-trip mismatch");
            val
        };

        let check_variant = |s: &Schema, ty: u8, payload: &[u8], expected: V| {
            let val = check_roundtrip(s, ty, payload, true);
            let PropValue::Typed { ty: t2, bytes } = &val else {
                panic!("type {ty:#x}: expected Typed, got {val:?}");
            };
            let got = typed_variant(*t2, bytes)
                .unwrap_or_else(|| panic!("type {ty:#x}: typed_variant returned None"));
            assert_eq!(got, expected, "type {ty:#x}: Variant mismatch");
            let back = typed_value(&expected)
                .unwrap_or_else(|| panic!("type {ty:#x}: typed_value returned None"));
            let mut out = Vec::new();
            encode_value(&mut out, s, ty, &back, true).unwrap();
            assert_eq!(out, payload, "type {ty:#x}: Variant -> wire mismatch");
        };

        // 0x02 string
        assert_eq!(
            check_roundtrip(&schema, 0x02, b"\x05hello", true),
            PropValue::String("hello".into())
        );
        // 0x03 Void / nil
        assert_eq!(
            check_roundtrip(&schema, 0x03, &[], true),
            PropValue::Typed { ty: 0x03, bytes: vec![] }
        );
        // 0x06 ProtectedString, 0x2e SharedString, 0x2f ProtectedString alt
        for ty in [0x06u8, 0x2e, 0x2f] {
            let hash = [0x42u8; 16];
            assert_eq!(
                check_roundtrip(&schema, ty, &hash, true),
                PropValue::SharedHash(hash)
            );
        }
        // 0x07 Enum
        assert_eq!(
            check_roundtrip(&schema, 0x07, &[0xa0, 0x06], true),
            PropValue::Enum(800)
        );
        // 0x08 BinaryString
        assert_eq!(
            check_roundtrip(&schema, 0x08, &[3, 0xff, 0x00, 0xab], true),
            PropValue::BinaryString(vec![0xff, 0x00, 0xab])
        );
        // 0x09 bool
        assert_eq!(check_roundtrip(&schema, 0x09, &[1], true), PropValue::Bool(true));
        // 0x0a int32 (zigzag -7 = 13 = 0x0d)
        assert_eq!(check_roundtrip(&schema, 0x0a, &[13], true), PropValue::Int(-7));
        // 0x0b float
        assert_eq!(
            check_roundtrip(&schema, 0x0b, &1.5f32.to_le_bytes(), true),
            PropValue::Float32(1.5)
        );
        // 0x0c double
        assert_eq!(
            check_roundtrip(&schema, 0x0c, &2.25f64.to_le_bytes(), true),
            PropValue::Float(2.25)
        );
        // 0x0d UDim
        let mut b = Vec::new();
        b.extend_from_slice(&0.5f32.to_le_bytes());
        b.extend_from_slice(&(-12i32).to_le_bytes());
        check_variant(&schema, 0x0d, &b, V::UDim(t::UDim::new(0.5, -12)));
        // 0x0e UDim2
        let mut b = Vec::new();
        for (sc, off) in [(0.25f32, 10i32), (0.75, -20)] {
            b.extend_from_slice(&sc.to_le_bytes());
            b.extend_from_slice(&off.to_le_bytes());
        }
        check_variant(
            &schema,
            0x0e,
            &b,
            V::UDim2(t::UDim2::new(t::UDim::new(0.25, 10), t::UDim::new(0.75, -20))),
        );
        // 0x0f Ray
        let mut b = Vec::new();
        for v in [1.0f32, 2.0, 3.0, 0.0, 1.0, 0.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(
            &schema,
            0x0f,
            &b,
            V::Ray(t::Ray::new(t::Vector3::new(1.0, 2.0, 3.0), t::Vector3::new(0.0, 1.0, 0.0))),
        );
        // 0x10 Faces (u32 LE)
        check_variant(
            &schema,
            0x10,
            &0x05u32.to_le_bytes(),
            V::Faces(t::Faces::from_bits(0x05).unwrap()),
        );
        // 0x11 Axes (u32 LE)
        check_variant(
            &schema,
            0x11,
            &0x03u32.to_le_bytes(),
            V::Axes(t::Axes::from_bits(0x03).unwrap()),
        );
        // 0x12 BrickColor (u16 LE: 194 = MediumStoneGrey)
        check_variant(
            &schema,
            0x12,
            &194u16.to_le_bytes(),
            V::BrickColor(t::BrickColor::MediumStoneGrey),
        );
        // 0x13 Color3
        let mut b = Vec::new();
        for v in [0.25f32, 0.5, 0.75] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(
            check_roundtrip(&schema, 0x13, &b, true),
            PropValue::Color3([0.25, 0.5, 0.75])
        );
        // 0x14 Color3uint8
        assert_eq!(
            check_roundtrip(&schema, 0x14, &[10, 20, 30], true),
            PropValue::Color3uint8([10, 20, 30])
        );
        // 0x15 Vector2
        let mut b = Vec::new();
        for v in [1.0f32, -2.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(
            check_roundtrip(&schema, 0x15, &b, true),
            PropValue::Vector2([1.0, -2.0])
        );
        // 0x16 Vector3
        let mut b = Vec::new();
        for v in [1.0f32, 2.0, 3.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(
            check_roundtrip(&schema, 0x16, &b, true),
            PropValue::Vector3([1.0, 2.0, 3.0])
        );
        // 0x18 Vector2int16
        let mut b = Vec::new();
        for v in [-5i16, 12] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(&schema, 0x18, &b, V::Vector2int16(t::Vector2int16::new(-5, 12)));
        // 0x19 Vector3int16
        let mut b = Vec::new();
        for v in [-5i16, 12, 300] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(&schema, 0x19, &b, V::Vector3int16(t::Vector3int16::new(-5, 12, 300)));
        // 0x1a CFrame (full matrix + basic rotation id)
        let mut b = Vec::new();
        for v in [1.0f32, 2.0, 3.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(0);
        for v in [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_roundtrip(&schema, 0x1a, &b, true);
        let mut b_rot = Vec::new();
        for v in [4.0f32, 5.0, 6.0] {
            b_rot.extend_from_slice(&v.to_le_bytes());
        }
        b_rot.push(0x02);
        assert_eq!(
            check_raw(&schema, 0x1a, &b_rot, true),
            PropValue::CFrame {
                pos: [4.0, 5.0, 6.0],
                rot: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            }
        );
        // 0x1b CFrame compressed (rot_id = 0 -> 6B packed identity quat: max_idx=3, c0=c1=c2=0)
        let mut b = Vec::new();
        for v in [1.0f32, 2.0, 3.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(0);
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&(3u32 << 30).to_le_bytes());
        assert_eq!(
            check_raw(&schema, 0x1b, &b, true),
            PropValue::CFrame {
                pos: [1.0, 2.0, 3.0],
                rot: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            }
        );
        // 0x1c Object & 0x45 InstanceRef
        for ty in [0x1cu8, 0x45] {
            assert_eq!(check_roundtrip(&schema, ty, &[0], true), PropValue::Ref(None));
            assert_eq!(
                check_roundtrip(&schema, ty, &[1, 0x34, 0x12, 0, 0], true),
                PropValue::Ref(Some(0x1234))
            );
        }
        // 0x1d Tuple, 0x1e Array (including 0x07 Enum with u16 enum id + 0x00 nil)
        let arr_bytes = [2u8, 0x09, 1, 0x07, 0xff, 0xff, 5];
        assert_eq!(
            check_roundtrip(&schema, 0x1d, &arr_bytes, true),
            PropValue::Array(vec![PropValue::Bool(true), PropValue::Enum(5)])
        );
        assert_eq!(
            check_roundtrip(&schema, 0x1e, &arr_bytes, true),
            PropValue::Array(vec![PropValue::Bool(true), PropValue::Enum(5)])
        );
        assert_eq!(
            check_raw(&schema, 0x1e, &[1, 0x00], true),
            PropValue::Array(vec![PropValue::Typed { ty: 0x03, bytes: vec![] }])
        );
        // 0x1f Dictionary, 0x20 Map
        let dict_bytes = [1u8, 1, b'k', 0x09, 0];
        assert_eq!(
            check_roundtrip(&schema, 0x1f, &dict_bytes, true),
            PropValue::Dictionary(vec![("k".into(), PropValue::Bool(false))])
        );
        assert_eq!(
            check_roundtrip(&schema, 0x20, &dict_bytes, true),
            PropValue::Dictionary(vec![("k".into(), PropValue::Bool(false))])
        );
        // 0x21 ContentId
        assert_eq!(
            check_roundtrip(&schema, 0x21, b"\x03abc", false),
            PropValue::String("abc".into())
        );
        assert_eq!(
            check_roundtrip(&schema, 0x21, &[3, 0xf6, 0x01], true),
            PropValue::String("rbxassetid://123".into())
        );
        assert_eq!(
            check_roundtrip(&schema, 0x21, b"\x04\x04font", true),
            PropValue::String("rbxasset://font".into())
        );
        assert_eq!(
            check_roundtrip(&schema, 0x21, b"\x00\x03foo", true),
            PropValue::String("foo".into())
        );
        // 0x22 SystemAddress
        assert_eq!(check_roundtrip(&schema, 0x22, &[2], true), PropValue::Int(2));
        // 0x23 NumberSequence
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_le_bytes());
        for v in [0.0f32, 1.0, 0.25] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(
            &schema,
            0x23,
            &b,
            V::NumberSequence(t::NumberSequence {
                keypoints: vec![t::NumberSequenceKeypoint::new(0.0, 1.0, 0.25)],
            }),
        );
        // 0x24 NumberSequenceKeypoint
        check_roundtrip(&schema, 0x24, &[0u8; 12], true);
        // 0x25 NumberRange
        let mut b = Vec::new();
        b.extend_from_slice(&1.0f32.to_le_bytes());
        b.extend_from_slice(&5.0f32.to_le_bytes());
        check_variant(&schema, 0x25, &b, V::NumberRange(t::NumberRange::new(1.0, 5.0)));
        // 0x26 ColorSequence
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_le_bytes());
        for v in [0.5f32, 1.0, 0.5, 0.0, 0.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(
            &schema,
            0x26,
            &b,
            V::ColorSequence(t::ColorSequence {
                keypoints: vec![t::ColorSequenceKeypoint::new(0.5, t::Color3::new(1.0, 0.5, 0.0))],
            }),
        );
        // 0x27 ColorSequenceKeypoint
        check_roundtrip(&schema, 0x27, &[0u8; 20], true);
        // 0x28 Rect2D
        let mut b = Vec::new();
        for v in [1.0f32, 2.0, 3.0, 4.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(
            &schema,
            0x28,
            &b,
            V::Rect(t::Rect::new(t::Vector2::new(1.0, 2.0), t::Vector2::new(3.0, 4.0))),
        );
        // 0x29 PhysicalProperties
        check_variant(&schema, 0x29, &[0], V::PhysicalProperties(t::PhysicalProperties::Default));
        let mut b = vec![3u8];
        for v in [0.7f32, 0.3, 0.5, 1.0, 1.0, 0.2] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(
            &schema,
            0x29,
            &b,
            V::PhysicalProperties(t::PhysicalProperties::Custom(
                t::CustomPhysicalProperties::new(0.7, 0.3, 0.5, 1.0, 1.0, 0.2),
            )),
        );
        // 0x2a Region3
        let mut b = Vec::new();
        for v in [-1.0f32, -2.0, -3.0, 4.0, 5.0, 6.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(
            &schema,
            0x2a,
            &b,
            V::Region3(t::Region3::new(t::Vector3::new(-1.0, -2.0, -3.0), t::Vector3::new(4.0, 5.0, 6.0))),
        );
        // 0x2b Region3int16
        let mut b = Vec::new();
        for v in [-1i16, -2, -3, 4, 5, 6] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(
            &schema,
            0x2b,
            &b,
            V::Region3int16(t::Region3int16::new(
                t::Vector3int16::new(-1, -2, -3),
                t::Vector3int16::new(4, 5, 6),
            )),
        );
        // 0x2c int64
        assert_eq!(check_roundtrip(&schema, 0x2c, &[1], true), PropValue::Int64(-1));
        // 0x30 DateTime
        check_roundtrip(&schema, 0x30, &1_700_000_000_000u64.to_le_bytes(), true);
        // 0x31 Name
        assert_eq!(
            check_roundtrip(&schema, 0x31, b"\x04Part", true),
            PropValue::String("Part".into())
        );
        let long_name = "A".repeat(130);
        let mut long_bytes = vec![0x7f, 130, 1];
        long_bytes.extend_from_slice(long_name.as_bytes());
        assert_eq!(
            check_roundtrip(&schema, 0x31, &long_bytes, true),
            PropValue::String(long_name)
        );
        assert_eq!(
            check_roundtrip(&schema, 0x31, &[0x80], true),
            PropValue::String("Workspace".into())
        );
        assert_eq!(
            check_roundtrip(&schema, 0x31, &[0xff, 1], true),
            PropValue::String("DictName128".into())
        );
        // 0x32 OptionalCFrame
        check_variant(&schema, 0x32, &[0], V::OptionalCFrame(None));
        let mut b = vec![0x80u8];
        for v in [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 7.0, 8.0, 9.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        check_variant(
            &schema,
            0x32,
            &b,
            V::OptionalCFrame(Some(t::CFrame::new(
                t::Vector3::new(7.0, 8.0, 9.0),
                t::Matrix3::identity(),
            ))),
        );
        // 0x33 OptionalCFrame compressed
        let mut b = vec![0x80u8];
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&(3u32 << 30).to_le_bytes());
        for v in [7.0f32, 8.0, 9.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(check_raw(&schema, 0x33, &[0], true), PropValue::Typed { ty: 0x32, bytes: vec![] });
        let PropValue::Typed { ty: 0x32, bytes } = check_raw(&schema, 0x33, &b, true) else { panic!() };
        assert_eq!(
            typed_variant(0x32, &bytes),
            Some(V::OptionalCFrame(Some(t::CFrame::new(
                t::Vector3::new(7.0, 8.0, 9.0),
                t::Matrix3::identity(),
            ))))
        );
        // 0x34 UniqueId
        let mut b = Vec::new();
        b.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        b.extend_from_slice(&0x99aa_bbccu32.to_le_bytes());
        b.extend_from_slice(&0x0000_0042u32.to_le_bytes());
        check_variant(
            &schema,
            0x34,
            &b,
            V::UniqueId(t::UniqueId::new(0x42, 0x99aa_bbcc, 0x1122_3344_5566_7788)),
        );
        // 0x35 PathWaypoint
        let mut b = vec![0u8; 12];
        b.push(1);
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(b"hi");
        check_roundtrip(&schema, 0x35, &b, true);
        // 0x36 Font
        let mut b = Vec::new();
        b.extend_from_slice(&700u16.to_le_bytes());
        b.push(1);
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(b"fams");
        b.extend_from_slice(&0u32.to_le_bytes());
        check_variant(
            &schema,
            0x36,
            &b,
            V::Font(t::Font::new("fams", t::FontWeight::Bold, t::FontStyle::Italic)),
        );
        // 0x37 SecurityCapabilities
        check_variant(
            &schema,
            0x37,
            &0x1234u64.to_le_bytes(),
            V::SecurityCapabilities(t::SecurityCapabilities::from_bits(0x1234)),
        );
        // 0x38 buffer
        assert_eq!(
            check_roundtrip(&schema, 0x38, &[0, 3, 1, 2, 3], true),
            PropValue::BinaryString(vec![1, 2, 3])
        );
        let compressed = zstd::bulk::compress(b"hello buffer", 1).unwrap();
        let mut zbuf = Vec::new();
        put_varint(&mut zbuf, compressed.len() as u64);
        put_varint(&mut zbuf, 12);
        zbuf.extend_from_slice(&compressed);
        assert_eq!(
            check_raw(&schema, 0x38, &zbuf, true),
            PropValue::BinaryString(b"hello buffer".to_vec())
        );
        // 0x39 ReplicationPV v1 and v2
        check_roundtrip(&schema, 0x39, &[0x01, 0x00, 1, 2, 3, 4, 0x00, 0x00], true);
        let schema_pv2 = Schema { replication_pv_v2: true, ..schema.clone() };
        check_roundtrip(&schema_pv2, 0x39, &[0x69, 1, 2, 3, 4], true);
        // 0x3a FacsReplicationData
        check_roundtrip(&schema, 0x3a, &[0, 1, 2, 3, 4, 5, 6, 2, 0xaa, 0xbb], true);
        // 0x3b NetAssetHandle, 0x3c NetAssetRef
        for ty in [0x3bu8, 0x3c] {
            assert_eq!(
                check_roundtrip(&schema, ty, &[0xff, 0xff, 0x03], true),
                PropValue::NetAssetHandle(None)
            );
            let mut b = vec![0u8];
            b.extend_from_slice(&0x1234u64.to_le_bytes());
            b.push(7);
            assert_eq!(
                check_roundtrip(&schema, ty, &b, true),
                PropValue::NetAssetHandle(Some(NetAssetId { guid: 0x1234, index: 7 }))
            );
        }
        // 0x3d Content
        assert_eq!(check_roundtrip(&schema, 0x3d, &[0], true), PropValue::String(String::new()));
        assert_eq!(
            check_roundtrip(&schema, 0x3d, &[1, 3, 0xf6, 0x01], true),
            PropValue::String("rbxassetid://123".into())
        );
        check_raw(&schema, 0x3d, &[2, 1, 5, 0, 0, 0], true);
        assert_eq!(
            check_raw(&schema, 0x3d, &[3, 3, 0xf6, 0x01], true),
            PropValue::String("rbxassetid://123".into())
        );
        // 0x3e Random
        check_roundtrip(&schema, 0x3e, &42u64.to_le_bytes(), true);
        // 0x3f SlimReplicationData
        check_roundtrip(&schema, 0x3f, &[1, 1, 2, 3, 4, 5, 6, 7, 8], true);
        // 0x41 AnimTrackPlayState
        check_roundtrip(&schema, 0x41, &[1u8; 13], true);
        // 0x42 AnimTrackMetadata
        check_roundtrip(&schema, 0x42, &[1, 2, 3], true);
        // 0x43 AnimTrackWeight
        check_roundtrip(&schema, 0x43, &[0u8; 16], true);
        // 0x44 User
        check_roundtrip(&schema, 0x44, &[2, 1, 4, 6], true);
        // 0x46 TweenInfo v1
        check_roundtrip(&schema, 0x46, &[0u8; 15], true);
        // 0x47 TweenInfo v2
        check_roundtrip(&schema, 0x47, &[0u8; 15], true);
        let mut t12 = vec![0u8; 23];
        t12[12] = 0x0c;
        check_roundtrip(&schema, 0x47, &t12, true);
        let mut t11 = vec![0u8; 15];
        t11[12] = 0x0b;
        t11.push(2);
        t11.extend_from_slice(&[0u8; 16]);
        check_roundtrip(&schema, 0x47, &t11, true);

        // Rejected types in Studio 0.748
        for bad_ty in [0x00u8, 0x01, 0x04, 0x05, 0x17, 0x2d, 0x40, 0x48, 0xff] {
            assert!(
                schema.decode_value_bytes(bad_ty, &[0u8; 32], true).is_err(),
                "type {bad_ty:#x} should be rejected"
            );
        }
    }
}
