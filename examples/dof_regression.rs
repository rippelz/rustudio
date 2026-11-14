//! Executes the production CoC and radius expressions at known stud depths.
use egui_wgpu::wgpu;
fn main(){pollster::block_on(run())}
async fn run(){
 let source=std::fs::read_to_string("src/shader.wgsl").unwrap();
 let coc=source.split("fn dof_coc(").nth(1).unwrap().split("@fragment").next().unwrap();
 let radius=source.split("let intensity = select(g.dof.w, g.dof.z, center.a < 0.0);").nth(1).unwrap().split("var sum").next().unwrap();
 let shader=format!(r#"
 struct Globals {{ dof: vec4<f32> }}
 var<private> g: Globals;
 fn fx_distance(uv: vec2<f32>)->f32 {{ return uv.x; }}
 fn dof_coc({coc}
 @group(0) @binding(0) var<storage,read_write> result: array<f32>;
 @compute @workgroup_size(1) fn main() {{
 g.dof=vec4<f32>(100.0,20.0,0.75,0.025);
 let depths=array<f32,5>(40.0,80.0,100.0,120.0,240.0);
 for(var i=0u;i<5u;i++){{
 let c=dof_coc(vec2<f32>(depths[i],0.0));
 let center=vec4<f32>(0.0,0.0,0.0,c);let coc2=c*c;
 let intensity=select(g.dof.w,g.dof.z,center.a<0.0);
 {radius}
 result[i]=radius;
 }}
 }}
 "#);
 let instance=wgpu::Instance::default();let a=instance.request_adapter(&Default::default()).await.unwrap();let(d,q)=a.request_device(&Default::default()).await.unwrap();
 let module=d.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("production DOF regression"),source:wgpu::ShaderSource::Wgsl(shader.into())});
 let pipeline=d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:None,layout:None,module:&module,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
 let out=d.create_buffer(&wgpu::BufferDescriptor{label:None,size:20,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_SRC,mapped_at_creation:false});
 let read=d.create_buffer(&wgpu::BufferDescriptor{label:None,size:20,usage:wgpu::BufferUsages::MAP_READ|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false});
 let bg=d.create_bind_group(&wgpu::BindGroupDescriptor{label:None,layout:&pipeline.get_bind_group_layout(0),entries:&[wgpu::BindGroupEntry{binding:0,resource:out.as_entire_binding()}]});
 let mut enc=d.create_command_encoder(&Default::default());{let mut p=enc.begin_compute_pass(&Default::default());p.set_pipeline(&pipeline);p.set_bind_group(0,&bg,&[]);p.dispatch_workgroups(1,1,1);}enc.copy_buffer_to_buffer(&out,0,&read,0,20);q.submit([enc.finish()]);
 let(tx,rx)=std::sync::mpsc::channel();read.slice(..).map_async(wgpu::MapMode::Read,move|r|tx.send(r).unwrap());d.poll(wgpu::PollType::wait_indefinitely()).unwrap();rx.recv().unwrap().unwrap();let data=read.slice(..).get_mapped_range().unwrap();let actual:&[f32]=bytemuck::cast_slice(&data);
 for (value,expected) in actual.iter().zip([0.75,0.75*(0.5_f32/80.0).powi(2),0.0,0.0,0.025*(119.5_f32/240.0).powi(2)]){assert!((value-expected).abs()<1e-6,"{actual:?}");}
 println!("PASS GPU production CoC/radius near40 / focus80,100,120 / far240 studs: {actual:?}");
}
