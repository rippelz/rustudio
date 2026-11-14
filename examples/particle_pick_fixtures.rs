//! Make disposable picking variants of a supplied Village place. Original stays untouched.
use rbx_dom_weak::{InstanceBuilder, types::{Variant,Vector3,CFrame,Matrix3}};
fn main() {
 let args: Vec<_> = std::env::args().collect();
 for mode in ["locked", "attachment", "visible"] {
  let mut d = rbx_binary::from_reader(std::fs::File::open(&args[1]).unwrap()).unwrap();
  let fields:Vec<_> = d.descendants().filter(|i| i.name=="FieldSpawner").map(|i|i.referent()).collect();
  for r in fields {
   if mode=="locked" {d.get_by_ref_mut(r).unwrap().properties.insert("Locked".into(),Variant::Bool(true));}
   if mode=="attachment" {
    let children=d.get_by_ref(r).unwrap().children().to_vec();
    let a=d.insert(r,InstanceBuilder::new("Attachment"));
    for c in children {if d.get_by_ref(c).unwrap().class=="ParticleEmitter" {d.transfer_within(c,a);}}
   }
  }
  if mode=="visible" {
   let ws=d.descendants().find(|i| i.class=="Workspace").unwrap().referent();
   d.insert(ws, InstanceBuilder::new("Part").with_name("VisiblePriorityProbe")
    .with_property("Anchored",true).with_property("Size",Vector3::new(30.,2.,30.))
    .with_property("CFrame",CFrame::new(Vector3::new(329.9,12.,-395.7),Matrix3::identity())));
  }
  let path=format!("{}/{}.rbxl",args[2],mode);
  rbx_binary::to_writer(std::fs::File::create(path).unwrap(),&d,d.root().children()).unwrap();
 }
}
