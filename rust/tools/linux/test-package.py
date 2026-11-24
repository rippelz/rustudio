import os, pathlib, subprocess, tempfile, tarfile, shlex, sys
temporary=tempfile.TemporaryDirectory(prefix='rustudio-package-')
root=pathlib.Path(temporary.name)
home=root/'clean home'; home.mkdir(); bundle=root/'bundle'; bundle.mkdir()
with tarfile.open(sys.argv[1]) as t: t.extractall(bundle)  # Trusted archive produced by package.sh (Python 3.10 compatible).
bundle=bundle/'rustudio-linux-x86_64'
assert (bundle/'LICENSE-MIT').is_file() and (bundle/'LICENSE-APACHE').is_file()
env=dict(os.environ,HOME=str(home),PATH='/usr/bin:/bin',XDG_DATA_HOME=str(home/'.local/share'),XDG_CONFIG_HOME=str(home/'.config'),XDG_CACHE_HOME=str(home/'.cache'))
def install(*args): subprocess.run(['sh',str(bundle/'install.sh'),*args],env=env,cwd='/tmp',check=True)
install()
prefix=home/'.local'; desktop=prefix/'share/applications/rustudio.desktop'
for line in desktop.read_text().splitlines():
 if line.startswith('Exec='): assert pathlib.Path(shlex.split(line[5:])[0]).is_file()
assert 'Icon=rustudio' in desktop.read_text()
for size in [16,24,32,48,64,128,256,512]: assert (prefix/f'share/icons/hicolor/{size}x{size}/apps/rustudio.png').is_file()
assert (prefix/'share/icons/hicolor/scalable/apps/rustudio.svg').is_file()
assert (prefix/'lib/rustudio/api-dump.json').read_bytes()==(bundle/'api-dump.json').read_bytes()
result=subprocess.run([str(prefix/'bin/rustudio'),'--api-coverage',str(root/'coverage.json')],env=env,cwd='/tmp',capture_output=True,text=True,timeout=60)
print('API coverage exit:',result.returncode, result.stderr[-1000:]); assert result.returncode==0
assert (root/'coverage.json').stat().st_size>0
sentinels=[home/'.config/rustudio/settings',prefix/'share/rustudio/project.rbxl',prefix/'lib/rustudio/user-file']
for p in sentinels: p.parent.mkdir(parents=True,exist_ok=True); p.write_text('keep')
install(); install('--uninstall'); install('--uninstall')
for p in sentinels: assert p.read_text()=='keep'
assert not desktop.exists(); assert not (prefix/'bin/rustudio').exists(); assert not (prefix/'lib/rustudio/api-dump.json').exists()
print('PASS: archive, launcher, icons, API coverage, reinstall, repeated uninstall, data preservation',root)

temporary.cleanup()
