param([Parameter(Mandatory=$true)][string]$Archive)
$ErrorActionPreference = 'Stop'
$root = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
$prefix = Join-Path $root 'installed app'
$programs = Join-Path $root 'Start Menu'
try {
    Expand-Archive -LiteralPath $Archive -DestinationPath $root
    $bundle = Join-Path $root 'rustudio-windows-x64'
    $installer = Join-Path $bundle 'install.ps1'
    # LoadLibraryEx as data avoids executing application code.
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public class IconResource {
 [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr LoadLibraryEx(string path, IntPtr file, uint flags);
 [DllImport("kernel32.dll")] public static extern IntPtr FindResource(IntPtr module, IntPtr name, IntPtr type);
 [DllImport("kernel32.dll")] public static extern bool FreeLibrary(IntPtr module);
}
'@
    $module = [IconResource]::LoadLibraryEx((Join-Path $bundle 'rustudio.exe'), [IntPtr]::Zero, 2)
    try {
        if ($module -eq [IntPtr]::Zero -or [IconResource]::FindResource($module, [IntPtr]1, [IntPtr]14) -eq [IntPtr]::Zero) {
            throw 'Executable has no icon group resource'
        }
    } finally { if ($module -ne [IntPtr]::Zero) { [void][IconResource]::FreeLibrary($module) } }
    & $installer -Prefix $prefix -ProgramsDirectory $programs
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut((Join-Path $programs 'rustudio.lnk'))
    if ($link.TargetPath -ne (Join-Path $prefix 'rustudio.exe')) { throw 'Wrong shortcut target' }
    if ($link.IconLocation -ne "$(Join-Path $prefix 'rustudio.exe'),0") { throw 'Wrong shortcut icon' }
    if ($link.WorkingDirectory -ne $prefix) { throw 'Wrong working directory' }
    if ((Get-FileHash (Join-Path $prefix 'api-dump.json')).Hash -ne (Get-FileHash (Join-Path $bundle 'api-dump.json')).Hash) { throw 'API data missing or changed' }
    Set-Content (Join-Path $prefix 'user-project.rbxl') 'preserve'
    & $installer -Prefix $prefix -ProgramsDirectory $programs
    & (Join-Path $prefix 'install.ps1') -Uninstall -Prefix $prefix -ProgramsDirectory $programs
    & $installer -Uninstall -Prefix $prefix -ProgramsDirectory $programs
    if (Test-Path (Join-Path $programs 'rustudio.lnk')) { throw 'Shortcut remains' }
    if (Test-Path (Join-Path $prefix 'rustudio.exe')) { throw 'Executable remains' }
    if ((Get-Content (Join-Path $prefix 'user-project.rbxl')) -ne 'preserve') { throw 'User data lost' }
    & $installer -Prefix $prefix -ProgramsDirectory $programs -NoShortcut
    if (Test-Path (Join-Path $programs 'rustudio.lnk')) { throw 'Unwanted shortcut created' }
    & $installer -Uninstall -Prefix $prefix -ProgramsDirectory $programs
    Write-Host 'PASS: embedded icon, shortcut, API data, reinstall and uninstall'
} finally { if (Test-Path $root) { Remove-Item -LiteralPath $root -Recurse -Force } }
