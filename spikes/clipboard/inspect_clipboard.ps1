<#
.SYNOPSIS
  Lists the formats on the Windows clipboard (name, id, size) and, for clipboard contents that
  came from this spike's Copy button, hashes the "PNG" format.

.DESCRIPTION
  Format names come from EnumClipboardFormats (P/Invoke). Sizes, the PNG hash and the
  Get-Clipboard cross-checks are only done when the custom format
  application/x-worldwright-ref is present, i.e. only for data the spike app itself put there.
  Anything else on the clipboard is listed by format name only; its data is never read.

  Output: a table on screen and, with -Json, a JSON file:
    { formats: [{id, name, size_bytes}], has_*: ..., png: {bytes, sha256}, get_clipboard: {...} }

.EXAMPLE
  pwsh -File spikes/clipboard/inspect_clipboard.ps1
  pwsh -File spikes/clipboard/inspect_clipboard.ps1 -Json out.json -PngOut clip.png
#>
param(
    [string]$Json,
    [string]$PngOut
)
$ErrorActionPreference = 'Stop'

Add-Type -Language CSharp -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class WwClip
{
    [DllImport("user32.dll", SetLastError = true)] static extern bool OpenClipboard(IntPtr hWnd);
    [DllImport("user32.dll", SetLastError = true)] static extern bool CloseClipboard();
    [DllImport("user32.dll")] static extern uint EnumClipboardFormats(uint format);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClipboardFormatName(uint format, StringBuilder name, int max);
    [DllImport("user32.dll")] static extern IntPtr GetClipboardData(uint format);
    [DllImport("user32.dll", EntryPoint = "RegisterClipboardFormatW", CharSet = CharSet.Unicode)] static extern uint RegisterClipboardFormat(string name);
    [DllImport("kernel32.dll")] static extern UIntPtr GlobalSize(IntPtr h);
    [DllImport("kernel32.dll")] static extern IntPtr GlobalLock(IntPtr h);
    [DllImport("kernel32.dll")] static extern bool GlobalUnlock(IntPtr h);

    public class Fmt { public uint Id; public string Name; public long Size = -1; }

    static readonly Dictionary<uint, string> Std = new Dictionary<uint, string> {
        {1,"CF_TEXT"},{2,"CF_BITMAP"},{3,"CF_METAFILEPICT"},{4,"CF_SYLK"},{5,"CF_DIF"},{6,"CF_TIFF"},
        {7,"CF_OEMTEXT"},{8,"CF_DIB"},{9,"CF_PALETTE"},{10,"CF_PENDATA"},{11,"CF_RIFF"},{12,"CF_WAVE"},
        {13,"CF_UNICODETEXT"},{14,"CF_ENHMETAFILE"},{15,"CF_HDROP"},{16,"CF_LOCALE"},{17,"CF_DIBV5"}};

    static bool Open()
    {
        for (int i = 0; i < 20; i++) { if (OpenClipboard(IntPtr.Zero)) return true; System.Threading.Thread.Sleep(50); }
        return false;
    }

    // names only; sizes (which render delayed formats) only when readData is true
    public static List<Fmt> List(bool readData)
    {
        var res = new List<Fmt>();
        if (!Open()) throw new Exception("OpenClipboard failed");
        try {
            uint f = 0;
            while ((f = EnumClipboardFormats(f)) != 0) {
                var x = new Fmt { Id = f };
                string n;
                if (Std.TryGetValue(f, out n)) x.Name = n;
                else { var sb = new StringBuilder(256); x.Name = GetClipboardFormatName(f, sb, 256) > 0 ? sb.ToString() : ("#" + f); }
                if (readData && f != 2 && f != 3 && f != 9 && f != 14) {
                    IntPtr h = GetClipboardData(f);
                    if (h != IntPtr.Zero) x.Size = (long)GlobalSize(h).ToUInt64();
                }
                res.Add(x);
            }
        } finally { CloseClipboard(); }
        return res;
    }

    public static byte[] GetBytes(string registeredName)
    {
        uint id = RegisterClipboardFormat(registeredName);
        if (!Open()) throw new Exception("OpenClipboard failed");
        try {
            IntPtr h = GetClipboardData(id);
            if (h == IntPtr.Zero) return null;
            long n = (long)GlobalSize(h).ToUInt64();
            IntPtr p = GlobalLock(h);
            if (p == IntPtr.Zero) return null;
            try { var b = new byte[n]; Marshal.Copy(p, b, 0, (int)n); return b; } finally { GlobalUnlock(h); }
        } finally { CloseClipboard(); }
    }
}
'@

$RefName = 'application/x-worldwright-ref'

# Pass 1: names only.
$names = [WwClip]::List($false)
$ours = $null -ne ($names | Where-Object { $_.Name -eq $RefName })

# Pass 2: sizes, only for our own data.
$formats = if ($ours) { [WwClip]::List($true) } else { $names }

$out = [ordered]@{
    ours       = $ours
    formats    = @($formats | ForEach-Object {
        [ordered]@{ id = $_.Id; name = $_.Name; size_bytes = $(if ($_.Size -ge 0) { $_.Size } else { $null }) } })
    has_png         = [bool]($formats | Where-Object { $_.Name -eq 'PNG' })
    has_dib         = [bool]($formats | Where-Object { $_.Name -in 'CF_DIB', 'CF_DIBV5' })
    has_file_drop   = [bool]($formats | Where-Object { $_.Name -in 'CF_HDROP', 'FileNameW', 'FileName' })
    has_ref         = $ours
    has_unicodetext = [bool]($formats | Where-Object { $_.Name -in 'CF_UNICODETEXT', 'CF_TEXT', 'CF_OEMTEXT' })
}

if ($ours) {
    $png = [WwClip]::GetBytes('PNG')
    if ($png) {
        $out.png = [ordered]@{
            bytes  = $png.Length
            sha256 = (Get-FileHash -InputStream ([IO.MemoryStream]::new($png)) -Algorithm SHA256).Hash.ToLower()
        }
        if ($PngOut) { [IO.File]::WriteAllBytes($PngOut, $png) }
    } else {
        $out.png = $null
    }
    # Cross-checks with Get-Clipboard -Format (our data only). PowerShell 7's Get-Clipboard has no
    # -Format parameter, so this runs in Windows PowerShell 5.1 (STA) and returns JSON.
    $ps51 = @'
$ErrorActionPreference = 'SilentlyContinue'
$drop = @(Get-Clipboard -Format FileDropList)
$text = Get-Clipboard -Format Text -Raw
$img  = Get-Clipboard -Format Image
$r = [ordered]@{
    file_drop_list = @($drop | ForEach-Object { $_.FullName })
    text_present   = [bool]$text
    image          = $(if ($img) { '{0}x{1}' -f $img.Width, $img.Height } else { $null })
}
if ($img) { $img.Dispose() }
$r | ConvertTo-Json -Compress
'@
    $gc = & powershell.exe -NoProfile -STA -Command $ps51 | ConvertFrom-Json
    $out.get_clipboard = [ordered]@{
        file_drop_list = @($gc.file_drop_list)
        text_present   = [bool]$gc.text_present
        image          = $gc.image
    }
} else {
    $out.note = 'Clipboard does not hold the spike''s own data; only format names were listed, no data was read.'
}

'{0,-6} {1,-34} {2}' -f 'Id', 'Format', 'Size (bytes)'
foreach ($f in $out.formats) { '{0,-6} {1,-34} {2}' -f $f.id, $f.name, $(if ($null -ne $f.size_bytes) { $f.size_bytes } else { '-' }) }
if ($out.png) { "PNG sha256: $($out.png.sha256)  ($($out.png.bytes) bytes)" }
"has_unicodetext: $($out.has_unicodetext)"

if ($Json) { $out | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $Json -Encoding utf8 }
