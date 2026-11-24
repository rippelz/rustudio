#!/usr/bin/env python3
"""Capture terrain closeups in an explicitly supplied, agent-owned edit Studio.
Usage: terrain_closeup.py OWNED_STUDIO_SLOT PLACE OUTPUT_DIRECTORY
       terrain_closeup.py - PLACE OUTPUT_DIRECTORY --reuse-studio DIR   (rustudio only)
The place must contain terrain_build.luau's material pads and have Decoration=false.
Studio capture includes its 38px device bar, drawn over the 3D view: rustudio
renders the same full viewport and metrics drop those rows from both. No Studio is saved.

The Studio must draw textured terrain: the offline Studio is served terrain TexturePacks as the
KTX2 representations Studio asks for (see
MECHANICS.md "Textured offline Studio reference"); else it draws flat avgAlbedo.

Metrics leave out rustudio's viewport overlays (the "Select · World" chip at
the top left, the axis gizmo at the bottom left) in both images.
"""
import sys,os,re,time,json,shutil,subprocess,argparse
sys.path.insert(0,'tools/fidelity')
from PIL import Image
import numpy as np
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('instance'); parser.add_argument('place'); parser.add_argument('out')
parser.add_argument('--binary',default=os.environ.get('RUSTUDIO_BIN','target/release/rustudio'))
parser.add_argument('--studio-only',action='store_true')
parser.add_argument('--reuse-studio',help='directory holding <name>-studio.png from an earlier capture')
parser.add_argument('--only',help='comma-separated names to (re)render')
args=parser.parse_args(); instance,place,out=args.instance,args.place,args.out; out=os.path.abspath(os.path.expanduser(out)); os.makedirs(out,exist_ok=True)
names='Grass Slate Concrete Brick Sand WoodPlanks Rock Glacier Snow Sandstone Mud Basalt Ground CrackedLava Asphalt Cobblestone Ice LeafyGrass Salt Limestone Pavement'.split()
# Oblique view, eight studs above the pad, with a 0.1 stud horizontal offset to avoid the vertical-camera singularity.
cams=[dict(material=n,x=(i%7)*32,y=12,z=(i//7)*32+.1,yaw=0,pitch=-89.28384) for i,n in enumerate(names)]
cams += [dict(material='Water',x=0,y=12,z=96.1,yaw=0,pitch=-89.28384)]
cams += [dict(material='boundary-'+str(i+1),x=256+i*32,y=12,z=.1,yaw=0,pitch=-89.28384) for i in range(3)]
json.dump(cams,open(out+'/cameras.json','w'),indent=2)
only=set(args.only.split(',')) if args.only else None
m=None
if not args.reuse_studio:
 from fidelity import Mcp
 m=Mcp();a={'instance':instance}
 quality=m.call('run_luau',dict(a,code='settings().Rendering.EditQualityLevel=Enum.QualityLevel.Level21; return tostring(settings().Rendering.EditQualityLevel)'))
 if 'Level21' not in quality: raise RuntimeError('Cannot verify Editor Quality Level: '+quality)
 json.dump(dict(editor_quality=quality,instance=instance,binary=os.path.abspath(args.binary)),open(out+'/capture-settings.json','w'),indent=2)
for c in cams:
 n=c['material'];x=c['x'];z=c['z']
 if only and n not in only: continue
 if args.reuse_studio:
  src=os.path.join(os.path.expanduser(args.reuse_studio),f'{n}-studio.png')
  if os.path.abspath(src)!=f'{out}/{n}-studio.png': shutil.copy(src,f'{out}/{n}-studio.png')
 else:
  r=m.call('run_luau',dict(a,code=f"workspace.CurrentCamera.CameraType=Enum.CameraType.Scriptable; workspace.CurrentCamera.FieldOfView=70; workspace.CurrentCamera.CFrame=CFrame.lookAt(Vector3.new({x},12,{z}),Vector3.new({x},4,{z-.1})); return game:GetService('RunService'):IsEdit()"))
  if 'true' not in r: raise RuntimeError(r)
  time.sleep(.3)
  r=m.call('screenshot',dict(a,target='viewport',max_width=4096));p=re.search(r'saved to (\S+\.png)',r)
  if not p:raise RuntimeError(r)
  shutil.copy(p[1],f'{out}/{n}-studio.png')
 if args.studio_only:
  print(n,flush=True)
  continue
 studio_size=Image.open(f'{out}/{n}-studio.png').size
 # The headless editor chrome uses 284 horizontal and 396 vertical pixels.
 # Studio's 3D view runs under its 38 px device bar (its vertical FOV spans
 # the whole viewport), so rustudio renders the full viewport and both lose
 # the bar's rows in the metric.
 window=f'{studio_size[0]+284}x{studio_size[1]+396}'
 cmd=['gamescope','--backend','headless','--',args.binary,place,'--screenshot',f'{out}/{n}-rustudio.png','--shot-after','3','--window',window,'--camera',f'{x},12,{z},0,-89.28384']
 with open(out+'/render.log','a') as f:subprocess.run(cmd,env=dict(os.environ,RUSTUDIO_NO_AUDIO='1'),stdout=f,stderr=f,check=True)

 view=f'{out}/{n}-rustudio.view.png'
 if os.path.exists(view): shutil.copy(view,f'{out}/{n}-rustudio.png')
 print(n,flush=True)

if args.studio_only: sys.exit(0)

def overlay_mask(w,h):
 """True where both images are compared: rustudio's overlay chip and gizmo left out
 (the chip sits in the rows under Studio's device bar, already cropped)."""
 keep=np.ones((h,w),bool)
 keep[h-64:,:64]=False         # axis gizmo
 return keep

rows=[]
for c in cams:
 n=c['material']
 if not os.path.exists(f'{out}/{n}-rustudio.png'): continue
 studio=Image.open(f'{out}/{n}-studio.png').convert('RGB')
 studio=studio.crop((0,38,studio.width,studio.height))
 rust=Image.open(f'{out}/{n}-rustudio.png').convert('RGB')
 if rust.height == studio.height + 38:
  rust=rust.crop((0,38,rust.width,rust.height))
 if studio.size != rust.size:
  raise RuntimeError(f'{n}: render dimensions differ: {studio.size} vs {rust.size}')
 s=np.asarray(studio,dtype=float); r=np.asarray(rust,dtype=float); keep=overlay_mask(*studio.size)
 mae=float(np.abs(s-r)[keep].mean())
 mean_s=s[keep].mean(0); mean_r=r[keep].mean(0)
 rows.append(dict(material=n,mae=mae,mean_studio=[round(v,1) for v in mean_s],mean_rustudio=[round(v,1) for v in mean_r],
  std_studio=round(float(s[keep].std()),1),std_rustudio=round(float(r[keep].std()),1),
  studio=f'{out}/{n}-studio.png',rustudio=f'{out}/{n}-rustudio.png'))
json.dump(rows,open(out+'/metrics.json','w'),indent=2)
for x in rows: print(f"{x['material']:12s} MAE {x['mae']:6.2f}  mean S {x['mean_studio']} R {x['mean_rustudio']}  std S {x['std_studio']} R {x['std_rustudio']}")
