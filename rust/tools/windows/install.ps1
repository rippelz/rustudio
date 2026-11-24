# Per-user installation from an extracted release ZIP. No admin rights required.
[CmdletBinding()]
param(
    [switch]$Uninstall,
    [switch]$Interactive,
    [switch]$NoShortcut,
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'Programs\rustudio'),
    [string]$ProgramsDirectory = [Environment]::GetFolderPath('Programs')
)
$ErrorActionPreference = 'Stop'
if ($Interactive -and !$Uninstall) {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    [System.Windows.Forms.Application]::EnableVisualStyles()
    $form = New-Object System.Windows.Forms.Form
    $form.Text = 'Install rustudio'
    $form.ClientSize = New-Object System.Drawing.Size(560, 230)
    $form.StartPosition = 'CenterScreen'
    $form.FormBorderStyle = 'FixedDialog'
    $form.MaximizeBox = $false
    $form.MinimizeBox = $false
    $form.AutoScaleMode = 'Font'

    $heading = New-Object System.Windows.Forms.Label
    $heading.Text = 'Choose where to install rustudio for your account.'
    $heading.SetBounds(20, 20, 520, 25)
    $label = New-Object System.Windows.Forms.Label
    $label.Text = 'Installation folder'
    $label.SetBounds(20, 55, 400, 20)
    $folder = New-Object System.Windows.Forms.TextBox
    $folder.Text = $Prefix
    $folder.SetBounds(20, 80, 420, 25)
    $browse = New-Object System.Windows.Forms.Button
    $browse.Text = 'Browse...'
    $browse.SetBounds(450, 78, 90, 28)
    $browse.Add_Click({
        $picker = New-Object System.Windows.Forms.FolderBrowserDialog
        $picker.Description = 'Choose the rustudio installation folder'
        $picker.SelectedPath = $folder.Text
        try {
            if ($picker.ShowDialog($form) -eq 'OK') { $folder.Text = $picker.SelectedPath }
        } finally { $picker.Dispose() }
    })
    $shortcutOption = New-Object System.Windows.Forms.CheckBox
    $shortcutOption.Text = 'Create a Start menu shortcut'
    $shortcutOption.Checked = !$NoShortcut
    $shortcutOption.SetBounds(20, 120, 500, 25)
    $install = New-Object System.Windows.Forms.Button
    $install.Text = 'Install'
    $install.SetBounds(340, 180, 95, 30)
    $install.Add_Click({
        if ($folder.Text -notmatch '^(?:[A-Za-z]:[\\/]|\\\\[^\\]+\\[^\\]+)') {
            [void][System.Windows.Forms.MessageBox]::Show($form, 'Choose an absolute installation path.', 'Install rustudio')
            return
        }
        $form.DialogResult = 'OK'
        $form.Close()
    })
    $cancel = New-Object System.Windows.Forms.Button
    $cancel.Text = 'Cancel'
    $cancel.DialogResult = 'Cancel'
    $cancel.SetBounds(445, 180, 95, 30)
    $form.AcceptButton = $install
    $form.CancelButton = $cancel
    $form.Controls.AddRange(@($heading, $label, $folder, $browse, $shortcutOption, $install, $cancel))
    try {
        if ($form.ShowDialog() -ne 'OK') { return }
        $Prefix = $folder.Text
        $NoShortcut = !$shortcutOption.Checked
    } finally { $form.Dispose() }
}
# Command-line use remains noninteractive unless -Interactive is requested.
trap {
    if ($Interactive) {
        [void][System.Windows.Forms.MessageBox]::Show(
            "Installation failed: $($_.Exception.Message)`nClose rustudio before upgrading and choose a writable folder.",
            'Install rustudio', 'OK', 'Error')
    }
    break
}
$Prefix = [IO.Path]::GetFullPath($Prefix)
$shortcutPath = Join-Path $ProgramsDirectory 'rustudio.lnk'
$files = @('rustudio.exe', 'api-dump.json', 'LICENSE-MIT', 'LICENSE-APACHE', 'README.md', 'install.ps1', 'uninstall.cmd')
$exe = Join-Path $Prefix 'rustudio.exe'
# Fail before copying if the installed executable is still running.
if (Test-Path $exe) {
    $stream = [IO.File]::Open($exe, 'Open', 'ReadWrite', 'None')
    $stream.Dispose()
}
if ($Uninstall) {
    if (Test-Path $shortcutPath) {
        $shell = New-Object -ComObject WScript.Shell
        if ($shell.CreateShortcut($shortcutPath).TargetPath -eq $exe) {
            Remove-Item -LiteralPath $shortcutPath
        }
    }
    foreach ($file in $files) {
        $path = Join-Path $Prefix $file
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
    }
    if ((Test-Path $Prefix) -and @(Get-ChildItem -LiteralPath $Prefix -Force).Count -eq 0) {
        Remove-Item -LiteralPath $Prefix
    }
    Write-Host 'rustudio uninstalled. Projects, settings and downloaded content were preserved.'
    return
}
foreach ($file in $files) {
    if (!(Test-Path -LiteralPath (Join-Path $PSScriptRoot $file))) { throw "Missing bundled file: $file" }
}
New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
foreach ($file in $files) {
    $source = Join-Path $PSScriptRoot $file
    $destination = Join-Path $Prefix $file
    if ([IO.Path]::GetFullPath($source) -ne [IO.Path]::GetFullPath($destination)) {
        Copy-Item -LiteralPath $source -Destination $destination -Force
    }
}
if (!$NoShortcut) {
    New-Item -ItemType Directory -Force -Path $ProgramsDirectory | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = $exe
    $shortcut.WorkingDirectory = $Prefix
    $shortcut.IconLocation = "$exe,0"
    $shortcut.Description = 'rustudio'
    $shortcut.Save()
}
$message = "rustudio installed to $Prefix."
if (!$NoShortcut) { $message += ' Launch it from the Start menu.' }
Write-Host $message
if ($Interactive) {
    [void][System.Windows.Forms.MessageBox]::Show($message, 'rustudio installed', 'OK', 'Information')
}
